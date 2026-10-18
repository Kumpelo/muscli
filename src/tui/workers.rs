//! Background workers.
//!
//! Everything that must not block the render loop runs on its own thread and
//! reports back over a channel: filesystem watching and removable-media
//! discovery, fuzzy search, cover decoding, terminal event reading and shutdown
//! signals. The types here are the messages those threads send.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use notify::EventKind;

use super::*;

pub(super) enum ScanMessage {
    /// How far through the current source the scan is.
    Progress {
        label: String,
        done: usize,
        total: usize,
    },
    Source {
        label: String,
        tracks: usize,
        moved_tracks: Vec<(String, String)>,
    },
    Error(String),
    Done {
        changed: bool,
    },
}

#[derive(Debug)]
pub(super) struct CoverDecodeRequest {
    pub(super) path: PathBuf,
    pub(super) size: u32,
    pub(super) blend_from: Option<PathBuf>,
}

pub(super) struct CoverDecodeResult {
    pub(super) path: PathBuf,
    pub(super) size: u32,
    pub(super) image: Option<image::DynamicImage>,
    pub(super) blend_frames: Vec<image::DynamicImage>,
}

#[derive(Debug)]
pub(super) struct SearchRequest {
    pub(super) generation: u64,
    pub(super) query: String,
    pub(super) index: SearchIndex,
}

#[derive(Debug)]
pub(super) struct SearchResult {
    pub(super) generation: u64,
    pub(super) matches: Vec<usize>,
}

/// What a watcher event asks the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WatchEvent {
    /// This source changed on disk.
    Source(PathBuf),
    /// The set of removable drives changed, so which sources exist at all may
    /// have changed and every one needs looking at.
    SourcesChanged,
}

/// How often the removable-drive poll runs.
///
/// On Unix the mount parents are watched directly, so this is only a safety
/// net for mounts that appear without an inotify event; the poll is the primary
/// mechanism only on Windows.
const REMOVABLE_POLL: Duration = Duration::from_secs(5);

fn watch_event_may_affect_library(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(
                notify::event::ModifyKind::Data(_)
                    | notify::event::ModifyKind::Name(_)
                    | notify::event::ModifyKind::Any
                    | notify::event::ModifyKind::Other
            )
    )
}

fn watch_path_may_affect_library(kind: &EventKind, path: &Path, extensions: &[String]) -> bool {
    if crate::library::has_extension(path, extensions) || path.is_dir() {
        return true;
    }
    if path.is_file() {
        return false;
    }

    // Removed/renamed paths often no longer have metadata. Only discard the
    // event when the backend explicitly says it was a non-audio file; unknown
    // kinds may be directories (including names such as Album.2024).
    !matches!(
        kind,
        EventKind::Create(notify::event::CreateKind::File)
            | EventKind::Remove(notify::event::RemoveKind::File)
    )
}

pub(super) fn start_watchers(config: &Config, tx: tokio_mpsc::UnboundedSender<WatchEvent>) {
    let configured = config.sources.clone();
    let watch_config = config.clone();
    let extensions = config.scan_options().extensions;
    thread::Builder::new()
        .name("muscli-watcher".into())
        .spawn(move || {
            // Shared with the event callback so it can attribute a changed path
            // to the source it belongs to.
            let roots: Arc<Mutex<BTreeSet<PathBuf>>> =
                Arc::new(Mutex::new(configured.iter().cloned().collect()));
            let callback_roots = Arc::clone(&roots);
            let event_tx = tx.clone();
            let Ok(mut watcher) = RecommendedWatcher::new(
                move |result: notify::Result<notify::Event>| {
                    let Ok(event) = result else { return };
                    // Access times and metadata touches are not library
                    // changes; reacting to them meant a rescan of every source
                    // whenever anything read a file.
                    if !watch_event_may_affect_library(&event.kind) {
                        return;
                    }
                    let Ok(roots) = callback_roots.lock() else {
                        return;
                    };
                    for path in &event.paths {
                        // A directory event matters too: renaming or deleting a
                        // folder changes the library without touching a file.
                        if !watch_path_may_affect_library(&event.kind, path, &extensions) {
                            continue;
                        }
                        if let Some(root) = roots
                            .iter()
                            .find(|root| path.starts_with(root))
                            .or_else(|| roots.iter().find(|root| root.starts_with(path)))
                        {
                            let _ = event_tx.send(WatchEvent::Source(root.clone()));
                        }
                    }
                },
                notify::Config::default(),
            ) else {
                return;
            };
            for root in configured.iter().filter(|p| p.exists()) {
                let _ = watcher.watch(root, RecursiveMode::Recursive);
            }
            // Watching the directories drives mount into means a hot-plug is
            // noticed immediately rather than on the next poll.
            for parent in removable_parents() {
                let _ = watcher.watch(&parent, RecursiveMode::NonRecursive);
            }

            // Observe only active music sources. A Ventoy data partition may
            // contain many ISOs; watching and recursively scanning its root is
            // unnecessary when a conventional music directory is the intended source.
            let discover_music = || -> BTreeSet<PathBuf> {
                if !watch_config.auto_discover_removable {
                    return BTreeSet::new();
                }
                crate::config::sources_for_mounts(
                    &watch_config,
                    &crate::config::discover_removable_roots(),
                )
                .into_iter()
                .filter(|root| !configured.contains(root))
                .collect()
            };
            let mut known_removable = discover_music();
            let mut watched_removable = BTreeSet::new();
            loop {
                let current_removable = discover_music();

                if current_removable != known_removable {
                    let _ = tx.send(WatchEvent::SourcesChanged);
                    known_removable = current_removable.clone();
                }

                if let Ok(mut shared) = roots.lock() {
                    shared.clear();
                    shared.extend(configured.iter().cloned());
                    shared.extend(current_removable.iter().cloned());
                }

                let added = current_removable
                    .difference(&watched_removable)
                    .cloned()
                    .collect::<Vec<_>>();
                for root in added {
                    if watcher.watch(&root, RecursiveMode::Recursive).is_ok() {
                        watched_removable.insert(root);
                    }
                }

                let removed = watched_removable
                    .difference(&current_removable)
                    .cloned()
                    .collect::<Vec<_>>();
                for root in removed {
                    let _ = watcher.unwatch(&root);
                    watched_removable.remove(&root);
                }

                thread::sleep(REMOVABLE_POLL);
            }
        })
        .ok();
}

