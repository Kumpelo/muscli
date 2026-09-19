//! In-memory library state.
//!
//! Loads tracks and their statistics from the database, rebuilds the derived
//! indexes the views read (albums, artists, genres, smart-playlist matches),
//! and drives the background scan and ReplayGain analysis that keep them
//! current.

use super::*;
use crate::library::scan_source_reporting;
use crate::t;

/// How many files between progress messages.
const PROGRESS_STEP: usize = 250;

/// Sources waiting to be rescanned.
///
/// A file change only ever affects one source, so tracking which one avoids
/// walking every drive because one tag was edited. "Everything" is kept as a
/// distinct state because it is not the same request: only a full pass may
/// decide that a source has gone missing.
#[derive(Debug, Default)]
pub(super) struct PendingScan {
    roots: BTreeSet<PathBuf>,
    everything: bool,
}

impl PendingScan {
    pub(super) fn record(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Source(root) => {
                self.roots.insert(root);
            }
            WatchEvent::SourcesChanged => self.everything = true,
        }
    }

    pub(super) fn record_full_rescan(&mut self) {
        self.everything = true;
    }

    pub(super) fn is_empty(&self) -> bool {
        !self.everything && self.roots.is_empty()
    }

    pub(super) fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}

/// Everything the views read, as a pure function of the database.
///
/// Built away from `App` so it can be assembled on a worker thread: a rescan of
/// a large library otherwise froze the render loop while every track was read
/// and regrouped.
pub(super) struct LibrarySnapshot {
    tracks: Vec<Track>,
    stats: HashMap<String, TrackStats>,
    added_at: HashMap<String, i64>,
    track_index: HashMap<String, usize>,
    favorite_indices: Vec<usize>,
    albums: Vec<Album>,
    album_index: HashMap<String, usize>,
    artists: Vec<Artist>,
    artist_index: HashMap<String, usize>,
    genres: Vec<Genre>,
    search_index: SearchIndex,
    playlists: Vec<Playlist>,
    smart_playlists: Vec<SmartPlaylist>,
    saved_queues: Vec<SavedQueue>,
    history: Vec<HistoryEntry>,
}

impl LibrarySnapshot {
    pub(super) fn load(db: &Database) -> Result<Self> {
        let _profile = crate::profiling::span("library_snapshot");
        let library = db.load_library_state()?;
        let tracks = library.tracks;

        // The four derived indexes are independent, so on a big library they
        // are worth building side by side; below that the threads cost more
        // than they save.
        let (search_index, albums, artists, genres) = if tracks.len() >= 2_000 {
            thread::scope(|scope| -> Result<_> {
                let borrowed = &tracks;
                let search = scope.spawn(|| SearchIndex::build(borrowed));
                let albums = scope.spawn(|| group_albums(borrowed));
                let artists = scope.spawn(|| group_artists(borrowed));
                let genres = scope.spawn(|| group_genres(borrowed));
                Ok((
                    search
                        .join()
                        .map_err(|_| anyhow::anyhow!("search index worker panicked"))?,
                    albums
                        .join()
                        .map_err(|_| anyhow::anyhow!("album grouping worker panicked"))?,
                    artists
                        .join()
                        .map_err(|_| anyhow::anyhow!("artist grouping worker panicked"))?,
                    genres
                        .join()
                        .map_err(|_| anyhow::anyhow!("genre grouping worker panicked"))?,
                ))
            })?
        } else {
            (
                SearchIndex::build(&tracks),
                group_albums(&tracks),
                group_artists(&tracks),
                group_genres(&tracks),
            )
        };

        Ok(Self {
            track_index: tracks
                .iter()
                .enumerate()
                .map(|(index, track)| (track.id.clone(), index))
                .collect(),
            favorite_indices: tracks
                .iter()
                .enumerate()
                .filter_map(|(index, track)| track.favorite.then_some(index))
                .collect(),
            album_index: albums
                .iter()
                .enumerate()
                .map(|(index, album)| (album.key.clone(), index))
                .collect(),
            artist_index: artists
                .iter()
                .enumerate()
                .map(|(index, artist)| (artist.name.clone(), index))
                .collect(),
            tracks,
            stats: library.stats,
            added_at: library.added_at,
            albums,
            artists,
            genres,
            search_index,
            playlists: db.load_playlists()?,
            smart_playlists: db.load_smart_playlists()?,
            saved_queues: db.load_saved_queues()?,
            history: db.load_history(500)?,
        })
    }
}

