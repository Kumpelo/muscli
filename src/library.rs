use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[cfg(unix)]
use std::process::Command;

use anyhow::{Context, Result};
use lofty::{
    file::{AudioFile, TaggedFileExt},
    prelude::ItemKey,
    probe::Probe,
};
use walkdir::WalkDir;

use crate::{
    db::{Database, ScannedTrack},
    model::{Track, normalize_text, parse_slash_number},
    paths::AppPaths,
};

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub sources: usize,
    pub tracks: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SourceScan {
    pub id: String,
    pub root: PathBuf,
    pub label: String,
    pub tracks: Vec<ScannedTrack>,
    pub failed_paths: BTreeSet<String>,
    pub skipped: usize,
    pub errors: Vec<String>,
}

pub fn scan_to_database(
    db: &mut Database,
    paths: &AppPaths,
    roots: &[PathBuf],
    cover_cache_bytes: u64,
) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    let mut ids = BTreeSet::new();
    for root in roots {
        match scan_source(paths, root) {
            Ok(scan) => {
                ids.insert(scan.id.clone());
                report.sources += 1;
                report.tracks += scan.tracks.len();
                report.skipped += scan.skipped;
                report.errors.extend(scan.errors.clone());
                let _ = db.upsert_scan(&scan.id, &scan.root, &scan.label, &scan.tracks)?;
                db.prune_missing_for_source(&scan.id, &scan.failed_paths)?;
            }
            Err(error) => report.errors.push(format!("{}: {error:#}", root.display())),
        }
    }
    db.mark_missing_sources(&ids)?;
    prune_unreferenced_covers(&paths.cover_cache_dir(), &db.referenced_cover_paths()?)?;
    let removed = prune_cover_cache(&paths.cover_cache_dir(), cover_cache_bytes)?;
    db.clear_cover_paths(&removed)?;
    Ok(report)
}

pub fn scan_source(paths: &AppPaths, root: &Path) -> Result<SourceScan> {
    let _profile = crate::profiling::span("scan_source");
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot access {}", root.display()))?;
    let label = root
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("Music")
        .to_owned();
    let id = source_id(&root);
    let cached = Database::open(&paths.database_file())
        .and_then(|db| db.scan_cache(&id))
        .unwrap_or_default();
    let mut cover_validity = HashMap::<PathBuf, bool>::new();
    let mut artwork_cache = HashMap::<String, Option<PathBuf>>::new();
    let mut external_cover_cache = HashMap::<PathBuf, Option<PathBuf>>::new();
    let mut scan = SourceScan {
        id: id.clone(),
        root: root.clone(),
        label,
        tracks: Vec::new(),
        failed_paths: BTreeSet::new(),
        skipped: 0,
        errors: Vec::new(),
    };

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file()
            || !entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("flac"))
        {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();
        let fingerprint = file_fingerprint(entry.path());
        let unchanged = fingerprint
            .and_then(|(size, modified)| {
                cached
                    .get(&relative)
                    .filter(|item| {
                        item.file_size == size
                            && item.modified_ns == modified
                            && item.track.cover_path.as_deref().is_none_or(|cover| {
                                *cover_validity
                                    .entry(cover.to_path_buf())
                                    .or_insert_with(|| cached_cover_is_valid(cover))
                            })
                    })
                    .cloned()
            })
            .map(|mut item| {
                item.track.path = entry.path().to_path_buf();
                item.track.available = true;
                item
            });
        let result = unchanged.map(Ok).unwrap_or_else(|| {
            read_track(
                paths,
                &id,
                &root,
                entry.path(),
                fingerprint,
                &mut artwork_cache,
                &mut external_cover_cache,
            )
        });
        match result {
            Ok(track) => scan.tracks.push(track),
            Err(error) => {
                scan.failed_paths.insert(relative);
                scan.skipped += 1;
                scan.errors
                    .push(format!("{}: {error:#}", entry.path().display()));
            }
        }
    }
    Ok(scan)
}

fn read_track(
    paths: &AppPaths,
    source_id: &str,
    root: &Path,
    path: &Path,
    fingerprint: Option<(u64, i64)>,
    artwork_cache: &mut HashMap<String, Option<PathBuf>>,
    external_cover_cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Result<ScannedTrack> {
    let (file_size, modified_ns) = match fingerprint {
        Some(value) => value,
        None => {
            let metadata = fs::metadata(path)?;
            let modified_ns = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
                .unwrap_or(0);
            (metadata.len(), modified_ns)
        }
    };
    let tagged = Probe::open(path)?.guess_file_type()?.read()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown Track");
    let parent_album = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown Album");
    let parent_artist = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("Unknown Artist");
    let value = |key: ItemKey| tag.and_then(|t| t.get_string(key)).map(normalize_text);

    let title = value(ItemKey::TrackTitle)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| strip_track_prefix(stem));
    let artist = value(ItemKey::TrackArtist)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| parent_artist.to_owned());
    let album = value(ItemKey::AlbumTitle)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| parent_album.to_owned());
    let album_artist = value(ItemKey::AlbumArtist)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| artist.clone());
    let genre = value(ItemKey::Genre).unwrap_or_default();
    let year = value(ItemKey::RecordingDate)
        .or_else(|| value(ItemKey::Year))
        .and_then(|v| v.get(0..4).and_then(|v| v.parse().ok()));
    let disc_number = parse_slash_number(value(ItemKey::DiscNumber).as_deref());
    let track_number = parse_slash_number(value(ItemKey::TrackNumber).as_deref());
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let id = blake3::hash(format!("{source_id}\0{relative}").as_bytes())
        .to_hex()
        .to_string();
    let cover_path = find_or_cache_cover(paths, tag, path, artwork_cache, external_cover_cache)?;
    let duration_ms = tagged
        .properties()
        .duration()
        .as_millis()
        .min(u64::MAX as u128) as u64;

    Ok(ScannedTrack {
        track: Track {
            id,
            source_id: source_id.to_owned(),
            relative_path: relative,
            path: path.to_path_buf(),
            title,
            artist,
            album_artist,
            album,
            genre,
            year,
            disc_number,
            track_number,
            duration_ms,
            cover_path,
            available: true,
            favorite: false,
        },
        file_size,
        modified_ns,
    })
}

