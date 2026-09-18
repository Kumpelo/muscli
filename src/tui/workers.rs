//! Background workers.
//!
//! Everything that must not block the render loop runs on its own thread and
//! reports back over a channel: filesystem watching and removable-media
//! discovery, fuzzy search, cover decoding, terminal event reading and shutdown
//! signals. The types here are the messages those threads send.

use super::*;

pub(super) enum ScanMessage {
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
}

pub(super) struct CoverDecodeResult {
    pub(super) path: PathBuf,
    pub(super) size: u32,
    pub(super) image: Option<image::DynamicImage>,
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

pub(super) fn start_watchers(config: &Config, tx: tokio_mpsc::UnboundedSender<()>) {
    let configured = config.sources.clone();
    thread::Builder::new()
        .name("muscli-watcher".into())
        .spawn(move || {
            let event_tx = tx.clone();
            let Ok(mut watcher) = RecommendedWatcher::new(
                move |result: notify::Result<notify::Event>| {
                    if result.is_ok() {
                        let _ = event_tx.send(());
                    }
                },
                notify::Config::default(),
            ) else {
                return;
            };
            for root in configured.iter().filter(|p| p.exists()) {
                let _ = watcher.watch(root, RecursiveMode::Recursive);
            }

            let mut known_removable = crate::config::discover_removable_roots()
                .into_iter()
                .filter(|path| path.exists())
                .collect::<BTreeSet<_>>();
            let mut watched_removable = BTreeSet::new();
            loop {
                let current_removable = crate::config::discover_removable_roots()
                    .into_iter()
                    .filter(|path| path.exists())
                    .collect::<BTreeSet<_>>();

                if current_removable != known_removable {
                    let _ = tx.send(());
                    known_removable = current_removable.clone();
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

                thread::sleep(Duration::from_secs(2));
            }
        })
        .ok();
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

pub(super) fn start_cover_decode_worker(
    requests: Receiver<CoverDecodeRequest>,
    results: tokio_mpsc::UnboundedSender<CoverDecodeResult>,
) {
    thread::Builder::new()
        .name("muscli-cover-decode".into())
        .spawn(move || {
            while let Ok(request) = requests.recv() {
                let image = image::ImageReader::open(&request.path)
                    .ok()
                    .and_then(|reader| reader.decode().ok())
                    .map(|image| image.thumbnail(request.size, request.size));
                if results
                    .send(CoverDecodeResult {
                        path: request.path,
                        size: request.size,
                        image,
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