impl App {
    /// Adopt a freshly built snapshot.
    ///
    /// Only the parts that depend on live application state - the search query,
    /// the open detail views, the cursor - are recomputed here; everything else
    /// was prepared off-thread.
    pub(super) fn install_snapshot(&mut self, snapshot: LibrarySnapshot) {
        self.tracks = snapshot.tracks;
        self.stats = snapshot.stats;
        self.added_at = snapshot.added_at;
        self.track_index = snapshot.track_index;
        self.favorite_indices = snapshot.favorite_indices;
        self.albums = snapshot.albums;
        self.album_index = snapshot.album_index;
        self.artists = snapshot.artists;
        self.artist_index = snapshot.artist_index;
        self.genres = snapshot.genres;
        self.search_index = snapshot.search_index;
        self.playlists = snapshot.playlists;
        self.smart_playlists = snapshot.smart_playlists;
        self.saved_queues = snapshot.saved_queues;
        self.history = snapshot.history;

        self.refresh_search();
        self.rebuild_genre_indices();
        self.rebuild_home_tracks();
        self.rebuild_smart_matches();
        // A rescan can delete whatever is currently open; unwind to the
        // deepest level that still exists.
        self.prune_nav();
        self.selected = self.selected.min(self.item_count().saturating_sub(1));
        self.reload_running = false;
        self.dirty = true;
    }

    /// Load the library on this thread.
    ///
    /// Used for the first load, where the restored session needs the tracks
    /// before the UI can draw anything meaningful.
    pub(super) fn reload_library(&mut self) -> Result<()> {
        let snapshot = LibrarySnapshot::load(&self.db)?;
        self.install_snapshot(snapshot);
        Ok(())
    }

    /// Ask the worker to rebuild the library in the background.
    ///
    /// Requests while one is already running are collapsed into a single
    /// follow-up, so a burst of scans does not queue a burst of reloads.
    pub(super) fn request_reload(&mut self) {
        if self.reload_running {
            self.reload_again = true;
            return;
        }
        self.reload_running = true;
        if self.reload_tx.send(()).is_err() {
            // The worker is gone; fall back to loading here rather than
            // leaving the library stale.
            self.reload_running = false;
            let _ = self.reload_library();
        }
    }

    pub(super) fn handle_reload_result(
        &mut self,
        result: Result<Box<LibrarySnapshot>, String>,
    ) {
        self.reload_running = false;
        match result {
            Ok(snapshot) => {
                self.install_snapshot(*snapshot);
                self.status = t!(
                    "status.library_ready",
                    tracks = self.tracks.len(),
                    albums = self.albums.len()
                );
            }
            Err(error) => {
                self.status = t!("status.database_error", error = error);
                self.dirty = true;
            }
        }
        if std::mem::take(&mut self.reload_again) {
            self.request_reload();
        }
    }

    pub(super) fn rebuild_smart_matches(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.smart_matches = self
            .smart_playlists
            .iter()
            .map(|playlist| {
                (
                    playlist.id,
                    evaluate_smart_playlist(
                        playlist,
                        &self.tracks,
                        &self.search_index,
                        &self.stats,
                        &self.added_at,
                        now,
                    ),
                )
            })
            .collect();
    }

    pub(super) fn rebuild_smart_matches_for_field(&mut self, field: &str) {
        let now = chrono::Utc::now().timestamp();
        for playlist in &self.smart_playlists {
            if playlist.rules.iter().any(|rule| rule.field == field) {
                self.smart_matches.insert(
                    playlist.id,
                    evaluate_smart_playlist(
                        playlist,
                        &self.tracks,
                        &self.search_index,
                        &self.stats,
                        &self.added_at,
                        now,
                    ),
                );
            }
        }
    }

    pub(super) fn set_favorite_local(&mut self, track_id: &str, favorite: bool) {
        let Some(index) = self.track_index.get(track_id).copied() else {
            return;
        };
        self.tracks[index].favorite = favorite;
        match self.favorite_indices.binary_search(&index) {
            Ok(position) if !favorite => {
                self.favorite_indices.remove(position);
            }
            Err(position) if favorite => {
                self.favorite_indices.insert(position, index);
            }
            _ => {}
        }
        self.rebuild_smart_matches_for_field("favorite");
        self.selected = self.selected.min(self.item_count().saturating_sub(1));
        self.dirty = true;
    }

    pub(super) fn refresh_playlists(&mut self) -> Result<()> {
        self.playlists = self.db.load_playlists()?;
        self.selected = self.selected.min(self.item_count().saturating_sub(1));
        self.dirty = true;
        Ok(())
    }

    pub(super) fn refresh_smart_playlists(&mut self) -> Result<()> {
        self.smart_playlists = self.db.load_smart_playlists()?;
        self.rebuild_smart_matches();
        self.prune_nav();
        self.selected = self.selected.min(self.item_count().saturating_sub(1));
        self.dirty = true;
        Ok(())
    }

