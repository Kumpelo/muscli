use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{
    Album, Artist, HistoryEntry, Playlist, ReplayGainAnalysis, SavedPlayback, SavedQueue,
    SmartMatch, SmartPlaylist, Track, TrackStats,
};

pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone, Copy)]
pub struct HistoryUpdate<'a> {
    pub history_id: i64,
    pub track_id: &'a str,
    pub listened_delta_ms: u64,
    pub position_ms: u64,
    pub was_counted: bool,
    pub count_now: bool,
    pub completed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryHealth {
    pub tracks: usize,
    pub unavailable_tracks: usize,
    pub missing_tracks_on_available_sources: usize,
    pub dangling_covers: usize,
    pub sources: Vec<(PathBuf, bool)>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("could not open database {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "cache_size", -16_384i64)?;
        conn.pragma_update(None, "mmap_size", 134_217_728i64)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 1 && path.exists() {
            let stamp = chrono::Local::now().format("%Y%m%d%H%M%S");
            let backup = path.with_extension(format!("db.backup.{stamp}"));
            conn.execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])?;
        }
        let mut db = Self { conn };
        db.migrate()?;
        db.seed_smart_playlists()?;
        db.conn.execute_batch("PRAGMA optimize;")?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(Duration::from_secs(2))?;
        let mut db = Self { conn };
        db.migrate()?;
        db.seed_smart_playlists()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 4 {
            anyhow::bail!("library database is newer than this muscli build");
        }
        if version == 0 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE sources (
                    id TEXT PRIMARY KEY,
                    root TEXT NOT NULL,
                    label TEXT NOT NULL DEFAULT '',
                    available INTEGER NOT NULL DEFAULT 1,
                    last_scan INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE tracks (
                    id TEXT PRIMARY KEY,
                    source_id TEXT NOT NULL REFERENCES sources(id),
                    relative_path TEXT NOT NULL,
                    path TEXT NOT NULL,
                    title TEXT NOT NULL,
                    artist TEXT NOT NULL,
                    album_artist TEXT NOT NULL,
                    album TEXT NOT NULL,
                    genre TEXT NOT NULL DEFAULT '',
                    year INTEGER,
                    disc_number INTEGER NOT NULL DEFAULT 0,
                    track_number INTEGER NOT NULL DEFAULT 0,
                    duration_ms INTEGER NOT NULL DEFAULT 0,
                    cover_path TEXT,
                    file_size INTEGER NOT NULL DEFAULT 0,
                    modified_ns INTEGER NOT NULL DEFAULT 0,
                    added_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    gain_db REAL,
                    true_peak_db REAL,
                    gain_file_size INTEGER,
                    gain_modified_ns INTEGER,
                    available INTEGER NOT NULL DEFAULT 1,
                    favorite INTEGER NOT NULL DEFAULT 0,
                    UNIQUE(source_id, relative_path)
                );
                CREATE INDEX tracks_album ON tracks(album_artist, album, disc_number, track_number);
                CREATE INDEX tracks_artist ON tracks(artist);
                CREATE INDEX tracks_available ON tracks(available);
                CREATE TABLE playlists (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                    created_at INTEGER NOT NULL DEFAULT (unixepoch())
                );
                CREATE TABLE playlist_tracks (
                    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                    PRIMARY KEY(playlist_id, position)
                );
                CREATE TABLE app_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE track_stats (
                    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                    play_count INTEGER NOT NULL DEFAULT 0,
                    total_listen_ms INTEGER NOT NULL DEFAULT 0,
                    last_played_at INTEGER,
                    resume_position_ms INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                    started_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    listened_ms INTEGER NOT NULL DEFAULT 0,
                    position_ms INTEGER NOT NULL DEFAULT 0,
                    counted INTEGER NOT NULL DEFAULT 0,
                    completed INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX history_recent ON history(started_at DESC);
                CREATE TABLE smart_playlists (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                    match_mode TEXT NOT NULL,
                    rules_json TEXT NOT NULL,
                    sort_field TEXT NOT NULL DEFAULT 'title',
                    descending INTEGER NOT NULL DEFAULT 0,
                    item_limit INTEGER
                );
                CREATE TABLE saved_queues (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                );
                CREATE TABLE saved_queue_tracks (
                    queue_id INTEGER NOT NULL REFERENCES saved_queues(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                    PRIMARY KEY(queue_id, position)
                );
                PRAGMA user_version = 2;",
            )?;
            tx.commit()?;
        }
        if version == 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "ALTER TABLE tracks ADD COLUMN added_at INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE tracks ADD COLUMN gain_db REAL;
                 ALTER TABLE tracks ADD COLUMN true_peak_db REAL;
                 ALTER TABLE tracks ADD COLUMN gain_file_size INTEGER;
                 ALTER TABLE tracks ADD COLUMN gain_modified_ns INTEGER;
                 UPDATE tracks SET added_at=unixepoch() WHERE added_at=0;
                 CREATE TABLE track_stats (
                    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                    play_count INTEGER NOT NULL DEFAULT 0,
                    total_listen_ms INTEGER NOT NULL DEFAULT 0,
                    last_played_at INTEGER,
                    resume_position_ms INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                    started_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    listened_ms INTEGER NOT NULL DEFAULT 0,
                    position_ms INTEGER NOT NULL DEFAULT 0,
                    counted INTEGER NOT NULL DEFAULT 0,
                    completed INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX history_recent ON history(started_at DESC);
                 CREATE TABLE smart_playlists (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                    match_mode TEXT NOT NULL,
                    rules_json TEXT NOT NULL,
                    sort_field TEXT NOT NULL DEFAULT 'title',
                    descending INTEGER NOT NULL DEFAULT 0,
                    item_limit INTEGER
                 );
                 CREATE TABLE saved_queues (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                 );
                 CREATE TABLE saved_queue_tracks (
                    queue_id INTEGER NOT NULL REFERENCES saved_queues(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                    PRIMARY KEY(queue_id, position)
                 );
                 PRAGMA user_version = 2;",
            )?;
            tx.commit()?;
        }

        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 2 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS tracks_source_scan
                 ON tracks(source_id, available, file_size, duration_ms);
                 PRAGMA user_version = 3;",
            )?;
            tx.commit()?;
        }

        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 3 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS tracks_library_order
                 ON tracks(
                    album_artist COLLATE NOCASE,
                    album COLLATE NOCASE,
                    disc_number,
                    track_number,
                    title COLLATE NOCASE
                 );
                 PRAGMA user_version = 4;",
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    fn seed_smart_playlists(&self) -> Result<()> {
        let presets = [
            (
                "Metal favorito",
                "all",
                r#"[{"field":"genre","operator":"contains","value":"metal"},{"field":"favorite","operator":"is","value":true}]"#,
                "title",
                false,
                None,
            ),
            (
                "Agregadas recientemente",
                "all",
                r#"[{"field":"added_days","operator":"lte","value":30}]"#,
                "added_at",
                true,
                None,
            ),
            (
                "No escuchadas",
                "all",
                r#"[{"field":"played","operator":"is","value":false}]"#,
                "title",
                false,
                None,
            ),
            (
                "Más reproducidas",
                "all",
                r#"[{"field":"play_count","operator":"gte","value":1}]"#,
                "play_count",
                true,
                Some(100),
            ),
            (
                "Más de 8 minutos",
                "all",
                r#"[{"field":"duration_ms","operator":"gte","value":480000}]"#,
                "duration",
                true,
                None,
            ),
        ];
        for (name, mode, rules, sort, descending, limit) in presets {
            self.conn.execute(
                "INSERT OR IGNORE INTO smart_playlists(name,match_mode,rules_json,sort_field,descending,item_limit) VALUES(?1,?2,?3,?4,?5,?6)",
                params![name, mode, rules, sort, descending, limit],
            )?;
        }
        Ok(())
    }

    pub fn upsert_scan(
        &mut self,
        source_id: &str,
        root: &Path,
        label: &str,
        tracks: &[ScannedTrack],
    ) -> Result<Vec<(String, String)>> {
        let _profile = crate::profiling::span("db_upsert_scan");
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO sources(id, root, label, available, last_scan) VALUES(?1, ?2, ?3, 1, unixepoch())
             ON CONFLICT(id) DO UPDATE SET root=excluded.root, label=excluded.label, available=1, last_scan=excluded.last_scan",
            params![source_id, root.to_string_lossy(), label],
        )?;
        tx.execute(
            "UPDATE tracks SET available=0 WHERE source_id=?1",
            [source_id],
        )?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO tracks(
                    id, source_id, relative_path, path, title, artist, album_artist, album, genre,
                    year, disc_number, track_number, duration_ms, cover_path, file_size, modified_ns, available
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,1)
                 ON CONFLICT(id) DO UPDATE SET
                    path=excluded.path, title=excluded.title, artist=excluded.artist,
                    album_artist=excluded.album_artist, album=excluded.album, genre=excluded.genre,
                    year=excluded.year, disc_number=excluded.disc_number, track_number=excluded.track_number,
                    duration_ms=excluded.duration_ms, cover_path=excluded.cover_path,
                    file_size=excluded.file_size, modified_ns=excluded.modified_ns, available=1",
            )?;
            for item in tracks {
                stmt.execute(params![
                    item.track.id,
                    item.track.source_id,
                    item.track.relative_path,
                    item.track.path.to_string_lossy(),
                    item.track.title,
                    item.track.artist,
                    item.track.album_artist,
                    item.track.album,
                    item.track.genre,
                    item.track.year,
                    item.track.disc_number,
                    item.track.track_number,
                    item.track.duration_ms.min(i64::MAX as u64) as i64,
                    item.track
                        .cover_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned()),
                    item.file_size.min(i64::MAX as u64) as i64,
                    item.modified_ns,
                ])?;
            }
        }
        let moved_tracks = {
            let mut stmt = tx.prepare(
                "SELECT old.id, MIN(new.id)
                 FROM tracks old
                 JOIN tracks new ON new.source_id=old.source_id
                    AND new.available=1
                    AND new.id<>old.id
                    AND new.file_size=old.file_size
                    AND new.duration_ms=old.duration_ms
                    AND new.title=old.title COLLATE NOCASE
                    AND new.artist=old.artist COLLATE NOCASE
                    AND new.album=old.album COLLATE NOCASE
                    AND new.disc_number=old.disc_number
                    AND new.track_number=old.track_number
                 WHERE old.source_id=?1 AND old.available=0
                    AND old.file_size>0 AND old.duration_ms>0
                 GROUP BY old.id
                 HAVING COUNT(new.id)=1",
            )?;
            stmt.query_map([source_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<(String, String)>>>()?
        };
        for (old_id, new_id) in &moved_tracks {
            tx.execute(
                "UPDATE tracks
                 SET favorite = favorite OR COALESCE((SELECT favorite FROM tracks WHERE id=?1), 0)
                 WHERE id=?2",
                params![old_id, new_id],
            )?;
            tx.execute(
                "UPDATE playlist_tracks SET track_id=?2 WHERE track_id=?1",
                params![old_id, new_id],
            )?;
            tx.execute(
                "UPDATE app_state SET value=replace(value, ?1, ?2) WHERE key='playback'",
                params![old_id, new_id],
            )?;
            tx.execute(
                "INSERT INTO track_stats(track_id,play_count,total_listen_ms,last_played_at,resume_position_ms)
                 SELECT ?2,play_count,total_listen_ms,last_played_at,resume_position_ms FROM track_stats WHERE track_id=?1
                 ON CONFLICT(track_id) DO UPDATE SET
                    play_count=play_count+excluded.play_count,
                    total_listen_ms=total_listen_ms+excluded.total_listen_ms,
                    last_played_at=MAX(last_played_at,excluded.last_played_at),
                    resume_position_ms=MAX(resume_position_ms,excluded.resume_position_ms)",
                params![old_id, new_id],
            )?;
            tx.execute("DELETE FROM track_stats WHERE track_id=?1", [old_id])?;
            tx.execute(
                "UPDATE history SET track_id=?2 WHERE track_id=?1",
                params![old_id, new_id],
            )?;
            tx.execute(
                "UPDATE saved_queue_tracks SET track_id=?2 WHERE track_id=?1",
                params![old_id, new_id],
            )?;
            tx.execute("DELETE FROM tracks WHERE id=?1", [old_id])?;
        }
        tx.commit()?;
        Ok(moved_tracks)
    }

    pub fn mark_missing_sources(&self, available_ids: &BTreeSet<String>) -> Result<()> {
        let mut stmt = self.conn.prepare("SELECT id FROM sources")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids {
            if !available_ids.contains(&id) {
                self.conn
                    .execute("UPDATE sources SET available=0 WHERE id=?1", [&id])?;
                self.conn
                    .execute("UPDATE tracks SET available=0 WHERE source_id=?1", [&id])?;
            }
        }
        Ok(())
    }

    pub fn prune_missing_for_source(
        &mut self,
        source_id: &str,
        seen_paths: &BTreeSet<String>,
    ) -> Result<usize> {
        let _profile = crate::profiling::span("db_prune_missing_for_source");
        let stale = {
            let mut stmt = self.conn.prepare(
                "SELECT id,relative_path FROM tracks WHERE source_id=?1 AND available=0",
            )?;
            stmt.query_map([source_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|(_, relative)| !seen_paths.contains(relative))
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
        };
        let tx = self.conn.transaction()?;
        {
            let mut mark_seen = tx.prepare_cached(
                "UPDATE tracks SET available=1 WHERE source_id=?1 AND relative_path=?2",
            )?;
            for relative in seen_paths {
                mark_seen.execute(params![source_id, relative])?;
            }
        }
        {
            let mut delete_stale = tx.prepare_cached("DELETE FROM tracks WHERE id=?1")?;
            for id in &stale {
                delete_stale.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(stale.len())
    }

    pub fn clear_cover_paths(&mut self, paths: &[PathBuf]) -> Result<usize> {
        if paths.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut changed = 0;
        for path in paths {
            changed += tx.execute(
                "UPDATE tracks SET cover_path=NULL WHERE cover_path=?1",
                [path.to_string_lossy().as_ref()],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn clear_dangling_cover_paths(&mut self) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT cover_path FROM tracks WHERE cover_path IS NOT NULL")?;
        let dangling = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|result| result.ok())
            .map(PathBuf::from)
            .filter(|path| !path.is_file())
            .collect::<Vec<_>>();
        drop(stmt);
        self.clear_cover_paths(&dangling)
    }

    pub fn referenced_cover_paths(&self) -> Result<BTreeSet<PathBuf>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT cover_path FROM tracks WHERE cover_path IS NOT NULL")?;
        Ok(stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    pub fn prune_missing_tracks(&mut self) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT tracks.id,tracks.path,sources.root,sources.available
             FROM tracks JOIN sources ON sources.id=tracks.source_id",
        )?;
        let stale = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                    PathBuf::from(row.get::<_, String>(2)?),
                    row.get::<_, bool>(3)?,
                ))
            })?
            .filter_map(|row| row.ok())
            .filter(|(_, track, root, available)| *available && root.is_dir() && !track.is_file())
            .map(|(id, _, _, _)| id)
            .collect::<Vec<_>>();
        drop(stmt);
        let tx = self.conn.transaction()?;
        for id in &stale {
            tx.execute("DELETE FROM tracks WHERE id=?1", [id])?;
        }
        tx.commit()?;
        Ok(stale.len())
    }

    pub fn library_health(&self) -> Result<LibraryHealth> {
        let tracks = self
            .conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |row| {
                row.get::<_, i64>(0)
            })?
            .max(0) as usize;
        let unavailable_tracks = self
            .conn
            .query_row("SELECT COUNT(*) FROM tracks WHERE available=0", [], |row| {
                row.get::<_, i64>(0)
            })?
            .max(0) as usize;
        let mut source_stmt = self
            .conn
            .prepare("SELECT root,available FROM sources ORDER BY root")?;
        let sources = source_stmt
            .query_map([], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, bool>(1)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut missing_tracks_on_available_sources = 0;
        let mut dangling = BTreeSet::new();
        let mut track_stmt = self.conn.prepare(
            "SELECT tracks.path,tracks.cover_path,sources.root,sources.available
             FROM tracks JOIN sources ON sources.id=tracks.source_id",
        )?;
        for row in track_stmt.query_map([], |row| {
            Ok((
                PathBuf::from(row.get::<_, String>(0)?),
                row.get::<_, Option<String>>(1)?.map(PathBuf::from),
                PathBuf::from(row.get::<_, String>(2)?),
                row.get::<_, bool>(3)?,
            ))
        })? {
            let (track, cover, root, available) = row?;
            if available && root.is_dir() && !track.is_file() {
                missing_tracks_on_available_sources += 1;
            }
            if let Some(cover) = cover
                && !cover.is_file()
            {
                dangling.insert(cover);
            }
        }
        Ok(LibraryHealth {
            tracks,
            unavailable_tracks,
            missing_tracks_on_available_sources,
            dangling_covers: dangling.len(),
            sources,
        })
    }

    pub fn load_tracks(&self) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, relative_path, path, title, artist, album_artist, album, genre,
                    year, disc_number, track_number, duration_ms, cover_path, available, favorite
             FROM tracks
             ORDER BY album_artist COLLATE NOCASE, album COLLATE NOCASE, disc_number, track_number, title COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], row_to_track)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn scan_cache(&self, source_id: &str) -> Result<HashMap<String, ScannedTrack>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, relative_path, path, title, artist, album_artist, album, genre,
                    year, disc_number, track_number, duration_ms, cover_path, available, favorite,
                    file_size, modified_ns
             FROM tracks WHERE source_id=?1",
        )?;
        let rows = stmt.query_map([source_id], |row| {
            let track = row_to_track(row)?;
            Ok((
                track.relative_path.clone(),
                ScannedTrack {
                    track,
                    file_size: row.get::<_, i64>(16)?.max(0) as u64,
                    modified_ns: row.get(17)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }

    pub fn toggle_favorite(&self, id: &str) -> Result<bool> {
        self.conn.execute(
            "UPDATE tracks SET favorite = NOT favorite WHERE id=?1",
            [id],
        )?;
        Ok(self
            .conn
            .query_row("SELECT favorite FROM tracks WHERE id=?1", [id], |r| {
                r.get::<_, bool>(0)
            })?)
    }

    pub fn create_playlist(&self, name: &str) -> Result<i64> {
        self.conn
            .execute("INSERT OR IGNORE INTO playlists(name) VALUES(?1)", [name])?;
        Ok(self.conn.query_row(
            "SELECT id FROM playlists WHERE name=?1 COLLATE NOCASE",
            [name],
            |r| r.get(0),
        )?)
    }

    pub fn add_to_playlist(&self, playlist_id: i64, track_id: &str) -> Result<()> {
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id=?1",
            [playlist_id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO playlist_tracks(playlist_id, position, track_id) VALUES(?1, ?2, ?3)",
            params![playlist_id, position, track_id],
        )?;
        Ok(())
    }

    pub fn load_playlists(&self) -> Result<Vec<Playlist>> {
        let _profile = crate::profiling::span("db_load_playlists");
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, pt.track_id
             FROM playlists p
             LEFT JOIN playlist_tracks pt ON pt.playlist_id=p.id
             ORDER BY p.name COLLATE NOCASE, pt.position",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;

        let mut out = Vec::<Playlist>::new();
        for row in rows {
            let (id, name, track_id) = row?;
            if out.last().is_none_or(|playlist| playlist.id != id) {
                out.push(Playlist {
                    id,
                    name,
                    track_ids: Vec::new(),
                });
            }
            if let Some(track_id) = track_id
                && let Some(playlist) = out.last_mut()
            {
                playlist.track_ids.push(track_id);
            }
        }
        Ok(out)
    }

    pub fn save_playback(&self, state: &SavedPlayback) -> Result<()> {
        let value = serde_json::to_string(state)?;
        self.conn.execute(
            "INSERT INTO app_state(key,value) VALUES('playback',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [value],
        )?;
        Ok(())
    }

    pub fn load_playback(&self) -> Result<SavedPlayback> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM app_state WHERE key='playback'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match value {
            Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
            None => Ok(SavedPlayback::default()),
        }
    }

    pub fn load_added_at(&self) -> Result<HashMap<String, i64>> {
        let mut stmt = self.conn.prepare("SELECT id,added_at FROM tracks")?;
        Ok(stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn load_track_stats(&self) -> Result<HashMap<String, TrackStats>> {
        let mut stmt = self.conn.prepare(
            "SELECT track_id,play_count,total_listen_ms,last_played_at,resume_position_ms FROM track_stats",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TrackStats {
                    track_id: row.get(0)?,
                    play_count: row.get::<_, i64>(1)?.max(0) as u64,
                    total_listen_ms: row.get::<_, i64>(2)?.max(0) as u64,
                    last_played_at: row.get(3)?,
                    resume_position_ms: row.get::<_, i64>(4)?.max(0) as u64,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn start_history(&self, track_id: &str) -> Result<i64> {
        self.conn
            .execute("INSERT INTO history(track_id) VALUES(?1)", [track_id])?;
        Ok(self.conn.last_insert_rowid())
    }

    fn update_history_tx(tx: &rusqlite::Transaction<'_>, update: HistoryUpdate<'_>) -> Result<()> {
        tx.execute(
            "UPDATE history SET listened_ms=listened_ms+?2,position_ms=?3,counted=counted OR ?4,completed=completed OR ?5 WHERE id=?1",
            params![
                update.history_id,
                update.listened_delta_ms.min(i64::MAX as u64) as i64,
                update.position_ms.min(i64::MAX as u64) as i64,
                update.count_now,
                update.completed
            ],
        )?;
        tx.execute(
            "INSERT INTO track_stats(track_id,play_count,total_listen_ms,last_played_at,resume_position_ms)
             VALUES(?1,?2,?3,unixepoch(),?4)
             ON CONFLICT(track_id) DO UPDATE SET
                play_count=play_count+excluded.play_count,
                total_listen_ms=total_listen_ms+excluded.total_listen_ms,
                last_played_at=excluded.last_played_at,
                resume_position_ms=excluded.resume_position_ms",
            params![
                update.track_id,
                i64::from(update.count_now && !update.was_counted),
                update.listened_delta_ms.min(i64::MAX as u64) as i64,
                if update.completed { 0 } else { update.position_ms }
                    .min(i64::MAX as u64) as i64,
            ],
        )?;
        Ok(())
    }

    pub fn update_history(&mut self, update: HistoryUpdate<'_>) -> Result<()> {
        let tx = self.conn.transaction()?;
        Self::update_history_tx(&tx, update)?;
        tx.commit()?;
        Ok(())
    }

    pub fn update_history_and_playback(
        &mut self,
        update: HistoryUpdate<'_>,
        state: &SavedPlayback,
    ) -> Result<()> {
        let playback = serde_json::to_string(state)?;
        let tx = self.conn.transaction()?;
        Self::update_history_tx(&tx, update)?;
        tx.execute(
            "INSERT INTO app_state(key,value) VALUES('playback',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [playback],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,track_id,started_at,listened_ms,position_ms,counted,completed
             FROM history WHERE listened_ms>=5000 ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit.min(i64::MAX as usize) as i64], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                track_id: row.get(1)?,
                started_at: row.get(2)?,
                listened_ms: row.get::<_, i64>(3)?.max(0) as u64,
                position_ms: row.get::<_, i64>(4)?.max(0) as u64,
                counted: row.get(5)?,
                completed: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn load_smart_playlists(&self) -> Result<Vec<SmartPlaylist>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,name,match_mode,rules_json,sort_field,descending,item_limit FROM smart_playlists ORDER BY name COLLATE NOCASE",
        )?;
        let raw = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.into_iter()
            .map(|(id, name, mode, rules, sort_field, descending, limit)| {
                Ok(SmartPlaylist {
                    id,
                    name,
                    match_mode: if mode == "any" {
                        SmartMatch::Any
                    } else {
                        SmartMatch::All
                    },
                    rules: serde_json::from_str(&rules)?,
                    sort_field,
                    descending,
                    limit: limit.map(|value| value.max(0) as usize),
                })
            })
            .collect()
    }

    pub fn save_smart_playlist(&self, playlist: &SmartPlaylist) -> Result<()> {
        self.conn.execute(
            "UPDATE smart_playlists SET name=?2,match_mode=?3,rules_json=?4,sort_field=?5,descending=?6,item_limit=?7 WHERE id=?1",
            params![playlist.id, playlist.name, if playlist.match_mode == SmartMatch::Any { "any" } else { "all" }, serde_json::to_string(&playlist.rules)?, playlist.sort_field, playlist.descending, playlist.limit.map(|value| value.min(i64::MAX as usize) as i64)],
        )?;
        Ok(())
    }

    pub fn save_queue(&mut self, name: &str, track_ids: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO saved_queues(name,updated_at) VALUES(?1,unixepoch()) ON CONFLICT(name) DO UPDATE SET updated_at=excluded.updated_at",
            [name],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM saved_queues WHERE name=?1 COLLATE NOCASE",
            [name],
            |row| row.get(0),
        )?;
        tx.execute("DELETE FROM saved_queue_tracks WHERE queue_id=?1", [id])?;
        for (position, track_id) in track_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO saved_queue_tracks(queue_id,position,track_id) VALUES(?1,?2,?3)",
                params![id, position.min(i64::MAX as usize) as i64, track_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_saved_queues(&self) -> Result<Vec<SavedQueue>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id,name FROM saved_queues ORDER BY updated_at DESC")?;
        let base = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        base.into_iter()
            .map(|(id, name)| {
                let mut tracks = self.conn.prepare(
                    "SELECT track_id FROM saved_queue_tracks WHERE queue_id=?1 ORDER BY position",
                )?;
                let track_ids = tracks
                    .query_map([id], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(SavedQueue {
                    id,
                    name,
                    track_ids,
                })
            })
            .collect()
    }

    pub fn gain_analysis_candidates(
        &self,
        force: bool,
    ) -> Result<Vec<(String, std::path::PathBuf, u64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,path,file_size,modified_ns FROM tracks
             WHERE available=1 AND (?1 OR gain_db IS NULL OR gain_file_size<>file_size OR gain_modified_ns<>modified_ns)",
        )?;
        let rows = stmt.query_map([force], |row| {
            Ok((
                row.get(0)?,
                std::path::PathBuf::from(row.get::<_, String>(1)?),
                row.get::<_, i64>(2)?.max(0) as u64,
                row.get(3)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_gain(
        &self,
        track_id: &str,
        result: ReplayGainAnalysis,
        size: u64,
        modified: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET gain_db=?2,true_peak_db=?3,gain_file_size=?4,gain_modified_ns=?5 WHERE id=?1",
            params![track_id, result.gain_db, result.true_peak_db, size.min(i64::MAX as u64) as i64, modified],
        )?;
        Ok(())
    }

    pub fn track_gain(
        &self,
        track_id: &str,
        album_mode: bool,
    ) -> Result<Option<ReplayGainAnalysis>> {
        if album_mode {
            let mut stmt = self.conn.prepare(
                "SELECT peer.gain_db,peer.true_peak_db,peer.duration_ms
                 FROM tracks current JOIN tracks peer
                   ON lower(peer.album)=lower(current.album)
                  AND lower(peer.album_artist)=lower(current.album_artist)
                 WHERE current.id=?1 AND peer.gain_db IS NOT NULL",
            )?;
            let rows = stmt
                .query_map([track_id], |row| {
                    Ok((
                        row.get::<_, f64>(0)?,
                        row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                        row.get::<_, i64>(2)?.max(1) as f64,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if rows.is_empty() {
                return Ok(None);
            }
            let total_weight = rows.iter().map(|(_, _, weight)| weight).sum::<f64>();
            let energy = rows
                .iter()
                .map(|(gain, _, weight)| weight * 10f64.powf(-gain / 10.0))
                .sum::<f64>()
                / total_weight;
            Ok(Some(ReplayGainAnalysis {
                gain_db: -10.0 * energy.log10(),
                true_peak_db: rows
                    .iter()
                    .map(|(_, peak, _)| *peak)
                    .fold(f64::NEG_INFINITY, f64::max),
            }))
        } else {
            self.conn
                .query_row(
                    "SELECT gain_db,true_peak_db FROM tracks WHERE id=?1",
                    [track_id],
                    |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?)),
                )
                .map(|(gain, peak)| {
                    gain.zip(peak)
                        .map(|(gain_db, true_peak_db)| ReplayGainAnalysis {
                            gain_db,
                            true_peak_db,
                        })
                })
                .map_err(Into::into)
        }
    }
}

fn row_to_track(row: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: row.get(0)?,
        source_id: row.get(1)?,
        relative_path: row.get(2)?,
        path: row.get::<_, String>(3)?.into(),
        title: row.get(4)?,
        artist: row.get(5)?,
        album_artist: row.get(6)?,
        album: row.get(7)?,
        genre: row.get(8)?,
        year: row.get(9)?,
        disc_number: row.get(10)?,
        track_number: row.get(11)?,
        duration_ms: row.get::<_, i64>(12)?.max(0) as u64,
        cover_path: row.get::<_, Option<String>>(13)?.map(Into::into),
        available: row.get(14)?,
        favorite: row.get(15)?,
    })
}

#[derive(Debug, Clone)]
pub struct ScannedTrack {
    pub track: Track,
    pub file_size: u64,
    pub modified_ns: i64,
}

pub fn group_albums(tracks: &[Track]) -> Vec<Album> {
    let mut grouped: BTreeMap<(String, String), Album> = BTreeMap::new();
    for track in tracks {
        let artist = if track.album_artist.is_empty() {
            &track.artist
        } else {
            &track.album_artist
        };
        let key = (artist.to_lowercase(), track.album.to_lowercase());
        let album = grouped.entry(key.clone()).or_insert_with(|| Album {
            key: format!("{}\u{1f}{}", key.0, key.1),
            title: track.album.clone(),
            artist: artist.clone(),
            year: track.year,
            cover_path: track.cover_path.clone(),
            track_ids: Vec::new(),
            available_tracks: 0,
        });
        album.track_ids.push(track.id.clone());
        album.available_tracks += usize::from(track.available);
        if album.cover_path.is_none() {
            album.cover_path = track.cover_path.clone();
        }
    }
    grouped.into_values().collect()
}

pub fn group_artists(tracks: &[Track]) -> Vec<Artist> {
    let mut tracks_by_artist: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut albums_by_artist: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for track in tracks {
        let artist = if track.artist.is_empty() {
            "Unknown Artist"
        } else {
            &track.artist
        };
        tracks_by_artist
            .entry(artist.to_owned())
            .or_default()
            .push(track.id.clone());
        albums_by_artist
            .entry(artist.to_owned())
            .or_default()
            .insert(track.album.to_lowercase());
    }
    tracks_by_artist
        .into_iter()
        .map(|(name, track_ids)| Artist {
            album_count: albums_by_artist.get(&name).map_or(0, BTreeSet::len),
            name,
            track_ids,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, number: u32) -> Track {
        Track {
            id: id.into(),
            source_id: "s".into(),
            relative_path: format!("{id}.flac"),
            path: format!("/{id}.flac").into(),
            title: id.into(),
            artist: "Artist".into(),
            album_artist: "Artist".into(),
            album: "Album".into(),
            genre: String::new(),
            year: Some(2026),
            disc_number: 1,
            track_number: number,
            duration_ms: 1000,
            cover_path: None,
            available: true,
            favorite: false,
        }
    }

    #[test]
    fn schema_persists_tracks_and_playlists() -> Result<()> {
        let mut db = Database::open_memory()?;
        let scanned = vec![ScannedTrack {
            track: track("one", 1),
            file_size: 1,
            modified_ns: 1,
        }];
        db.upsert_scan("s", Path::new("/music"), "Music", &scanned)?;
        assert_eq!(db.load_tracks()?.len(), 1);
        let playlist = db.create_playlist("Mix")?;
        db.add_to_playlist(playlist, "one")?;
        assert_eq!(db.load_playlists()?[0].track_ids, ["one"]);
        Ok(())
    }

    #[test]
    fn reconciles_a_moved_track_without_losing_user_state() -> Result<()> {
        let mut db = Database::open_memory()?;
        let old = ScannedTrack {
            track: track("old-id", 1),
            file_size: 42,
            modified_ns: 1,
        };
        db.upsert_scan("s", Path::new("/music"), "Music", &[old])?;
        db.toggle_favorite("old-id")?;
        let playlist = db.create_playlist("Mix")?;
        db.add_to_playlist(playlist, "old-id")?;
        db.save_playback(&SavedPlayback {
            queue: vec!["old-id".into()],
            current_index: Some(0),
            ..SavedPlayback::default()
        })?;

        let mut moved_track = track("new-id", 1);
        moved_track.title = "old-id".into();
        let moved = ScannedTrack {
            track: moved_track,
            file_size: 42,
            modified_ns: 2,
        };
        let mappings = db.upsert_scan("s", Path::new("/music"), "Music", &[moved])?;

        assert_eq!(mappings, [("old-id".into(), "new-id".into())]);
        let tracks = db.load_tracks()?;
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, "new-id");
        assert!(tracks[0].favorite);
        assert_eq!(db.load_playlists()?[0].track_ids, ["new-id"]);
        assert_eq!(db.load_playback()?.queue, ["new-id"]);
        Ok(())
    }

    #[test]
    fn groups_album() {
        let albums = group_albums(&[track("one", 1), track("two", 2)]);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].track_ids.len(), 2);
    }

    #[test]
    fn prune_removes_missing_tracks_only_from_connected_sources() -> Result<()> {
        let root = std::env::temp_dir().join(format!("muscli-prune-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let mut db = Database::open_memory()?;
        let mut missing = track("missing", 1);
        missing.path = root.join("missing.flac");
        db.upsert_scan(
            "s",
            &root,
            "Music",
            &[ScannedTrack {
                track: missing,
                file_size: 1,
                modified_ns: 1,
            }],
        )?;
        assert_eq!(db.prune_missing_tracks()?, 1);
        assert!(db.load_tracks()?.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn scan_prune_preserves_seen_files_that_failed_to_parse() -> Result<()> {
        let mut db = Database::open_memory()?;
        let scanned = ScannedTrack {
            track: track("one", 1),
            file_size: 1,
            modified_ns: 1,
        };
        db.upsert_scan("s", Path::new("/music"), "Music", &[scanned])?;
        db.upsert_scan("s", Path::new("/music"), "Music", &[])?;
        assert_eq!(
            db.prune_missing_for_source("s", &BTreeSet::from(["one.flac".into()]))?,
            0
        );
        let tracks = db.load_tracks()?;
        assert_eq!(tracks.len(), 1);
        assert!(tracks[0].available);
        db.upsert_scan("s", Path::new("/music"), "Music", &[])?;
        assert_eq!(db.prune_missing_for_source("s", &BTreeSet::new())?, 1);
        assert!(db.load_tracks()?.is_empty());
        Ok(())
    }
}