fn find_or_cache_cover(
    paths: &AppPaths,
    tag: Option<&lofty::tag::Tag>,
    track_path: &Path,
    artwork_cache: &mut HashMap<String, Option<PathBuf>>,
    external_cover_cache: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Result<Option<PathBuf>> {
    if let Some(picture) = tag.and_then(|tag| tag.pictures().first())
        && let Some(cached) = cache_cover_data(paths, picture.data(), artwork_cache)?
    {
        return Ok(Some(cached));
    }

    let Some(dir) = track_path.parent() else {
        return Ok(None);
    };
    if let Some(cached) = external_cover_cache.get(dir) {
        return Ok(cached.clone());
    }

    let mut found = None;
    for name in [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "folder.jpg",
        "folder.png",
        "Cover.jpg",
        "Folder.jpg",
    ] {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            continue;
        }
        let Ok(data) = fs::read(&candidate) else {
            continue;
        };
        if let Some(cached) = cache_cover_data(paths, &data, artwork_cache)? {
            found = Some(cached);
            break;
        }
    }
    external_cover_cache.insert(dir.to_path_buf(), found.clone());
    Ok(found)
}

fn cover_cache_key(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

fn cache_cover_data(
    paths: &AppPaths,
    data: &[u8],
    artwork_cache: &mut HashMap<String, Option<PathBuf>>,
) -> Result<Option<PathBuf>> {
    let key = cover_cache_key(data);
    if let Some(cached) = artwork_cache.get(&key) {
        return Ok(cached.clone());
    }

    let target = paths.cover_cache_dir().join(format!("{key}.png"));
    let cached = if cached_cover_is_valid(&target) {
        Some(target)
    } else if let Ok(image) = image::load_from_memory(data) {
        Some(cache_cover(paths, &key, image)?)
    } else {
        None
    };
    artwork_cache.insert(key, cached.clone());
    Ok(cached)
}

fn cache_cover(paths: &AppPaths, key: &str, image: image::DynamicImage) -> Result<PathBuf> {
    fs::create_dir_all(paths.cover_cache_dir())?;
    let target = paths.cover_cache_dir().join(format!("{key}.png"));
    if !cached_cover_is_valid(&target) {
        let thumbnail = image.thumbnail(512, 512);
        let temporary = paths.cover_cache_dir().join(format!(".{key}.tmp"));
        thumbnail.save_with_format(&temporary, image::ImageFormat::Png)?;
        if target.exists() {
            fs::remove_file(&target)?;
        }
        fs::rename(temporary, &target)?;
    }
    Ok(target)
}

fn cached_cover_is_valid(path: &Path) -> bool {
    path.is_file()
        && image::image_dimensions(path).is_ok_and(|(width, height)| width <= 512 && height <= 512)
}

#[cfg(unix)]
fn source_id(root: &Path) -> String {
    let mount_field = |field: &str| {
        Command::new("findmnt")
            .args(["-n", "-o", field, "--target"])
            .arg(root)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let uuid = mount_field("UUID");
    let mount_target = mount_field("TARGET").map(PathBuf::from);
    let identity = source_identity(uuid.as_deref(), mount_target.as_deref(), root);
    blake3::hash(identity.as_bytes()).to_hex().to_string()
}

#[cfg(windows)]
fn source_id(root: &Path) -> String {
    use windows::{
        Win32::Storage::FileSystem::{GetVolumeNameForVolumeMountPointW, GetVolumePathNameW},
        core::PCWSTR,
    };

    let wide: Vec<u16> = root
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut mount = vec![0u16; 1024];
    let volume = unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut mount) }
        .ok()
        .and_then(|_| {
            let mount_len = mount.iter().position(|value| *value == 0)?;
            let mount_path = PathBuf::from(String::from_utf16_lossy(&mount[..mount_len]));
            let mut name = vec![0u16; 1024];
            unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(mount.as_ptr()), &mut name) }.ok()?;
            let name_len = name.iter().position(|value| *value == 0)?;
            Some((String::from_utf16_lossy(&name[..name_len]), mount_path))
        });
    let identity = match volume {
        Some((volume, mount)) => source_identity(Some(&volume), Some(&mount), root),
        None => source_identity(None, None, root),
    };
    blake3::hash(identity.as_bytes()).to_hex().to_string()
}