    pub(super) fn refresh_theme(&mut self) {
        if self.last_theme_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_theme_check = Instant::now();

        #[cfg(unix)]
        {
            let modified = self
                .theme_path
                .as_ref()
                .and_then(|path| fs::metadata(path).ok())
                .and_then(|metadata| metadata.modified().ok());
            if self.theme_path.is_some() && modified == self.theme_modified {
                return;
            }

            let (theme, path, modified) = UiTheme::load_with_source();
            self.theme_path = path;
            self.theme_modified = modified;
            if theme != self.theme {
                self.theme = theme;
                self.status = t!("status.theme_updated").into();
                self.dirty = true;
            }
        }
    }

    /// Rescan every configured and discovered source.
    pub(super) fn start_scan(&mut self) {
        let roots = all_sources(&self.config);
        self.spawn_scan(roots, true);
    }

    /// Act on what the watcher reported.
    pub(super) fn start_pending_scan(&mut self, pending: PendingScan) {
        if pending.everything {
            self.start_scan();
            return;
        }
        // Only look at the sources that actually changed, and keep the roots
        // that are still configured: a source removed from the config while the
        // event was in flight should not come back.
        let known = all_sources(&self.config);
        let roots: Vec<PathBuf> = pending
            .roots
            .into_iter()
            .filter(|root| known.contains(root))
            .collect();
        if roots.is_empty() {
            return;
        }
        self.spawn_scan(roots, false);
    }

    /// `full` says whether this pass covers every source. It must, before any
    /// source can be declared missing: a partial scan that ran
    /// mark_missing_sources would mark every drive it did not visit as
    /// unplugged and take the whole library offline.
    fn spawn_scan(&mut self, roots: Vec<PathBuf>, full: bool) {
        let tx = self.scan_tx.clone();
        let paths = self.paths.clone();
        let cover_cache_bytes = self.config.cover_cache_mb * 1024 * 1024;
        let options = self.config.scan_options();
        self.scan_running = true;
        self.last_scan = Instant::now();
        self.status = t!("status.scanning_sources", count = roots.len());
        thread::Builder::new()
            .name("muscli-scanner".into())
            .spawn(move || {
                let mut db = match Database::open(&paths.database_file()) {
                    Ok(db) => db,
                    Err(error) => {
                        let _ = tx.send(ScanMessage::Error(t!(
                            "status.database_error",
                            error = format!("{error:#}")
                        )));
                        let _ = tx.send(ScanMessage::Done { changed: false });
                        return;
                    }
                };
                let mut ids = BTreeSet::new();
                let mut changed = false;
                for root in roots {
                    let label = root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Music")
                        .to_owned();
                    let progress_tx = tx.clone();
                    let report = move |done: usize, total: usize| {
                        // Only worth a message every so often; a per-file
                        // update would flood the channel and the status line.
                        if done == total || done.is_multiple_of(PROGRESS_STEP) {
                            let _ = progress_tx.send(ScanMessage::Progress {
                                label: label.clone(),
                                done,
                                total,
                            });
                        }
                    };
                    let scan = match scan_source_reporting(&paths, &root, &db, &options, &report) {
                        Ok(scan) => scan,
                        Err(error) => {
                            let _ = tx
                                .send(ScanMessage::Error(format!("{}: {error:#}", root.display())));
                            continue;
                        }
                    };
                    ids.insert(scan.id.clone());
                    changed |= !scan.tracks.is_empty() || !scan.missing_track_ids.is_empty();
                    let moved_tracks = match db.upsert_scan(
                        &scan.id,
                        &scan.root,
                        &scan.label,
                        &scan.tracks,
                        &scan.missing_track_ids,
                    ) {
                        Ok(moved) => moved,
                        Err(error) => {
                            let _ =
                                tx.send(ScanMessage::Error(format!("{}: {error:#}", scan.label)));
                            continue;
                        }
                    };
                    match db.prune_missing_for_source(&scan.id, &scan.failed_paths) {
                        Ok(count) => changed |= count > 0,
                        Err(error) => {
                            let _ =
                                tx.send(ScanMessage::Error(format!("{}: {error:#}", scan.label)));
                        }
                    }
                    if tx
                        .send(ScanMessage::Source {
                            label: scan.label,
                            tracks: scan.track_count,
                            moved_tracks,
                        })
                        .is_err()
                    {
                        return;
                    }
                }

                if full {
                    match db.mark_missing_sources(&ids) {
                        Ok(count) => changed |= count > 0,
                        Err(error) => {
                            let _ = tx.send(ScanMessage::Error(t!(
                                "status.sources_error",
                                error = format!("{error:#}")
                            )));
                        }
                    }
                }
                match db.referenced_cover_paths() {
                    Ok(referenced) => {
                        if let Err(error) =
                            prune_unreferenced_covers(&paths.cover_cache_dir(), &referenced)
                        {
                            let _ = tx
                                .send(ScanMessage::Error(format!("Caché de portadas: {error:#}")));
                        }
                    }
                    Err(error) => {
                        let _ =
                            tx.send(ScanMessage::Error(format!("Caché de portadas: {error:#}")));
                    }
                }
                match prune_cover_cache(&paths.cover_cache_dir(), cover_cache_bytes) {
                    Ok(removed) => match db.clear_cover_paths(&removed) {
                        Ok(count) => changed |= count > 0,
                        Err(error) => {
                            let _ = tx
                                .send(ScanMessage::Error(format!("Caché de portadas: {error:#}")));
                        }
                    },
                    Err(error) => {
                        let _ =
                            tx.send(ScanMessage::Error(format!("Caché de portadas: {error:#}")));
                    }
                }
                let _ = tx.send(ScanMessage::Done { changed });
            })
            .ok();
    }

