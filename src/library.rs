use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
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
    fsutil::atomic_replace,
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
    pub track_count: usize,
    pub failed_paths: BTreeSet<String>,
    pub missing_track_ids: Vec<String>,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// How a scan should be carried out.
#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    /// Worker threads for tag and artwork reading. `0` derives a value from the
    /// machine. More is not always better: on a spinning disk or a USB stick,
    /// too many readers spend their time seeking rather than reading.
    pub threads: usize,
    pub cover_cache_bytes: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: 0,
            cover_cache_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Below this many files, scanning stays on the calling thread: spawning costs
/// more than it saves.
const PARALLEL_THRESHOLD: usize = 64;

/// Past this, extra readers contend for the same device more than they help.
const MAX_SCAN_THREADS: usize = 8;

/// Files claimed per trip to the shared cursor.
///
/// A compromise between two opposite pressures, both measured. Claiming one at
/// a time makes the atomic cost more than the work on a rescan, where nearly
/// every file is dismissed after a single stat. Claiming many unbalances a cold
/// scan instead: neighbouring files share album art, so a large batch hands one
/// thread every expensive decode while the others idle.
const CLAIM_BATCH: usize = 4;

fn worker_count(configured: usize, work: usize) -> usize {
    if work < PARALLEL_THRESHOLD {
        return 1;
    }
    let requested = if configured == 0 {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(MAX_SCAN_THREADS)
    } else {
        configured
    };
    requested.clamp(1, work)
}

/// What the scanner decided about one file.
enum ScanOutcome {
    /// Fingerprint matched and the row is already correct; nothing to write.
    Unchanged,
    /// Fingerprint matched but the row was marked unavailable, so the file has
    /// come back (a drive was reconnected) and only needs reactivating.
    Reactivated(Box<ScannedTrack>),
    /// New or modified; tags and artwork were read.
    Updated(Box<ScannedTrack>),
    Failed {
        relative: String,
        error: String,
    },
}

/// Work shared between scan threads.
///
/// Each cache turns a repeated expensive operation into a lookup: decoding the
/// same embedded artwork once per album rather than once per track, listing a
/// directory for an external cover once, and validating a cached cover once.
/// The lock is only held around the lookup and the insert, never around the
/// decode, so threads duplicating work on a race is possible and harmless -
/// the same key always yields the same file.
#[derive(Default)]
struct ScanCaches {
    artwork: Mutex<HashMap<String, Option<PathBuf>>>,
    external_cover: Mutex<HashMap<PathBuf, Option<PathBuf>>>,
    cover_validity: Mutex<HashMap<PathBuf, bool>>,
}

impl ScanCaches {
    fn cover_is_valid(&self, cover: &Path) -> bool {
        if let Some(known) = self
            .cover_validity
            .lock()
            .expect("scan cache poisoned")
            .get(cover)
        {
            return *known;
        }
        let valid = cached_cover_is_valid(cover);
        self.cover_validity
            .lock()
            .expect("scan cache poisoned")
            .insert(cover.to_path_buf(), valid);
        valid
    }
}

pub fn scan_to_database(
    db: &mut Database,
    paths: &AppPaths,
    roots: &[PathBuf],
    options: ScanOptions,
) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    let mut ids = BTreeSet::new();
    for root in roots {
        match scan_source_with_database(paths, root, db, options.threads) {
            Ok(scan) => {
                ids.insert(scan.id.clone());
                report.sources += 1;
                report.tracks += scan.track_count;
                report.skipped += scan.skipped;
                report.errors.extend(scan.errors.clone());
                let _ = db.upsert_scan(
                    &scan.id,
                    &scan.root,
                    &scan.label,
                    &scan.tracks,
                    &scan.missing_track_ids,
                )?;
                db.prune_missing_for_source(&scan.id, &scan.failed_paths)?;
            }
            Err(error) => report.errors.push(format!("{}: {error:#}", root.display())),
        }
    }
    let _ = db.mark_missing_sources(&ids)?;
    prune_unreferenced_covers(&paths.cover_cache_dir(), &db.referenced_cover_paths()?)?;
    let removed = prune_cover_cache(&paths.cover_cache_dir(), options.cover_cache_bytes)?;
    db.clear_cover_paths(&removed)?;
    Ok(report)
}

pub fn scan_source(paths: &AppPaths, root: &Path, threads: usize) -> Result<SourceScan> {
    let database = Database::open(&paths.database_file()).ok();
    scan_source_inner(paths, root, database.as_ref(), threads)
}

pub fn scan_source_with_database(
    paths: &AppPaths,
    root: &Path,
    database: &Database,
    threads: usize,
) -> Result<SourceScan> {
    scan_source_inner(paths, root, Some(database), threads)
}