/// Directories that removable drives are mounted into.
#[cfg(unix)]
fn removable_parents() -> Vec<PathBuf> {
    let user = std::env::var("USER").unwrap_or_default();
    [PathBuf::from("/run/media"), PathBuf::from("/media")]
        .into_iter()
        .map(|base| base.join(&user))
        .filter(|path| path.is_dir())
        .collect()
}

/// Windows surfaces drives by letter rather than by mount directory, so there
/// is nothing to watch and the poll carries this alone.
#[cfg(windows)]
fn removable_parents() -> Vec<PathBuf> {
    Vec::new()
}

pub(super) fn start_search_worker(
    requests: Receiver<SearchRequest>,
    results: tokio_mpsc::UnboundedSender<SearchResult>,
) {
    thread::Builder::new()
        .name("muscli-search".into())
        .spawn(move || {
            while let Ok(mut request) = requests.recv() {
                for newer in requests.try_iter() {
                    request = newer;
                }
                let matches = request.index.search(&request.query, 100);
                if results
                    .send(SearchResult {
                        generation: request.generation,
                        matches,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .ok();
}

/// Five bounded frames, built on the cover worker rather than the draw loop.
fn blend_cover_frames(
    old: &image::DynamicImage,
    new: &image::DynamicImage,
) -> Vec<image::DynamicImage> {
    let old = old
        .resize_to_fill(256, 256, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let new = new
        .resize_to_fill(256, 256, image::imageops::FilterType::Triangle)
        .to_rgba8();
    (1..=5u16)
        .map(|step| {
            let image = image::RgbaImage::from_fn(256, 256, |x, y| {
                let a = old.get_pixel(x, y).0;
                let b = new.get_pixel(x, y).0;
                image::Rgba(std::array::from_fn(|channel| {
                    ((u16::from(a[channel]) * (5 - step) + u16::from(b[channel]) * step) / 5) as u8
                }))
            });
            image::DynamicImage::ImageRgba8(image)
        })
        .collect()
}

pub(super) fn start_cover_decode_worker(
    requests: Receiver<CoverDecodeRequest>,
    results: tokio_mpsc::UnboundedSender<CoverDecodeResult>,
) {
    thread::Builder::new()
        .name("muscli-cover-decode".into())
        .spawn(move || {
            while let Ok(request) = requests.recv() {
                let image = crate::cover_security::decode_file(&request.path)
                    .map(|image| image.thumbnail(request.size, request.size));
                let blend_frames = request
                    .blend_from
                    .as_ref()
                    .and_then(|path| crate::cover_security::decode_file(path))
                    .zip(image.as_ref())
                    .map_or_else(Vec::new, |(old, new)| blend_cover_frames(&old, new));
                if results
                    .send(CoverDecodeResult {
                        path: request.path,
                        size: request.size,
                        image,
                        blend_frames,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .ok();
}

pub(super) fn start_terminal_event_reader() -> tokio_mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = tokio_mpsc::unbounded_channel();
    thread::Builder::new()
        .name("muscli-terminal-events".into())
        .spawn(move || {
            while let Ok(event) = event::read() {
                if tx.send(event).is_err() {
                    break;
                }
            }
        })
        .ok();
    rx
}

#[cfg(unix)]
pub(super) fn start_shutdown_listener() -> Result<tokio_mpsc::UnboundedReceiver<()>> {
    let (shutdown_tx, shutdown_rx) = tokio_mpsc::unbounded_channel();
    for kind in [
        tokio::signal::unix::SignalKind::interrupt(),
        tokio::signal::unix::SignalKind::terminate(),
        tokio::signal::unix::SignalKind::hangup(),
    ] {
        let mut signal = tokio::signal::unix::signal(kind)?;
        let tx = shutdown_tx.clone();
        tokio::task::spawn_local(async move {
            let _ = signal.recv().await;
            let _ = tx.send(());
        });
    }
    Ok(shutdown_rx)
}

#[cfg(windows)]
pub(super) fn start_shutdown_listener() -> Result<tokio_mpsc::UnboundedReceiver<()>> {
    let (shutdown_tx, shutdown_rx) = tokio_mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(());
    });
    Ok(shutdown_rx)
}

/// Rebuilds the library snapshot away from the render loop.
///
/// Keeps one database connection open for its lifetime rather than reopening
/// per request, and collapses a backlog into a single rebuild: what matters is
/// the latest state, not how many times it was asked for.
pub(super) fn start_library_worker(
    database_file: PathBuf,
    requests: Receiver<()>,
    results: tokio_mpsc::UnboundedSender<std::result::Result<Box<LibrarySnapshot>, String>>,
) {
    thread::Builder::new()
        .name("muscli-library".into())
        .spawn(move || {
            let mut db = Database::open(&database_file).map_err(|error| format!("{error:#}"));

            while requests.recv().is_ok() {
                while requests.try_recv().is_ok() {}

                if db.is_err() {
                    db = Database::open(&database_file).map_err(|error| format!("{error:#}"));
                }

                let result = match &db {
                    Ok(db) => LibrarySnapshot::load(db)
                        .map(Box::new)
                        .map_err(|error| format!("{error:#}")),
                    Err(error) => Err(error.clone()),
                };

                if results.send(result).is_err() {
                    break;
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    #[test]
    fn cover_blending_is_bounded_and_finishes_on_the_incoming_image() {
        let old = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([0, 0, 0, 255]),
        ));
        let new = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([200, 100, 50, 255]),
        ));
        let frames = super::blend_cover_frames(&old, &new);
        assert_eq!(frames.len(), 5);
        assert_eq!(frames[0].to_rgba8().get_pixel(0, 0).0, [40, 20, 10, 255]);
        assert_eq!(frames[4].to_rgba8().get_pixel(0, 0).0, [200, 100, 50, 255]);
        assert_eq!((frames[0].width(), frames[0].height()), (256, 256));
    }

    use super::*;

    #[test]
    fn configured_extensions_are_watched() {
        let extensions = vec!["flac".to_owned(), "mp4".to_owned()];
        assert!(watch_path_may_affect_library(
            &EventKind::Create(notify::event::CreateKind::File),
            Path::new("/music/new.mp4"),
            &extensions,
        ));
        assert!(watch_path_may_affect_library(
            &EventKind::Remove(notify::event::RemoveKind::File),
            Path::new("/music/gone.mp4"),
            &extensions,
        ));
    }

    #[test]
    fn dotted_removed_directories_are_not_filtered_as_files() {
        let extensions = vec!["flac".to_owned()];
        assert!(watch_path_may_affect_library(
            &EventKind::Remove(notify::event::RemoveKind::Folder),
            Path::new("/music/Artist/Album.2024"),
            &extensions,
        ));
        assert!(!watch_path_may_affect_library(
            &EventKind::Remove(notify::event::RemoveKind::File),
            Path::new("/music/notes.txt"),
            &extensions,
        ));
    }
}

#[cfg(test)]
mod library_event_tests {
    use super::*;

    #[test]
    fn playback_reads_and_timestamp_changes_do_not_trigger_scans() {
        use notify::event::{AccessKind, AccessMode, DataChange, MetadataKind, ModifyKind};
        for kind in [
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Open(AccessMode::Read)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
        ] {
            assert!(!watch_event_may_affect_library(&kind));
        }
        assert!(watch_event_may_affect_library(&EventKind::Modify(
            ModifyKind::Data(DataChange::Content)
        )));
    }
}
