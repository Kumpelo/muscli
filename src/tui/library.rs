//! In-memory library state.
//!
//! Loads tracks and their statistics from the database, rebuilds the derived
//! indexes the views read (albums, artists, genres, smart-playlist matches),
//! and drives the background scan and ReplayGain analysis that keep them
//! current.

use super::*;

impl App {
    pub(super) fn reload_library(&mut self) -> Result<()> {
        let _profile = crate::profiling::span("reload_library");
        let library = self.db.load_library_state()?;
        self.tracks = library.tracks;
        self.stats = library.stats;
        self.added_at = library.added_at;
        let (search_index, albums, artists, genres) = if self.tracks.len() >= 2_000 {
            thread::scope(|scope| -> Result<_> {
                let tracks = &self.tracks;
                let search = scope.spawn(|| SearchIndex::build(tracks));
                let albums = scope.spawn(|| group_albums(tracks));
                let artists = scope.spawn(|| group_artists(tracks));
                let genres = scope.spawn(|| group_genres(tracks));
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
                SearchIndex::build(&self.tracks),
                group_albums(&self.tracks),
                group_artists(&self.tracks),
                group_genres(&self.tracks),
            )
        };
        self.search_index = search_index;
        self.albums = albums;
        self.artists = artists;
        self.genres = genres;
        self.refresh_search();
        self.track_index = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), i))
            .collect();
        self.favorite_indices = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| track.favorite.then_some(index))
            .collect();
        self.album_index = self
            .albums
            .iter()
            .enumerate()
            .map(|(index, album)| (album.key.clone(), index))
            .collect();
        self.artist_index = self
            .artists
            .iter()
            .enumerate()
            .map(|(index, artist)| (artist.name.clone(), index))
            .collect();
        self.rebuild_genre_indices();
        self.playlists = self.db.load_playlists()?;
        self.smart_playlists = self.db.load_smart_playlists()?;
        self.saved_queues = self.db.load_saved_queues()?;
        self.history = self.db.load_history(500)?;
        self.rebuild_home_tracks();
        self.rebuild_smart_matches();
        self.refresh_artist_releases();
        if self.view == View::AlbumDetail && self.opened_album().is_none() {
            self.view = self.album_parent_view;
            self.opened_album_key = None;
        }
        if self.view == View::ArtistDetail && self.opened_artist().is_none() {
            self.view = View::Artists;
            self.opened_artist_name = None;
            self.artist_release_keys.clear();
        }
        if self.view == View::GenreDetail
            && !self.genres.iter().any(|genre| {
                self.opened_genre_name
                    .as_deref()
                    .is_some_and(|name| genre.name == name)
            })
        {
            self.view = View::Genres;
            self.opened_genre_name = None;
        }
        self.selected = self.selected.min(self.item_count().saturating_sub(1));
        self.dirty = true;
        Ok(())
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
        if self.opened_smart_playlist.is_some_and(|id| {
            !self
                .smart_playlists
                .iter()
                .any(|playlist| playlist.id == id)
        }) {
            self.opened_smart_playlist = None;
            if self.view == View::SmartPlaylistDetail {
                self.view = View::SmartPlaylists;
            }
        }
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
                self.status = "Tema de Omarchy actualizado".into();
                self.dirty = true;
            }
        }
    }

    pub(super) fn start_scan(&mut self) {
        let roots = all_sources(&self.config);
        let tx = self.scan_tx.clone();
        let paths = self.paths.clone();
        let cover_cache_bytes = self.config.cover_cache_mb * 1024 * 1024;
        let scan_threads = self.config.scan_threads;
        self.scan_running = true;
        self.last_scan = Instant::now();
        self.status = format!("Escaneando {} fuente(s)…", roots.len());
        thread::Builder::new()
            .name("muscli-scanner".into())
            .spawn(move || {
                let mut db = match Database::open(&paths.database_file()) {
                    Ok(db) => db,
                    Err(error) => {
                        let _ = tx.send(ScanMessage::Error(format!("Base de datos: {error:#}")));
                        let _ = tx.send(ScanMessage::Done { changed: false });
                        return;
                    }
                };
                let mut ids = BTreeSet::new();
                let mut changed = false;
                for root in roots {
                    let scan = match scan_source_with_database(&paths, &root, &db, scan_threads) {
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

                match db.mark_missing_sources(&ids) {
                    Ok(count) => changed |= count > 0,
                    Err(error) => {
                        let _ = tx.send(ScanMessage::Error(format!("Fuentes: {error:#}")));
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
                self.status = format!("Indexadas {tracks} pistas de {label}");
                self.dirty = true;
            }
            ScanMessage::Error(error) => {
                self.status = format!("Scan: {error}");
                self.dirty = true;
            }
            ScanMessage::Done { changed } => {
                self.scan_running = false;
                if changed {
                    self.reload_library()?;
                    self.status = format!(
                        "{} canciones · {} álbumes · listo",
                        self.tracks.len(),
                        self.albums.len()
                    );
                } else {
                    self.status = format!(
                        "{} canciones · {} álbumes · sin cambios",
                        self.tracks.len(),
                        self.albums.len()
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
                self.status = "Análisis de volumen terminado".into();
                self.dirty = true;
            }
        }
        Ok(())
    }
}