fn scan_source_inner(
    paths: &AppPaths,
    root: &Path,
    database: Option<&Database>,
    threads: usize,
) -> Result<SourceScan> {
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
    let cached = database
        .and_then(|db| db.scan_cache(&id).ok())
        .unwrap_or_default();

    // Creating the cover directory once, rather than on every cached cover.
    fs::create_dir_all(paths.cover_cache_dir()).ok();

    // Phase 1: walk the tree. Cheap, and it fixes the order everything else
    // reports in, so results stay identical run to run.
    let candidates = collect_candidates(&root);

    // Phase 2: read tags and artwork, in parallel when it is worth it.
    let caches = ScanCaches::default();
    let outcomes = read_candidates(paths, &id, &root, &candidates, &cached, &caches, threads);

    // Phase 3: fold the outcomes back together in walk order.
    let mut scan = SourceScan {
        id: id.clone(),
        root: root.clone(),
        label,
        tracks: Vec::new(),
        track_count: 0,
        failed_paths: BTreeSet::new(),
        missing_track_ids: Vec::new(),
        skipped: 0,
        errors: Vec::new(),
    };
    for outcome in outcomes {
        match outcome {
            ScanOutcome::Unchanged => scan.track_count += 1,
            ScanOutcome::Reactivated(item) | ScanOutcome::Updated(item) => {
                scan.track_count += 1;
                scan.tracks.push(*item);
            }
            ScanOutcome::Failed { relative, error } => {
                scan.failed_paths.insert(relative);
                scan.skipped += 1;
                scan.errors.push(error);
            }
        }
    }

    // Anything the database knew about that the walk did not find is gone.
    // Computed as a set difference rather than by draining `cached`, so the
    // readers above can share it immutably.
    let seen: HashSet<&str> = candidates
        .iter()
        .map(|(_, relative)| relative.as_str())
        .collect();
    scan.missing_track_ids = cached
        .iter()
        .filter(|(relative, _)| !seen.contains(relative.as_str()))
        .map(|(_, item)| item.track.id.clone())
        .collect();

    Ok(scan)
}

fn collect_candidates(root: &Path) -> Vec<(PathBuf, String)> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("flac"))
        })
        .map(|entry| {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .into_owned();
            (entry.path().to_path_buf(), relative)
        })
        .collect()
}

fn read_candidates(
    paths: &AppPaths,
    source_id: &str,
    root: &Path,
    candidates: &[(PathBuf, String)],
    cached: &HashMap<String, ScannedTrack>,
    caches: &ScanCaches,
    threads: usize,
) -> Vec<ScanOutcome> {
    let workers = worker_count(threads, candidates.len());
    if workers <= 1 {
        return candidates
            .iter()
            .map(|(path, relative)| {
                read_candidate(paths, source_id, root, path, relative, cached, caches)
            })
            .collect();
    }

    // A shared cursor rather than fixed slices: per-file cost varies enormously
    // (a track with 2 MB of embedded art against one with none), so static
    // partitioning would leave threads idle.
    //
    // Claims are batched because the cheap case dominates the common one. On a
    // rescan almost every file is dismissed by its fingerprint after one stat,
    // and taking the cursor once per file made the atomic traffic cost more
    // than the work itself - four threads came out slower than one. A small
    // batch amortises that away while still balancing the expensive files.
    let cursor = AtomicUsize::new(0);
    let mut collected: Vec<Vec<(usize, ScanOutcome)>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let start = cursor.fetch_add(CLAIM_BATCH, Ordering::Relaxed);
                        if start >= candidates.len() {
                            break;
                        }
                        let end = (start + CLAIM_BATCH).min(candidates.len());
                        for (offset, (path, relative)) in candidates[start..end].iter().enumerate()
                        {
                            local.push((
                                start + offset,
                                read_candidate(
                                    paths, source_id, root, path, relative, cached, caches,
                                ),
                            ));
                        }
                    }
                    local
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect()
    });

    // Reassemble in walk order so the result does not depend on thread timing.
    let mut ordered: Vec<Option<ScanOutcome>> = (0..candidates.len()).map(|_| None).collect();
    for (index, outcome) in collected.drain(..).flatten() {
        ordered[index] = Some(outcome);
    }
    ordered.into_iter().flatten().collect()
}

fn read_candidate(
    paths: &AppPaths,
    source_id: &str,
    root: &Path,
    path: &Path,
    relative: &str,
    cached: &HashMap<String, ScannedTrack>,
    caches: &ScanCaches,
) -> ScanOutcome {
    let fingerprint = file_fingerprint(path);
    let unchanged = fingerprint.and_then(|(size, modified)| {
        cached.get(relative).filter(|item| {
            item.file_size == size
                && item.modified_ns == modified
                && item
                    .track
                    .cover_path
                    .as_deref()
                    .is_none_or(|cover| caches.cover_is_valid(cover))
        })
    });
    if let Some(item) = unchanged {
        if item.track.available {
            return ScanOutcome::Unchanged;
        }
        // The file is back after the row was marked unavailable; refresh the
        // absolute path, which changes when a drive is remounted elsewhere.
        let mut item = item.clone();
        item.track.path = path.to_path_buf();
        item.track.available = true;
        return ScanOutcome::Reactivated(Box::new(item));
    }

    match read_track(paths, source_id, root, path, fingerprint, caches) {
        Ok(track) => ScanOutcome::Updated(Box::new(track)),
        Err(error) => ScanOutcome::Failed {
            relative: relative.to_owned(),
            error: format!("{}: {error:#}", path.display()),
        },
    }
}