fn source_identity(uuid: Option<&str>, mount_target: Option<&Path>, root: &Path) -> String {
    match uuid {
        Some(uuid) => {
            let within_volume = mount_target
                .and_then(|mount| root.strip_prefix(mount).ok())
                .unwrap_or(root);
            format!("uuid:{uuid}\0{}", within_volume.to_string_lossy())
        }
        None => format!("path:{}", root.to_string_lossy()),
    }
}

fn file_fingerprint(path: &Path) -> Option<(u64, i64)> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((
        metadata.len(),
        modified.as_nanos().min(i64::MAX as u128) as i64,
    ))
}

fn strip_track_prefix(stem: &str) -> String {
    let trimmed = stem.trim_start_matches(|c: char| {
        c.is_ascii_digit() || c == ' ' || c == '-' || c == '.' || c == '_'
    });
    if trimmed.is_empty() {
        stem.to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn prune_cover_cache(dir: &Path, max_bytes: u64) -> Result<Vec<PathBuf>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let last_used = meta
                .accessed()
                .or_else(|_| meta.modified())
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some((entry.path(), meta.len(), last_used))
        })
        .collect::<Vec<_>>();
    let mut total = files.iter().map(|(_, len, _)| len).sum::<u64>();
    let mut removed = Vec::new();
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, len, _) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
            removed.push(path);
        }
    }
    Ok(removed)
}

pub fn prune_unreferenced_covers(
    dir: &Path,
    referenced: &BTreeSet<PathBuf>,
) -> Result<Vec<PathBuf>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && !referenced.contains(&path) && fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(name: &str) -> AppPaths {
        let root = std::env::temp_dir().join(format!(
            "muscli-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        AppPaths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            runtime_dir: root.join("runtime"),
        }
    }

    #[test]
    fn filename_fallback_removes_number() {
        assert_eq!(strip_track_prefix("01 - Hello"), "Hello");
        assert_eq!(strip_track_prefix("Song"), "Song");
    }

    #[test]
    fn volume_identity_survives_a_different_mount_path() {
        let first = source_identity(
            Some("ABCD-1234"),
            Some(Path::new("/run/media/me/MUSIC")),
            Path::new("/run/media/me/MUSIC/FLAC"),
        );
        let second = source_identity(
            Some("ABCD-1234"),
            Some(Path::new("/media/me/RENAMED")),
            Path::new("/media/me/RENAMED/FLAC"),
        );
        assert_eq!(first, second);
    }

    #[test]
    fn separate_folders_on_one_volume_have_separate_identities() {
        let root = Path::new("/run/media/me/MUSIC");
        assert_ne!(
            source_identity(Some("ABCD-1234"), Some(root), &root.join("Jazz")),
            source_identity(Some("ABCD-1234"), Some(root), &root.join("Rock")),
        );
    }

    #[test]
    fn cover_cache_key_tracks_artwork_content() {
        assert_eq!(
            cover_cache_key(b"same artwork"),
            cover_cache_key(b"same artwork")
        );
        assert_ne!(
            cover_cache_key(b"first artwork"),
            cover_cache_key(b"second artwork")
        );
    }

    #[test]
    fn cached_covers_are_limited_to_512_pixels() -> Result<()> {
        let paths = test_paths("cover-size");
        paths.ensure()?;
        let image = image::DynamicImage::new_rgb8(1400, 1000);
        let cached = cache_cover(&paths, "album", image)?;
        assert_eq!(image::image_dimensions(cached)?, (512, 366));
        fs::remove_dir_all(paths.config_dir.parent().unwrap())?;
        Ok(())
    }

    #[test]
    fn pruning_reports_every_evicted_cover() -> Result<()> {
        let paths = test_paths("cover-prune");
        paths.ensure()?;
        let cover = paths.cover_cache_dir().join("large.bin");
        fs::write(&cover, vec![0u8; 32])?;
        let removed = prune_cover_cache(&paths.cover_cache_dir(), 0)?;
        assert_eq!(removed, [cover]);
        fs::remove_dir_all(paths.config_dir.parent().unwrap())?;
        Ok(())
    }

    #[test]
    fn pruning_removes_only_unreferenced_covers() -> Result<()> {
        let paths = test_paths("cover-unreferenced");
        paths.ensure()?;
        let keep = paths.cover_cache_dir().join("keep.png");
        let remove = paths.cover_cache_dir().join("remove.jpg");
        fs::write(&keep, [1])?;
        fs::write(&remove, [2])?;
        let referenced = BTreeSet::from([keep.clone()]);
        assert_eq!(
            prune_unreferenced_covers(&paths.cover_cache_dir(), &referenced)?,
            [remove]
        );
        assert!(keep.exists());
        fs::remove_dir_all(paths.config_dir.parent().unwrap())?;
        Ok(())
    }
}