    pub(super) fn handle_scan(&mut self, message: ScanMessage) -> Result<()> {
        match message {
            ScanMessage::Progress { label, done, total } => {
                self.status = t!(
                    "status.scanning_progress",
                    label = label,
                    done = done,
                    total = total
                );
                self.dirty = true;
            }
            ScanMessage::Source {
                label,
                tracks,
                moved_tracks,
            } => {
                let mut queue_changed = false;
                for id in &mut self.queue {
                    if let Some((_, new_id)) = moved_tracks.iter().find(|(old_id, _)| id == old_id)
                    {
                        *id = new_id.clone();
                        queue_changed = true;
                    }
                }
                self.queue_dirty |= queue_changed;
                self.status = t!("status.indexed", tracks = tracks, label = label);
                self.dirty = true;
            }
            ScanMessage::Error(error) => {
                self.status = t!("status.scan_error", error = error);
                self.dirty = true;
            }
            ScanMessage::Done { changed } => {
                self.scan_running = false;
                if changed {
                    // Rebuilt off-thread; the summary lands with the snapshot.
                    self.status = t!("status.updating_library").into();
                    self.request_reload();
                    self.dirty = true;
                } else {
                    self.status = t!(
                        "status.library_unchanged",
                        tracks = self.tracks.len(),
                        albums = self.albums.len()
                    );
                    self.dirty = true;
                }
            }
        }
        Ok(())
    }

    pub(super) fn start_gain_analysis(&mut self) -> Result<()> {
        if !self.config.replaygain_enabled || self.gain_running {
            return Ok(());
        }
        let candidates = self.db.gain_analysis_candidates(false)?;
        if candidates.is_empty() {
            return Ok(());
        }
        let total = candidates.len();
        self.gain_running = true;
        replaygain::start(
            candidates,
            self.config.replaygain_target_lufs,
            self.gain_tx.clone(),
        );
        self.gain_progress = Some((0, total));
        Ok(())
    }

    pub(super) fn handle_gain_message(&mut self, message: GainMessage) -> Result<()> {
        match message {
            GainMessage::Result {
                track_id,
                result,
                size,
                modified,
                completed,
                total,
            } => {
                self.db.save_gain(&track_id, result, size, modified)?;
                self.gain_progress = Some((completed, total));
                self.status = format!("Analizando volumen {completed}/{total}");
                self.dirty = true;
            }
            GainMessage::Error(error) => {
                self.status = format!("ReplayGain: {error}");
                self.dirty = true;
            }
            GainMessage::Done => {
                self.gain_running = false;
                self.gain_progress = None;
                self.status = t!("status.gain_finished").into();
                self.dirty = true;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn nothing_is_pending_to_begin_with() {
        assert!(PendingScan::default().is_empty());
    }

    #[test]
    fn file_events_collect_the_sources_they_touched() {
        let mut pending = PendingScan::default();
        pending.record(WatchEvent::Source(root("/music")));
        pending.record(WatchEvent::Source(root("/media/usb")));
        // Repeats of the same source must not queue a second pass.
        pending.record(WatchEvent::Source(root("/music")));

        assert!(!pending.is_empty());
        let taken = pending.take();
        assert_eq!(taken.roots.len(), 2);
        assert!(!taken.everything, "a file change is not a full rescan");
        assert!(pending.is_empty(), "taking leaves nothing behind");
    }

    #[test]
    fn a_drive_appearing_escalates_to_a_full_pass() {
        // Which sources exist may have changed, and only a full pass is allowed
        // to conclude that one has gone missing.
        let mut pending = PendingScan::default();
        pending.record(WatchEvent::Source(root("/music")));
        pending.record(WatchEvent::SourcesChanged);
        assert!(pending.take().everything);
    }

    #[test]
    fn an_explicit_rescan_is_always_full() {
        let mut pending = PendingScan::default();
        pending.record_full_rescan();
        assert!(pending.take().everything);
    }
}