fn read_track(
    paths: &AppPaths,
    source_id: &str,
    root: &Path,
    path: &Path,
    fingerprint: Option<(u64, i64)>,
    caches: &ScanCaches,
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
    let cover_path = find_or_cache_cover(paths, tag, path, caches)?;
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
    caches: &ScanCaches,
) -> Result<Option<PathBuf>> {
    if let Some(picture) = tag.and_then(|tag| tag.pictures().first())
        && let Some(cached) = cache_cover_data(paths, picture.data(), caches)?
    {
        return Ok(Some(cached));
    }

    let Some(dir) = track_path.parent() else {
        return Ok(None);
    };
    if let Some(cached) = caches
        .external_cover
        .lock()
        .expect("scan cache poisoned")
        .get(dir)
    {
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
        if let Some(cached) = cache_cover_data(paths, &data, caches)? {
            found = Some(cached);
            break;
        }
    }
    caches
        .external_cover
        .lock()
        .expect("scan cache poisoned")
        .insert(dir.to_path_buf(), found.clone());
    Ok(found)
}

fn cover_cache_key(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

fn cache_cover_data(paths: &AppPaths, data: &[u8], caches: &ScanCaches) -> Result<Option<PathBuf>> {
    let key = cover_cache_key(data);
    if let Some(cached) = caches
        .artwork
        .lock()
        .expect("scan cache poisoned")
        .get(&key)
    {
        return Ok(cached.clone());
    }

    // The lock is deliberately not held across decoding and re-encoding, which
    // is the expensive part. Two threads racing on the same key both do the
    // work and both write the same bytes to the same place, which is cheaper
    // than serialising every cover behind one mutex.
    let cached = if let Some(existing) = existing_cover(paths, &key) {
        Some(existing)
    } else if let Ok(image) = image::load_from_memory(data) {
        Some(cache_cover(paths, &key, image)?)
    } else {
        None
    };
    caches
        .artwork
        .lock()
        .expect("scan cache poisoned")
        .insert(key, cached.clone());
    Ok(cached)
}

/// Distinguishes concurrent writes of the same cover.
///
/// Covers are keyed by artwork content, so two tracks sharing art race for the
/// same cache entry. A shared scratch name would have them interleave writes
/// into one file and produce a truncated image; the process id and counter make
/// each attempt write somewhere of its own before the atomic rename.
fn scratch_name(key: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        ".{key}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Quality for cached thumbnails. Album art is photographic, so lossless
/// storage buys nothing visible at 512 px while costing roughly ten times the
/// bytes - and the cover cache has a fixed budget it has to live inside.
const COVER_QUALITY: u8 = 85;

/// Where a cover with this key is written from now on.
fn cover_target(paths: &AppPaths, key: &str) -> PathBuf {
    paths.cover_cache_dir().join(format!("{key}.jpg"))
}

/// An already-cached cover for this key, in either format.
///
/// Caches written by older versions hold PNGs. They decode perfectly well, so
/// there is nothing to migrate: they stay valid and the byte-budget pass
/// retires them as new art arrives.
fn existing_cover(paths: &AppPaths, key: &str) -> Option<PathBuf> {
    [
        cover_target(paths, key),
        paths.cover_cache_dir().join(format!("{key}.png")),
    ]
    .into_iter()
    .find(|candidate| cached_cover_is_valid(candidate))
}

fn cache_cover(paths: &AppPaths, key: &str, image: image::DynamicImage) -> Result<PathBuf> {
    fs::create_dir_all(paths.cover_cache_dir())?;
    if let Some(existing) = existing_cover(paths, key) {
        return Ok(existing);
    }
    let target = cover_target(paths, key);
    // JPEG has no alpha channel; flattening to RGB is required, not incidental.
    let thumbnail = image.thumbnail(512, 512).to_rgb8();
    let temporary = paths.cover_cache_dir().join(scratch_name(key));
    {
        let mut file = std::io::BufWriter::new(fs::File::create(&temporary)?);
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, COVER_QUALITY);
        encoder.encode_image(&thumbnail)?;
    }
    atomic_replace(&temporary, &target)?;
    Ok(target)
}

/// A scratch file a cover write has not committed yet.
fn is_scratch(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
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
            if !meta.is_file() || is_scratch(&entry.path()) {
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
        // Dot-prefixed entries are in-flight scratch files from cache_cover;
        // they are not referenced yet and deleting one corrupts a live write.
        if is_scratch(&path) {
            continue;
        }
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
