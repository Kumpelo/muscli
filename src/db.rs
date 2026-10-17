use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::model::{
    Album, Artist, HistoryEntry, Playlist, RepeatMode, ReplayGainAnalysis, SavedPlayback,
    SavedQueue, SmartMatch, SmartPlaylist, Track, TrackStats,
};

/// Three connections write concurrently: the UI, the scanner thread and
/// ReplayGain analysis. Upserting a large source can hold the write lock for
/// well over a second, and the previous two-second budget turned that into
/// spurious SQLITE_BUSY failures on unrelated writes.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Database {
    conn: Connection,
}

/// Apply the connection settings every database uses. Shared by the on-disk
/// and in-memory openers so the tests run what a user runs.
fn configure(conn: &Connection, on_disk: bool) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    if on_disk {
        // Meaningless for an in-memory database, which has no journal or file
        // to map.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", -16_384i64)?;
        conn.pragma_update(None, "mmap_size", 134_217_728i64)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct PlaybackCheckpoint {
    current_index: Option<usize>,
    position_ms: u64,
    volume: f64,
    last_nonzero_volume: Option<f64>,
    shuffle: bool,
    repeat: RepeatMode,
    volume_is_position: bool,
}

impl Default for PlaybackCheckpoint {
    fn default() -> Self {
        Self {
            current_index: None,
            position_ms: 0,
            volume: 1.0,
            last_nonzero_volume: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume_is_position: false,
        }
    }
}

impl From<&SavedPlayback> for PlaybackCheckpoint {
    fn from(value: &SavedPlayback) -> Self {
        Self {
            current_index: value.current_index,
            position_ms: value.position_ms,
            volume: value.volume,
            last_nonzero_volume: value.last_nonzero_volume,
            shuffle: value.shuffle,
            repeat: value.repeat,
            volume_is_position: value.volume_is_position,
        }
    }
}

impl PlaybackCheckpoint {
    fn apply_to(self, playback: &mut SavedPlayback) {
        playback.current_index = self.current_index;
        playback.position_ms = self.position_ms;
        playback.volume = self.volume;
        playback.last_nonzero_volume = self.last_nonzero_volume;
        playback.shuffle = self.shuffle;
        playback.repeat = self.repeat;
        playback.volume_is_position = self.volume_is_position;
    }
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

#[derive(Debug, Default)]
pub struct LibraryState {
    pub tracks: Vec<Track>,
    pub stats: HashMap<String, TrackStats>,
    pub added_at: HashMap<String, i64>,
}

/// One line of a listening summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    pub id: String,
    pub label: String,
    pub count: u64,
}

/// What has been listened to over some period.
#[derive(Debug, Clone, Default)]
pub struct ListeningSummary {
    pub plays: u64,
    pub listened_ms: u64,
    pub top_tracks: Vec<SummaryRow>,
    pub top_artists: Vec<SummaryRow>,
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
        configure(&conn, true)?;
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
        configure(&conn, false)?;
        let mut db = Self { conn };
        db.migrate()?;
        db.seed_smart_playlists()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 9 {
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

        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 4 {
            let tx = self.conn.transaction()?;
            // Seeded playlists carry a translation key so their names can
            // follow the interface language; a playlist the user made has none
            // and keeps the name they chose.
            tx.execute_batch(
                "ALTER TABLE smart_playlists ADD COLUMN preset_key TEXT;
                 UPDATE smart_playlists SET preset_key='preset.metal_favorites'
                    WHERE name='Metal favorito';
                 UPDATE smart_playlists SET preset_key='preset.recently_added'
                    WHERE name='Agregadas recientemente';
                 UPDATE smart_playlists SET preset_key='preset.unplayed'
                    WHERE name='No escuchadas';
                 UPDATE smart_playlists SET preset_key='preset.most_played'
                    WHERE name='Más reproducidas';
                 UPDATE smart_playlists SET preset_key='preset.long_tracks'
                    WHERE name='Más de 8 minutos';
                 PRAGMA user_version = 5;",
            )?;
            tx.commit()?;
        }
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 5 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                // El checkpoint pisa todo lo que la fila anterior al reparto traía,
                // menos la marca que dice en qué unidad está el volumen. Heredarla
                // hace que una posición se convierta por la curva una segunda vez.
                "INSERT INTO app_state(key,value)
                    SELECT 'playback_queue', json_extract(value,'$.queue')
                    FROM app_state
                    WHERE key='playback'
                        AND json_valid(value)
                        AND EXISTS (SELECT 1 FROM app_state WHERE key='playback_state')
                    ON CONFLICT(key) DO NOTHING;

                    DELETE FROM app_state
                    WHERE key='playback'
                        AND EXISTS (SELECT 1 FROM app_state WHERE key='playback_state');

                    PRAGMA user_version = 6;",
            )?;
            tx.commit()?;
        }
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 6 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "CREATE TABLE track_mix_analysis (
                    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                    bpm REAL,
                    confidence REAL NOT NULL,
                    energy_rms REAL NOT NULL,
                    error_text TEXT,
                    file_size INTEGER NOT NULL,
                    modified_ns INTEGER NOT NULL,
                    algorithm_version INTEGER NOT NULL
                );
                PRAGMA user_version = 7;",
            )?;
            tx.commit()?;
        }
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 7 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "ALTER TABLE tracks ADD COLUMN release_id TEXT NOT NULL DEFAULT '';
                 ALTER TABLE tracks ADD COLUMN compilation INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE tracks ADD COLUMN metadata_version INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 8;",
            )?;
            tx.commit()?;
        }
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 8 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(
                "ALTER TABLE track_mix_analysis ADD COLUMN boundaries TEXT;
                PRAGMA user_version = 9;",
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    fn seed_smart_playlists(&self) -> Result<()> {
        // A short-lived pre-merge v5 build could create both the translated
        // legacy row and a second English seed with the same preset_key.
        // Repair that state first, then enforce the key as the identity of a
        // built-in preset. User-created smart playlists keep preset_key NULL.
        self.conn.execute_batch(
            "DELETE FROM smart_playlists
             WHERE preset_key IS NOT NULL
               AND id NOT IN (
                    SELECT MIN(id) FROM smart_playlists
                    WHERE preset_key IS NOT NULL
                    GROUP BY preset_key
               );
             CREATE UNIQUE INDEX IF NOT EXISTS smart_playlists_preset_key
             ON smart_playlists(preset_key)
             WHERE preset_key IS NOT NULL;",
        )?;

        // Stored under stable English names with a translation key beside them,
        // so the rows survive a language change and the displayed name follows
        // it.
        let presets = [
            (
                "preset.metal_favorites",
                "Favourite metal",
                "all",
                r#"[{"field":"genre","operator":"contains","value":"metal"},{"field":"favorite","operator":"is","value":true}]"#,
                "title",
                false,
                None,
            ),
            (
                "preset.recently_added",
                "Recently added",
                "all",
                r#"[{"field":"added_days","operator":"lte","value":30}]"#,
                "added_at",
                true,
                None,
            ),
            (
                "preset.unplayed",
                "Unplayed",
                "all",
                r#"[{"field":"played","operator":"is","value":false}]"#,
                "title",
                false,
                None,
            ),
            (
                "preset.most_played",
                "Most played",
                "all",
                r#"[{"field":"play_count","operator":"gte","value":1}]"#,
                "play_count",
                true,
                Some(100),
            ),
            (
                "preset.long_tracks",
                "Over 8 minutes",
                "all",
                r#"[{"field":"duration_ms","operator":"gte","value":480000}]"#,
                "duration",
                true,
                None,
            ),
        ];
        for (preset_key, name, mode, rules, sort, descending, limit) in presets {
            self.conn.execute(
                "INSERT OR IGNORE INTO smart_playlists(name,match_mode,rules_json,sort_field,descending,item_limit,preset_key) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![name, mode, rules, sort, descending, limit, preset_key],
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
        missing_track_ids: &[String],
    ) -> Result<Vec<(String, String)>> {
        let _profile = crate::profiling::span("db_upsert_scan");
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO sources(id, root, label, available, last_scan) VALUES(?1, ?2, ?3, 1, unixepoch())
             ON CONFLICT(id) DO UPDATE SET root=excluded.root, label=excluded.label, available=1, last_scan=excluded.last_scan",
            params![source_id, root.to_string_lossy(), label],
        )?;
        if !missing_track_ids.is_empty() {
            let mut mark_missing =
                tx.prepare_cached("UPDATE tracks SET available=0 WHERE source_id=?1 AND id=?2")?;
            for track_id in missing_track_ids {
                mark_missing.execute(params![source_id, track_id])?;
            }
        }
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO tracks(
                    id, source_id, relative_path, path, title, artist, album_artist, album, genre,
                    year, disc_number, track_number, duration_ms, cover_path, file_size, modified_ns, available, release_id, compilation, metadata_version
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,1,?17,?18,1)
                 ON CONFLICT(id) DO UPDATE SET
                    path=excluded.path, title=excluded.title, artist=excluded.artist,
                    album_artist=excluded.album_artist, album=excluded.album, genre=excluded.genre,
                    year=excluded.year, disc_number=excluded.disc_number, track_number=excluded.track_number,
                    duration_ms=excluded.duration_ms, cover_path=excluded.cover_path,
                    file_size=excluded.file_size, modified_ns=excluded.modified_ns, available=1,
                    release_id=excluded.release_id, compilation=excluded.compilation, metadata_version=1",
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
                    item.track.release_id,
                    item.track.compilation,
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
        let playback_queue: Option<String> = tx
            .query_row(
                "SELECT value FROM app_state WHERE key='playback_queue'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(raw) = playback_queue
            && let Ok(mut queue) = serde_json::from_str::<Vec<String>>(&raw)
        {
            let mut changed = false;
            for id in &mut queue {
                if let Some((_, new_id)) = moved_tracks.iter().find(|(old_id, _)| id == old_id) {
                    *id = new_id.clone();
                    changed = true;
                }
            }
            if changed {
                tx.execute(
                    "UPDATE app_state SET value=?1 WHERE key='playback_queue'",
                    [serde_json::to_string(&queue)?],
                )?;
            }
        }
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

    pub fn mark_source_unavailable_by_root(&mut self, root: &Path) -> Result<usize> {
        let root = root.to_string_lossy().into_owned();
        let tx = self.conn.transaction()?;
        let id = tx
            .query_row(
                "SELECT id FROM sources WHERE root=?1",
                [root.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(id) = id else {
            tx.commit()?;
            return Ok(0);
        };

        let mut changed = 0;
        changed += tx.execute(
            "UPDATE sources SET available=0 WHERE id=?1 AND available!=0",
            [&id],
        )?;
        changed += tx.execute(
            "UPDATE tracks SET available=0 WHERE source_id=?1 AND available!=0",
            [&id],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    pub fn mark_missing_sources(&mut self, available_ids: &BTreeSet<String>) -> Result<usize> {
        let ids = {
            let mut stmt = self.conn.prepare("SELECT id FROM sources")?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut changed = 0;
        // One transaction: a source and its tracks must never be left disagreeing
        // about whether the drive is connected.
        let tx = self.conn.transaction()?;
        for id in ids {
            if !available_ids.contains(&id) {
                changed += tx.execute(
                    "UPDATE sources SET available=0 WHERE id=?1 AND available!=0",
                    [&id],
                )?;
                changed += tx.execute(
                    "UPDATE tracks SET available=0 WHERE source_id=?1 AND available!=0",
                    [&id],
                )?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn prune_missing_for_source(
        &mut self,
        source_id: &str,
        preserve_paths: &BTreeSet<String>,
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
            .filter(|(_, relative)| !preserve_paths.contains(relative))
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
        };
        let tx = self.conn.transaction()?;
        let mut changed = 0;
        if !preserve_paths.is_empty() {
            let mut preserve = tx.prepare_cached(
                "UPDATE tracks SET available=1
                 WHERE source_id=?1 AND relative_path=?2 AND available=0",
            )?;
            for relative in preserve_paths {
                changed += preserve.execute(params![source_id, relative])?;
            }
        }
        {
            let mut delete_stale = tx.prepare_cached("DELETE FROM tracks WHERE id=?1")?;
            for id in &stale {
                changed += delete_stale.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(changed)
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

    pub fn load_library_state(&self) -> Result<LibraryState> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.source_id, t.relative_path, t.path, t.title, t.artist, t.album_artist,
                    t.album, t.genre, t.year, t.disc_number, t.track_number, t.duration_ms,
                    t.cover_path, t.available, t.favorite, t.added_at,
                    s.track_id, s.play_count, s.total_listen_ms, s.last_played_at,
                    s.resume_position_ms, t.release_id, t.compilation
             FROM tracks t
             LEFT JOIN track_stats s ON s.track_id=t.id
             ORDER BY t.album_artist COLLATE NOCASE, t.album COLLATE NOCASE,
                      t.disc_number, t.track_number, t.title COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            let track = row_to_track(row)?;
            let added_at = row.get::<_, i64>(16)?;
            let stats_id = row.get::<_, Option<String>>(17)?;
            let stats = if let Some(track_id) = stats_id {
                Some(TrackStats {
                    track_id,
                    play_count: row.get::<_, Option<i64>>(18)?.unwrap_or_default().max(0) as u64,
                    total_listen_ms: row.get::<_, Option<i64>>(19)?.unwrap_or_default().max(0)
                        as u64,
                    last_played_at: row.get::<_, Option<i64>>(20)?,
                    resume_position_ms: row.get::<_, Option<i64>>(21)?.unwrap_or_default().max(0)
                        as u64,
                })
            } else {
                None
            };
            Ok((track, added_at, stats))
        })?;

        let mut state = LibraryState::default();
        for row in rows {
            let (track, added_at, stats) = row?;
            state.added_at.insert(track.id.clone(), added_at);
            if let Some(stats) = stats {
                state.stats.insert(track.id.clone(), stats);
            }
            state.tracks.push(track);
        }
        Ok(state)
    }

    pub fn load_tracks(&self) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, relative_path, path, title, artist, album_artist, album, genre,
                    year, disc_number, track_number, duration_ms, cover_path, available, favorite, release_id, compilation
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
                    file_size, CASE WHEN metadata_version=0 THEN -1 ELSE modified_ns END, release_id, compilation
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

    fn save_playback_tx(
        tx: &rusqlite::Transaction<'_>,
        state: &SavedPlayback,
        queue: Option<&[String]>,
    ) -> Result<()> {
        let checkpoint = serde_json::to_string(&PlaybackCheckpoint::from(state))?;
        tx.execute(
            "INSERT INTO app_state(key,value) VALUES('playback_state',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [checkpoint],
        )?;
        if let Some(queue) = queue {
            let queue = serde_json::to_string(queue)?;
            tx.execute(
                "INSERT INTO app_state(key,value) VALUES('playback_queue',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [queue],
            )?;
        }
        Ok(())
    }

    pub fn save_playback(&mut self, state: &SavedPlayback, queue: Option<&[String]>) -> Result<()> {
        let tx = self.conn.transaction()?;
        Self::save_playback_tx(&tx, state, queue)?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_playback(&self) -> Result<SavedPlayback> {
        let legacy: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM app_state WHERE key='playback'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let mut playback = legacy
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();

        let checkpoint: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM app_state WHERE key='playback_state'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(checkpoint) = checkpoint
            .as_deref()
            .and_then(|raw| serde_json::from_str::<PlaybackCheckpoint>(raw).ok())
        {
            checkpoint.apply_to(&mut playback);
        }

        let queue: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM app_state WHERE key='playback_queue'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(queue) = queue
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        {
            playback.queue = queue;
        }
        Ok(playback)
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

    /// Forget every resume point, leaving counts and history untouched.
    ///
    /// Returns how many tracks had one. Resume points are also cleared by
    /// playing a track to its end, so this is for clearing the lot at once.
    pub fn forget_resume_positions(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE track_stats SET resume_position_ms=0 WHERE resume_position_ms>0",
            [],
        )?)
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
        queue: Option<&[String]>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        Self::update_history_tx(&tx, update)?;
        Self::save_playback_tx(&tx, state, queue)?;
        tx.commit()?;
        Ok(())
    }

    /// What has actually been listened to.
    ///
    /// Lifetime totals come from `track_stats`, which survives history
    /// pruning. A bounded window must instead be aggregated from `history`:
    /// filtering a lifetime counter by only its last-played timestamp would
    /// incorrectly pull old plays into the requested period.
    pub fn listening_summary(&self, since: Option<i64>) -> Result<ListeningSummary> {
        if let Some(since) = since {
            return self.listening_summary_since(since);
        }

        let (played, total_ms): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(play_count),0), COALESCE(SUM(total_listen_ms),0)
             FROM track_stats",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.title, t.artist, s.play_count
             FROM track_stats s JOIN tracks t ON t.id=s.track_id
             WHERE s.play_count > 0
             ORDER BY s.play_count DESC, t.title COLLATE NOCASE
             LIMIT 20",
        )?;
        let top_tracks = stmt
            .query_map([], |row| {
                Ok(SummaryRow {
                    id: row.get(0)?,
                    label: format!(
                        "{} — {}",
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(1)?
                    ),
                    count: row.get::<_, i64>(3)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut stmt = self.conn.prepare(
            "SELECT t.artist, SUM(s.play_count) AS plays
             FROM track_stats s JOIN tracks t ON t.id=s.track_id
             WHERE s.play_count > 0
             GROUP BY lower(t.artist)
             ORDER BY plays DESC, t.artist COLLATE NOCASE
             LIMIT 20",
        )?;
        let top_artists = stmt
            .query_map([], |row| {
                Ok(SummaryRow {
                    id: String::new(),
                    label: row.get::<_, String>(0)?,
                    count: row.get::<_, i64>(1)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(ListeningSummary {
            plays: played.max(0) as u64,
            listened_ms: total_ms.max(0) as u64,
            top_tracks,
            top_artists,
        })
    }

    fn listening_summary_since(&self, since: i64) -> Result<ListeningSummary> {
        let (played, total_ms): (i64, i64) = self.conn.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN counted != 0 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(listened_ms),0)
             FROM history
             WHERE started_at >= ?1",
            [since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let mut stmt = self.conn.prepare(
            "SELECT
                t.id,
                t.title,
                t.artist,
                SUM(CASE WHEN h.counted != 0 THEN 1 ELSE 0 END) AS plays
             FROM history h
             JOIN tracks t ON t.id=h.track_id
             WHERE h.started_at >= ?1
             GROUP BY t.id, t.title, t.artist
             HAVING SUM(CASE WHEN h.counted != 0 THEN 1 ELSE 0 END) > 0
             ORDER BY plays DESC, t.title COLLATE NOCASE
             LIMIT 20",
        )?;
        let top_tracks = stmt
            .query_map([since], |row| {
                Ok(SummaryRow {
                    id: row.get(0)?,
                    label: format!(
                        "{} — {}",
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(1)?
                    ),
                    count: row.get::<_, i64>(3)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut stmt = self.conn.prepare(
            "SELECT
                t.artist,
                SUM(CASE WHEN h.counted != 0 THEN 1 ELSE 0 END) AS plays
             FROM history h
             JOIN tracks t ON t.id=h.track_id
             WHERE h.started_at >= ?1
             GROUP BY lower(t.artist)
             HAVING SUM(CASE WHEN h.counted != 0 THEN 1 ELSE 0 END) > 0
             ORDER BY plays DESC, t.artist COLLATE NOCASE
             LIMIT 20",
        )?;
        let top_artists = stmt
            .query_map([since], |row| {
                Ok(SummaryRow {
                    id: String::new(),
                    label: row.get::<_, String>(0)?,
                    count: row.get::<_, i64>(1)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(ListeningSummary {
            plays: played.max(0) as u64,
            listened_ms: total_ms.max(0) as u64,
            top_tracks,
            top_artists,
        })
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
            "SELECT id,name,match_mode,rules_json,sort_field,descending,item_limit,preset_key FROM smart_playlists ORDER BY name COLLATE NOCASE",
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
                    row.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.into_iter()
            .map(
                |(id, name, mode, rules, sort_field, descending, limit, preset_key)| {
                    Ok(SmartPlaylist {
                        id,
                        preset_key,
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
                },
            )
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
        {
            let mut insert_track = tx.prepare_cached(
                "INSERT INTO saved_queue_tracks(queue_id,position,track_id) VALUES(?1,?2,?3)",
            )?;
            for (position, track_id) in track_ids.iter().enumerate() {
                insert_track.execute(params![
                    id,
                    position.min(i64::MAX as usize) as i64,
                    track_id
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_saved_queues(&self) -> Result<Vec<SavedQueue>> {
        let mut stmt = self.conn.prepare(
            "SELECT q.id,q.name,qt.track_id
             FROM saved_queues q
             LEFT JOIN saved_queue_tracks qt ON qt.queue_id=q.id
             ORDER BY q.updated_at DESC,qt.position",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;

        let mut queues = Vec::<SavedQueue>::new();
        for row in rows {
            let (id, name, track_id) = row?;
            if queues.last().is_none_or(|queue| queue.id != id) {
                queues.push(SavedQueue {
                    id,
                    name,
                    track_ids: Vec::new(),
                });
            }
            if let Some(track_id) = track_id
                && let Some(queue) = queues.last_mut()
            {
                queue.track_ids.push(track_id);
            }
        }
        Ok(queues)
    }

    /// Only new or modified, available tracks require a MIX analysis. A row
    /// with unknown BPM is still complete and must not be retried on every run.
    pub fn mix_analysis_candidates(
        &self,
        force: bool,
    ) -> Result<Vec<(String, std::path::PathBuf, u64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id,t.path,t.file_size,t.modified_ns FROM tracks t
             LEFT JOIN track_mix_analysis a ON a.track_id=t.id
             WHERE t.available=1 AND
               (?1 OR a.track_id IS NULL OR a.file_size<>t.file_size
                   OR a.modified_ns<>t.modified_ns OR a.algorithm_version<>?2)
             ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![force, crate::audio::mix::ANALYSIS_VERSION], |row| {
            Ok((
                row.get(0)?,
                std::path::PathBuf::from(row.get::<_, String>(1)?),
                row.get::<_, i64>(2)?.max(0) as u64,
                row.get(3)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Save only when the indexed file still has the fingerprint that was
    /// analyzed. A scan may modify it while the background worker is busy.
    pub fn save_mix_analysis(
        &self,
        track_id: &str,
        analysis: crate::audio::mix::TrackAnalysis,
        size: u64,
        modified: i64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO track_mix_analysis
                (track_id,bpm,confidence,energy_rms,file_size,modified_ns,algorithm_version,boundaries)
             SELECT id,?2,?3,?4,?5,?6,?7,?8 FROM tracks
             WHERE id=?1 AND available=1 AND file_size=?5 AND modified_ns=?6
             ON CONFLICT(track_id) DO UPDATE SET
                bpm=excluded.bpm,
                confidence=excluded.confidence,
                energy_rms=excluded.energy_rms, boundaries=excluded.boundaries,
                error_text=NULL,
                file_size=excluded.file_size,
                modified_ns=excluded.modified_ns,
                algorithm_version=excluded.algorithm_version",
            params![
                track_id,
                analysis.bpm,
                analysis.confidence,
                analysis.energy_rms,
                size.min(i64::MAX as u64) as i64,
                modified,
                crate::audio::mix::ANALYSIS_VERSION,
                serde_json::to_string(&analysis.boundaries)?,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Unsupported codecs and corrupt files are recorded as failed. This
    /// prevents retry storms on every start; a changed file is retried.
    pub fn save_mix_failure(
        &self,
        track_id: &str,
        error: &str,
        size: u64,
        modified: i64,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO track_mix_analysis
                (track_id,bpm,confidence,energy_rms,error_text,file_size,modified_ns,algorithm_version)
             SELECT id,NULL,0,0,?2,?3,?4,?5 FROM tracks
             WHERE id=?1 AND available=1 AND file_size=?3 AND modified_ns=?4
             ON CONFLICT(track_id) DO UPDATE SET
                bpm=NULL,confidence=0,energy_rms=0,
                error_text=excluded.error_text,
                file_size=excluded.file_size,
                modified_ns=excluded.modified_ns,
                algorithm_version=excluded.algorithm_version",
            params![
                track_id,
                error,
                size.min(i64::MAX as u64) as i64,
                modified,
                crate::audio::mix::ANALYSIS_VERSION,
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn mix_analysis_for_path(
        &self,
        path: &Path,
    ) -> Result<Option<crate::audio::mix::TrackAnalysis>> {
        Ok(self
            .conn
            .query_row(
                "SELECT a.bpm,a.confidence,a.energy_rms,a.boundaries
                 FROM tracks t JOIN track_mix_analysis a ON a.track_id=t.id
                 WHERE t.path=?1 AND t.available=1 AND a.error_text IS NULL
                   AND a.file_size=t.file_size
                   AND a.modified_ns=t.modified_ns
                   AND a.algorithm_version=?2",
                params![path.to_string_lossy(), crate::audio::mix::ANALYSIS_VERSION],
                |row| {
                    Ok(crate::audio::mix::TrackAnalysis {
                        bpm: row.get(0)?,
                        confidence: row.get(1)?,
                        energy_rms: row.get(2)?,
                        boundaries: row
                            .get::<_, Option<String>>(3)?
                            .and_then(|raw| serde_json::from_str(&raw).ok())
                            .flatten(),
                    })
                },
            )
            .optional()?)
    }

    pub fn load_mix_analyses(&self) -> Result<HashMap<String, crate::audio::mix::TrackAnalysis>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id,a.bpm,a.confidence,a.energy_rms,a.boundaries FROM tracks t
             JOIN track_mix_analysis a ON a.track_id=t.id WHERE t.available=1
             AND a.error_text IS NULL AND a.file_size=t.file_size
             AND a.modified_ns=t.modified_ns AND a.algorithm_version=?1",
        )?;
        let rows = stmt.query_map([crate::audio::mix::ANALYSIS_VERSION], |row| {
            Ok((
                row.get(0)?,
                crate::audio::mix::TrackAnalysis {
                    bpm: row.get(1)?,
                    confidence: row.get(2)?,
                    energy_rms: row.get(3)?,
                    boundaries: row
                        .get::<_, Option<String>>(4)?
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .flatten(),
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Count cached descriptors without decoding, including uncertain BPMs.
    pub fn mix_analysis_status(&self) -> Result<(usize, usize, usize, usize)> {
        let available: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM tracks WHERE available=1", [], |row| {
                    row.get(0)
                })?;
        let (analyzed, with_bpm, failed): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),COUNT(a.bpm),COUNT(a.error_text)
             FROM tracks t JOIN track_mix_analysis a ON t.id=a.track_id
             WHERE t.available=1 AND a.file_size=t.file_size
               AND a.modified_ns=t.modified_ns AND a.algorithm_version=?1",
            [crate::audio::mix::ANALYSIS_VERSION],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok((
            available as usize,
            analyzed as usize,
            with_bpm as usize,
            failed as usize,
        ))
    }

    /// Failed analyses are cached by the same file fingerprint as successful
    /// results. An explicit retry requests only failures, not valid analyses.
    pub fn gain_analysis_candidates(
        &self,
        retry_failed: bool,
    ) -> Result<Vec<(String, std::path::PathBuf, u64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,path,file_size,modified_ns FROM tracks
             WHERE available=1 AND
                 (gain_file_size IS NULL OR gain_modified_ns IS NULL
                  OR gain_file_size<>file_size OR gain_modified_ns<>modified_ns
                  OR (?1 AND gain_db IS NULL))",
        )?;
        let rows = stmt.query_map([retry_failed], |row| {
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

    /// Cache failures without changing audio files or requiring a schema
    /// migration. NULL gain_db plus matching fingerprint marks a failed file.
    /// A later source change or explicit retry makes it eligible again.
    pub fn save_gain_failure(&self, track_id: &str, size: u64, modified: i64) -> Result<()> {
        let size = size.min(i64::MAX as u64) as i64;
        self.conn.execute(
            "UPDATE tracks
             SET gain_db=NULL, true_peak_db=NULL, gain_file_size=?2, gain_modified_ns=?3
             WHERE id=?1 AND available=1 AND file_size=?2 AND modified_ns=?3",
            params![track_id, size, modified],
        )?;
        Ok(())
    }

    /// ReplayGain for an album as grouped by the library. This is important
    /// for compilations: the individual tracks can have different Album Artist
    /// tags, so joining only by their tag would undercount the release.
    pub fn album_gain_for_tracks(
        &self,
        current_track_id: &str,
        track_ids: &[String],
    ) -> Result<Option<ReplayGainAnalysis>> {
        if track_ids.is_empty() {
            return self.track_gain(current_track_id, false);
        }
        let mut stmt = self.conn.prepare(
            "SELECT gain_db,true_peak_db,duration_ms,
                    CASE WHEN gain_db IS NOT NULL AND true_peak_db IS NOT NULL
                              AND gain_file_size=file_size
                              AND gain_modified_ns=modified_ns
                         THEN 1 ELSE 0 END
             FROM tracks WHERE id=?1 AND available=1",
        )?;
        let mut samples = Vec::with_capacity(track_ids.len());
        for track_id in track_ids {
            let sample = stmt
                .query_row([track_id], |row| {
                    Ok((
                        row.get::<_, Option<f64>>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, i64>(2)?.max(1) as f64,
                        row.get::<_, i64>(3)? != 0,
                    ))
                })
                .optional()?;
            match sample {
                Some((Some(gain), Some(peak), weight, true)) => {
                    samples.push((gain, peak, weight));
                }
                _ => {
                    // Defer normalization until every member has an analysis.
                    // Still give the currently playing track its own gain.
                    return self.track_gain(current_track_id, false);
                }
            }
        }
        let total_weight = samples.iter().map(|(_, _, weight)| weight).sum::<f64>();
        let energy = samples
            .iter()
            .map(|(gain, _, weight)| weight * 10f64.powf(-gain / 10.0))
            .sum::<f64>()
            / total_weight;
        Ok(Some(ReplayGainAnalysis {
            gain_db: -10.0 * energy.log10(),
            true_peak_db: samples
                .iter()
                .map(|(_, peak, _)| *peak)
                .fold(f64::NEG_INFINITY, f64::max),
        }))
    }

    pub fn track_gain(
        &self,
        track_id: &str,
        album_mode: bool,
    ) -> Result<Option<ReplayGainAnalysis>> {
        if album_mode {
            let mut stmt = self.conn.prepare(
                // COLLATE NOCASE rather than lower(): lower() on both sides
                // makes the comparison opaque to the index, turning every
                // track load into a full join over the library.
                "SELECT peer.gain_db,peer.true_peak_db,peer.duration_ms,
                        CASE WHEN peer.gain_db IS NOT NULL
                                  AND peer.true_peak_db IS NOT NULL
                                  AND peer.gain_file_size=peer.file_size
                                  AND peer.gain_modified_ns=peer.modified_ns
                             THEN 1 ELSE 0 END
                 FROM tracks current JOIN tracks peer
                   ON peer.album=current.album COLLATE NOCASE
                  AND peer.album_artist=current.album_artist COLLATE NOCASE
                 WHERE current.id=?1 AND peer.available=1",
            )?;
            let rows = stmt
                .query_map([track_id], |row| {
                    Ok((
                        row.get::<_, Option<f64>>(0)?,
                        row.get::<_, Option<f64>>(1)?,
                        row.get::<_, i64>(2)?.max(1) as f64,
                        row.get::<_, i64>(3)? != 0,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if rows.is_empty() {
                return Ok(None);
            }
            // Never treat a partly scanned/analysed album as a complete one:
            // adding even a single track changes its integrated loudness.
            // Until all available peers are ready, use this track's own
            // up-to-date analysis rather than a misleading partial album gain.
            if rows.iter().any(|(_, _, _, valid)| !valid) {
                return self.track_gain(track_id, false);
            }
            let total_weight = rows.iter().map(|(_, _, weight, _)| weight).sum::<f64>();
            let energy = rows
                .iter()
                .map(|(gain, _, weight, _)| weight * 10f64.powf(-gain.unwrap_or(0.0) / 10.0))
                .sum::<f64>()
                / total_weight;
            Ok(Some(ReplayGainAnalysis {
                gain_db: -10.0 * energy.log10(),
                true_peak_db: rows
                    .iter()
                    .filter_map(|(_, peak, _, _)| *peak)
                    .fold(f64::NEG_INFINITY, f64::max),
            }))
        } else {
            // A track that is not in the index has no gain; that is an answer,
            // not an error. Returning QueryReturnedNoRows here aborted playback
            // of a track the scanner had just pruned.
            Ok(self
                .conn
                .query_row(
                    "SELECT gain_db,true_peak_db FROM tracks
                     WHERE id=?1
                       AND gain_file_size=file_size
                       AND gain_modified_ns=modified_ns",
                    [track_id],
                    |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?)),
                )
                .optional()?
                .and_then(|(gain, peak)| {
                    gain.zip(peak)
                        .map(|(gain_db, true_peak_db)| ReplayGainAnalysis {
                            gain_db,
                            true_peak_db,
                        })
                }))
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
        release_id: row.get("release_id")?,
        compilation: row.get("compilation")?,
    })
}

#[derive(Debug, Clone)]
pub struct ScannedTrack {
    pub track: Track,
    pub file_size: u64,
    pub modified_ns: i64,
}

/// Only conventional numbered disc directories are lifted to a release folder.
/// Arbitrary sibling folders are separate editions even when title/year agree.
fn release_folder(track: &Track) -> String {
    let parent = track.path.parent().unwrap_or_else(|| Path::new(""));
    let name = parent
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let compact = name.replace([' ', '-', '_'], "");
    let disc_folder = ["cd", "disc", "disk"].iter().any(|prefix| {
        compact.strip_prefix(prefix).is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.parse::<u32>().ok() == Some(track.disc_number)
                && track.disc_number > 0
        })
    });
    if disc_folder {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    }
    .to_string_lossy()
    .into_owned()
}

fn album_folder_key(track: &Track) -> (String, String, String, Option<i32>) {
    (
        track.source_id.clone(),
        release_folder(track),
        crate::model::normalize_text(&track.album).to_lowercase(),
        track.year,
    )
}

/// Recognize artist-sorted copies of one release only with independent evidence.
/// A bucket includes conflicting tags/years/artwork so ambiguous peers veto the
/// inference instead of silently being excluded from a plausible subset.
fn artist_partitioned_compilations(tracks: &[Track]) -> HashMap<String, String> {
    let mut candidates: BTreeMap<_, Vec<&Track>> = BTreeMap::new();
    for track in tracks {
        let release = PathBuf::from(release_folder(track));
        let Some(root) = release.parent().and_then(Path::parent) else {
            continue;
        };
        let Some(folder) = release.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let title = crate::model::normalize_text(&track.album).to_lowercase();
        if title.is_empty() {
            continue;
        }
        candidates
            .entry((
                track.source_id.clone(),
                root.to_path_buf(),
                crate::model::normalize_text(folder).to_lowercase(),
                title,
            ))
            .or_default()
            .push(track);
    }
    let mut inferred = HashMap::new();
    for (key, members) in candidates {
        let first = members[0];
        if members.len() < 2
            || first.year.is_none()
            || first.cover_path.is_none()
            || !members.iter().all(|track| {
                let release = PathBuf::from(release_folder(track));
                let directory = release
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str());
                track.album_artist.trim().is_empty()
                    && track.release_id.trim().is_empty()
                    && track.year == first.year
                    && track.cover_path == first.cover_path
                    && directory.is_some_and(|name| {
                        crate::model::normalize_text(name).to_lowercase()
                            == crate::model::normalize_text(&track.artist).to_lowercase()
                    })
            })
        {
            continue;
        }
        let artists = members
            .iter()
            .map(|track| crate::model::normalize_text(&track.artist).to_lowercase())
            .collect::<BTreeSet<_>>();
        if artists.len() < 2 {
            continue;
        }
        let mut discs: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for track in &members {
            discs
                .entry(track.disc_number.max(1))
                .or_default()
                .push(track.track_number);
        }
        let numbered = discs
            .iter_mut()
            .enumerate()
            .all(|(index, (disc, numbers))| {
                numbers.sort_unstable();
                *disc as usize == index + 1
                    && numbers
                        .iter()
                        .enumerate()
                        .all(|(index, number)| *number as usize == index + 1)
            });
        if !numbered {
            continue;
        }
        let album_key = format!("artist-partitioned:{key:?}\u{1f}{:?}", first.year);
        for track in members {
            inferred.insert(track.id.clone(), album_key.clone());
        }
    }
    inferred
}

pub fn group_albums(tracks: &[Track]) -> Vec<Album> {
    let partitioned = artist_partitioned_compilations(tracks);
    let mut folders: BTreeMap<_, Vec<&Track>> = BTreeMap::new();
    for track in tracks {
        folders
            .entry(album_folder_key(track))
            .or_default()
            .push(track);
    }
    let compilations = folders
        .into_iter()
        .filter_map(|(key, members)| {
            let performers = members
                .iter()
                .map(|track| track.artist.to_lowercase())
                .collect::<BTreeSet<_>>();
            let positions = members
                .iter()
                .map(|track| (track.disc_number.max(1), track.track_number))
                .collect::<BTreeSet<_>>();
            let inferred = members.len() >= 2
                && performers.len() >= 2
                && positions.len() == members.len()
                && members.iter().all(|track| {
                    track.track_number > 0
                        && (track.album_artist.is_empty()
                            || track.album_artist.eq_ignore_ascii_case(&track.artist))
                });
            let album_artists = members
                .iter()
                .map(|track| crate::model::normalize_text(&track.album_artist).to_lowercase())
                .filter(|artist| !artist.is_empty())
                .collect::<BTreeSet<_>>();
            let artist = if album_artists.len() == 1 {
                members
                    .iter()
                    .find(|track| !track.album_artist.trim().is_empty())
                    .map_or("Various Artists", |track| track.album_artist.as_str())
            } else {
                "Various Artists"
            };
            (inferred || members.iter().any(|track| track.compilation))
                .then_some((key, artist.to_owned()))
        })
        .collect::<BTreeMap<_, _>>();

    let mut grouped: BTreeMap<String, Album> = BTreeMap::new();
    // Stable ordering also makes representative metadata and cover deterministic.
    let mut ordered = tracks.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|track| {
        (
            track.disc_number.max(1),
            track.track_number,
            &track.path,
            &track.id,
        )
    });
    for track in ordered {
        let folder = album_folder_key(track);
        let artist = if partitioned.contains_key(&track.id) {
            "Various Artists"
        } else if let Some(artist) = compilations.get(&folder) {
            artist.as_str()
        } else if track.album_artist.is_empty() {
            track.display_artist()
        } else {
            &track.album_artist
        };
        let key = if let Some(key) = partitioned.get(&track.id) {
            key.clone()
        } else if track.release_id.trim().is_empty() {
            format!(
                "folder:{folder:?}\u{1f}{}",
                crate::model::normalize_text(artist).to_lowercase()
            )
        } else {
            format!("mbid:{}", track.release_id.trim().to_lowercase())
        };
        let album = grouped.entry(key.clone()).or_insert_with(|| Album {
            key,
            title: track.album.clone(),
            artist: artist.to_owned(),
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
    let mut albums = grouped.into_values().collect::<Vec<_>>();
    albums.sort_by(|a, b| {
        (a.artist.to_lowercase(), a.title.to_lowercase(), &a.key).cmp(&(
            b.artist.to_lowercase(),
            b.title.to_lowercase(),
            &b.key,
        ))
    });
    albums
}

pub fn group_artists(tracks: &[Track]) -> Vec<Artist> {
    let mut grouped: BTreeMap<String, (Vec<String>, BTreeSet<String>)> = BTreeMap::new();
    for track in tracks {
        let artist = if track.artist.is_empty() {
            "Unknown Artist"
        } else {
            &track.artist
        };
        let (track_ids, albums) = grouped.entry(artist.to_owned()).or_default();
        track_ids.push(track.id.clone());
        albums.insert(track.album.to_lowercase());
    }
    grouped
        .into_iter()
        .map(|(name, (track_ids, albums))| Artist {
            album_count: albums.len(),
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
            release_id: String::new(),
            compilation: false,
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
    fn mix_analysis_is_incremental_and_preserves_valid_unknown_bpm() -> Result<()> {
        let mut db = Database::open_memory()?;
        let scanned = (0..20)
            .map(|n| ScannedTrack {
                track: track(&format!("mix-{n}"), n + 1),
                file_size: 100,
                modified_ns: 10,
            })
            .collect::<Vec<_>>();
        db.upsert_scan("s", Path::new("/music"), "Music", &scanned, &[])?;
        assert_eq!(db.mix_analysis_candidates(false)?.len(), 20);
        let analysis = crate::audio::mix::TrackAnalysis {
            bpm: Some(128.0),
            confidence: 0.85,
            energy_rms: 0.12,
            boundaries: None,
        };
        for n in 0..19 {
            assert!(db.save_mix_analysis(&format!("mix-{n}"), analysis, 100, 10)?);
        }
        assert!(db.save_mix_analysis(
            "mix-19",
            crate::audio::mix::TrackAnalysis {
                bpm: None,
                confidence: 0.0,
                energy_rms: 0.01,
                boundaries: None,
            },
            100,
            10
        )?);
        assert_eq!(db.mix_analysis_status()?, (20, 20, 19, 0));
        assert!(db.mix_analysis_candidates(false)?.is_empty());

        let added = (20..25)
            .map(|n| ScannedTrack {
                track: track(&format!("mix-{n}"), n + 1),
                file_size: 100,
                modified_ns: 10,
            })
            .collect::<Vec<_>>();
        db.upsert_scan("s", Path::new("/music"), "Music", &added, &[])?;
        assert_eq!(db.mix_analysis_candidates(false)?.len(), 5);
        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("mix-0", 1),
                file_size: 101,
                modified_ns: 11,
            }],
            &[],
        )?;
        assert_eq!(db.mix_analysis_candidates(false)?.len(), 6);
        assert!(
            !db.save_mix_analysis("mix-0", analysis, 100, 10)?,
            "a stale worker must not overwrite analysis after a rescan"
        );
        assert_eq!(db.mix_analysis_for_path(Path::new("/mix-0.flac"))?, None);
        assert!(db.save_mix_analysis("mix-0", analysis, 101, 11)?);
        assert_eq!(
            db.mix_analysis_for_path(Path::new("/mix-0.flac"))?,
            Some(analysis)
        );
        assert_eq!(db.mix_analysis_candidates(false)?.len(), 5);
        assert_eq!(db.mix_analysis_candidates(true)?.len(), 25);
        Ok(())
    }

    #[test]
    fn mix_analysis_failure_is_cached_until_file_changes() -> Result<()> {
        let mut db = Database::open_memory()?;
        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("unsupported", 1),
                file_size: 42,
                modified_ns: 7,
            }],
            &[],
        )?;
        assert!(db.save_mix_failure("unsupported", "unsupported codec", 42, 7)?);
        assert_eq!(db.mix_analysis_status()?, (1, 1, 0, 1));
        assert!(db.mix_analysis_candidates(false)?.is_empty());
        assert_eq!(
            db.mix_analysis_for_path(Path::new("/unsupported.flac"))?,
            None
        );
        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("unsupported", 1),
                file_size: 43,
                modified_ns: 8,
            }],
            &[],
        )?;
        assert_eq!(db.mix_analysis_candidates(false)?.len(), 1);
        assert!(!db.save_mix_failure("unsupported", "stale failure", 42, 7)?);
        Ok(())
    }

    #[test]
    fn schema_persists_tracks_and_playlists() -> Result<()> {
        let mut db = Database::open_memory()?;
        let scanned = vec![ScannedTrack {
            track: track("one", 1),
            file_size: 1,
            modified_ns: 1,
        }];
        db.upsert_scan("s", Path::new("/music"), "Music", &scanned, &[])?;
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
        db.upsert_scan("s", Path::new("/music"), "Music", &[old], &[])?;
        db.toggle_favorite("old-id")?;
        let playlist = db.create_playlist("Mix")?;
        db.add_to_playlist(playlist, "old-id")?;
        db.save_playback(
            &SavedPlayback {
                queue: Vec::new(),
                current_index: Some(0),
                ..SavedPlayback::default()
            },
            Some(&["old-id".into()]),
        )?;

        let mut moved_track = track("new-id", 1);
        moved_track.title = "old-id".into();
        let moved = ScannedTrack {
            track: moved_track,
            file_size: 42,
            modified_ns: 2,
        };
        let mappings = db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[moved],
            &["old-id".into()],
        )?;

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
    fn stale_gain_is_not_returned_after_the_audio_changes() -> Result<()> {
        let mut db = Database::open_memory()?;
        let original = ScannedTrack {
            track: track("one", 1),
            file_size: 100,
            modified_ns: 1,
        };
        db.upsert_scan("s", Path::new("/music"), "Music", &[original], &[])?;
        db.save_gain(
            "one",
            ReplayGainAnalysis {
                gain_db: -4.0,
                true_peak_db: -1.0,
            },
            100,
            1,
        )?;
        assert!(db.track_gain("one", false)?.is_some());

        let changed = ScannedTrack {
            track: track("one", 1),
            file_size: 101,
            modified_ns: 2,
        };
        db.upsert_scan("s", Path::new("/music"), "Music", &[changed], &[])?;
        assert!(
            db.track_gain("one", false)?.is_none(),
            "a gain calculated for previous file contents must not be reused"
        );
        Ok(())
    }

    #[test]
    fn gain_candidates_after_rescan_only_include_new_or_modified_tracks() -> Result<()> {
        let mut db = Database::open_memory()?;
        let existing = (0..20)
            .map(|index| ScannedTrack {
                track: track(&format!("existing-{index}"), index + 1),
                file_size: 100,
                modified_ns: 1,
            })
            .collect::<Vec<_>>();
        db.upsert_scan("s", Path::new("/music"), "Music", &existing, &[])?;

        let analyzed = ReplayGainAnalysis {
            gain_db: -4.0,
            true_peak_db: -1.0,
        };
        for entry in &existing {
            db.save_gain(
                &entry.track.id,
                analyzed,
                entry.file_size,
                entry.modified_ns,
            )?;
        }
        assert!(db.gain_analysis_candidates(false)?.is_empty());

        let added = (20..25)
            .map(|index| ScannedTrack {
                track: track(&format!("new-{index}"), index + 1),
                file_size: 100,
                modified_ns: 1,
            })
            .collect::<Vec<_>>();
        db.upsert_scan("s", Path::new("/music"), "Music", &added, &[])?;

        let candidates = db.gain_analysis_candidates(false)?;
        assert_eq!(candidates.len(), 5, "existing analyzed tracks stay cached");
        assert!(
            candidates
                .iter()
                .all(|(id, _, _, _)| id.starts_with("new-")),
            "only newly imported tracks should need analysis"
        );

        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("existing-0", 1),
                file_size: 101,
                modified_ns: 2,
            }],
            &[],
        )?;
        let candidates = db.gain_analysis_candidates(false)?;
        assert_eq!(candidates.len(), 6);
        assert!(
            candidates.iter().any(|(id, _, _, _)| id == "existing-0"),
            "a modified track should be reanalyzed"
        );
        Ok(())
    }

    #[test]
    fn replaygain_failures_are_not_retried_on_every_start() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("library.db");
        let mut db = Database::open(&database)?;
        let entry = ScannedTrack {
            track: track("broken-audio", 1),
            file_size: 100,
            modified_ns: 1,
        };
        db.upsert_scan("s", Path::new("/music"), "Music", &[entry], &[])?;
        assert_eq!(db.gain_analysis_candidates(false)?.len(), 1);

        db.save_gain_failure("broken-audio", 100, 1)?;
        assert!(db.gain_analysis_candidates(false)?.is_empty());
        assert!(db.track_gain("broken-audio", false)?.is_none());
        drop(db);

        let mut db = Database::open(&database)?;
        assert!(db.gain_analysis_candidates(false)?.is_empty());
        assert_eq!(db.gain_analysis_candidates(true)?.len(), 1);

        // A stale worker result cannot suppress analysis of a changed file.
        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("broken-audio", 1),
                file_size: 101,
                modified_ns: 2,
            }],
            &[],
        )?;
        db.save_gain_failure("broken-audio", 100, 1)?;
        assert_eq!(db.gain_analysis_candidates(false)?.len(), 1);
        db.save_gain_failure("broken-audio", 101, 2)?;
        assert!(db.gain_analysis_candidates(false)?.is_empty());
        Ok(())
    }

    #[test]
    fn retrying_failed_gain_does_not_reanalyze_cached_successes() -> Result<()> {
        let mut db = Database::open_memory()?;
        let entries = [ScannedTrack {
            track: track("ok", 1),
            file_size: 100,
            modified_ns: 1,
        }, ScannedTrack {
            track: track("failed", 2),
            file_size: 100,
            modified_ns: 1,
        }];
        db.upsert_scan("s", Path::new("/music"), "Music", &entries, &[])?;
        db.save_gain("ok", ReplayGainAnalysis { gain_db: -4.0, true_peak_db: -1.0 }, 100, 1)?;
        db.save_gain_failure("failed", 100, 1)?;
        assert!(db.gain_analysis_candidates(false)?.is_empty());
        let retry = db.gain_analysis_candidates(true)?;
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].0, "failed");
        Ok(())
    }

    #[test]
    fn incremental_gain_survives_restart_and_rejects_late_results() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("library.db");
        let analyzed = ReplayGainAnalysis {
            gain_db: -4.0,
            true_peak_db: -1.0,
        };
        {
            let mut db = Database::open(&database)?;
            db.upsert_scan(
                "s",
                Path::new("/music"),
                "Music",
                &[ScannedTrack {
                    track: track("existing", 1),
                    file_size: 100,
                    modified_ns: 1,
                }],
                &[],
            )?;
            db.save_gain("existing", analyzed, 100, 1)?;
        }
        let mut db = Database::open(&database)?;
        assert!(db.gain_analysis_candidates(false)?.is_empty());
        assert!(db.track_gain("existing", false)?.is_some());
        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("new", 2),
                file_size: 100,
                modified_ns: 1,
            }],
            &[],
        )?;
        let candidates = db.gain_analysis_candidates(false)?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "new");
        // The file changes while its original analysis is still in flight.
        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("new", 2),
                file_size: 101,
                modified_ns: 2,
            }],
            &[],
        )?;
        db.save_gain("new", analyzed, 100, 1)?;
        assert!(db.track_gain("new", false)?.is_none());
        let candidates = db.gain_analysis_candidates(false)?;
        assert_eq!(candidates.len(), 1);
        assert_eq!((candidates[0].2, candidates[0].3), (101, 2));
        db.save_gain("new", analyzed, 101, 2)?;
        drop(db);
        let db = Database::open(&database)?;
        assert!(db.gain_analysis_candidates(false)?.is_empty());
        assert!(db.track_gain("new", false)?.is_some());
        Ok(())
    }

    #[test]
    fn album_gain_waits_for_all_available_tracks_and_recalculates_on_rescan() -> Result<()> {
        let mut db = Database::open_memory()?;
        let first = ScannedTrack {
            track: track("first", 1),
            file_size: 100,
            modified_ns: 1,
        };
        let second = ScannedTrack {
            track: track("second", 2),
            file_size: 100,
            modified_ns: 1,
        };
        db.upsert_scan("s", Path::new("/music"), "Music", &[first, second], &[])?;
        let first_gain = ReplayGainAnalysis {
            gain_db: -4.0,
            true_peak_db: -1.0,
        };
        db.save_gain("first", first_gain, 100, 1)?;

        assert_eq!(
            db.track_gain("first", true)?,
            Some(first_gain),
            "until the album is complete, use valid track gain"
        );
        assert_eq!(
            db.track_gain("second", true)?,
            None,
            "unanalysed tracks cannot borrow partial album gain"
        );

        let second_gain = ReplayGainAnalysis {
            gain_db: -6.0,
            true_peak_db: -0.5,
        };
        db.save_gain("second", second_gain, 100, 1)?;
        let complete = db.track_gain("first", true)?.expect("album gain");
        assert!(complete.gain_db < -4.0 && complete.gain_db > -6.0);
        assert_eq!(complete.true_peak_db, -0.5);

        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[ScannedTrack {
                track: track("third", 3),
                file_size: 100,
                modified_ns: 1,
            }],
            &[],
        )?;
        assert_eq!(
            db.track_gain("first", true)?,
            Some(first_gain),
            "adding a track invalidates the aggregate until it is analysed"
        );

        db.save_gain(
            "third",
            ReplayGainAnalysis {
                gain_db: -2.0,
                true_peak_db: -0.75,
            },
            100,
            1,
        )?;
        let updated = db.track_gain("first", true)?.expect("updated album gain");
        assert!(
            (updated.gain_db - complete.gain_db).abs() > 0.1,
            "album normalization must reflect its new track"
        );
        Ok(())
    }

    fn artist_partitioned_collection() -> Vec<Track> {
        (1..=14)
            .map(|number| {
                let artist = match number {
                    3 | 12 => "Guest A",
                    4 => "Guest B",
                    9 => "Guest C",
                    10 => "Guest D",
                    11 => "Guest E",
                    13 => "Guest F",
                    _ => "Producer",
                };
                let mut item = track(&format!("track-{number:02}"), number);
                item.album = "2nd Collection".into();
                item.artist = artist.into();
                item.album_artist.clear();
                item.year = Some(2007);
                item.disc_number = 0;
                item.path =
                    format!("/music/{artist}/2nd Collection (2007)/{number:02}.flac").into();
                item.cover_path = Some("/cache/shared-cover.jpg".into());
                item
            })
            .collect()
    }

    #[test]
    fn artist_partitioned_collection_with_missing_release_tags_is_one_album() {
        let mut tracks = artist_partitioned_collection();
        tracks.reverse();
        let before = tracks.clone();
        let albums = group_albums(&tracks);
        assert_eq!(
            albums.len(),
            1,
            "one coherent release across artist directories"
        );
        assert_eq!(albums[0].artist, "Various Artists");
        assert_eq!(
            albums[0].track_ids,
            (1..=14)
                .map(|n| format!("track-{n:02}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            tracks, before,
            "grouping must not rewrite track artists or identities"
        );
    }

    #[test]
    fn artist_partitioned_inference_rejects_missing_or_conflicting_evidence() {
        for evidence in [
            "cover",
            "missing-cover",
            "year",
            "number",
            "gap",
            "album-artist",
            "release-id",
            "artist-directory",
        ] {
            let mut tracks = artist_partitioned_collection();
            match evidence {
                "cover" => tracks[2].cover_path = Some("/cache/another-edition.jpg".into()),
                "missing-cover" => tracks[2].cover_path = None,
                "year" => tracks[13].year = None,
                "number" => tracks[2].track_number = 1,
                "gap" => tracks[13].track_number = 15,
                "album-artist" => tracks[2].album_artist = "Explicit curator".into(),
                "release-id" => tracks[2].release_id = "explicit-release".into(),
                "artist-directory" => {
                    tracks[2].path = "/music/Unsorted/2nd Collection (2007)/03.flac".into()
                }
                _ => unreachable!(),
            }
            assert!(
                group_albums(&tracks).len() > 1,
                "must not guess with {evidence} conflict"
            );
        }
    }

    #[test]
    fn artist_partitioned_same_title_editions_keep_their_own_album() {
        let first = artist_partitioned_collection();
        let mut second = first.clone();
        for track in &mut second {
            track.id = format!("other-{}", track.id);
            track.path = track
                .path
                .to_string_lossy()
                .replace("(2007)", "(2007 Remaster)")
                .into();
        }
        let albums = group_albums(&[first, second].concat());
        assert_eq!(albums.len(), 2);
        assert!(albums.iter().all(|album| album.track_ids.len() == 14));
        assert_ne!(albums[0].key, albums[1].key);
    }

    #[test]
    fn compilation_flag_preserves_an_explicit_album_artist() {
        let mut first = track("one", 1);
        first.artist = "Guest".into();
        first.album_artist = "Curator".into();
        first.compilation = true;
        let mut second = first.clone();
        second.id = "two".into();
        second.artist = "Another guest".into();
        second.track_number = 2;
        let albums = group_albums(&[second, first]);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "Curator");
    }

    #[test]
    fn release_ids_override_artist_folder_and_title_but_separate_editions() {
        let mut first = track("first", 2);
        first.release_id = "release-a".into();
        first.path = "/music/a/2.flac".into();
        let mut second = track("second", 1);
        second.release_id = "release-a".into();
        second.album_artist = "Guest credit".into();
        second.path = "/music/b/1.flac".into();
        let mut other = first.clone();
        other.id = "other".into();
        other.release_id = "release-b".into();
        let albums = group_albums(&[first, second, other]);
        assert_eq!(albums.len(), 2);
        assert_eq!(
            albums
                .iter()
                .find(|a| a.key == "mbid:release-a")
                .unwrap()
                .track_ids,
            ["second", "first"]
        );
    }

    #[test]
    fn numbered_disc_folders_group_and_same_year_editions_stay_separate() {
        let mut first = track("disc1", 1);
        first.path = "/music/Release/CD1/01.flac".into();
        first.disc_number = 1;
        let mut second = track("disc2", 1);
        second.path = "/music/Release/CD2/01.flac".into();
        second.disc_number = 2;
        let mut other = first.clone();
        other.id = "other".into();
        other.path = "/music/Other Edition/01.flac".into();
        let albums = group_albums(&[second, other, first]);
        assert_eq!(albums.len(), 2);
        assert!(
            albums
                .iter()
                .any(|album| album.track_ids == ["disc1", "disc2"])
        );
    }

    #[test]
    fn missing_album_artist_compilation_retains_individual_performers() {
        let mut first = track("one", 1);
        first.album_artist.clear();
        first.artist = "One".into();
        let mut second = track("two", 2);
        second.album_artist.clear();
        second.artist = "Two".into();
        let tracks = [second, first];
        let albums = group_albums(&tracks);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "Various Artists");
        assert_eq!(albums[0].track_ids, ["one", "two"]);
        assert_eq!(tracks[0].artist, "Two");
    }

    #[test]
    fn groups_numbered_multi_artist_compilation_without_merging_other_editions() {
        let mut first = track("song-a", 1);
        first.album = "Night Drive".into();
        first.artist = "Performer A".into();
        first.album_artist = "Performer A".into();
        first.path = "/music/Night Drive/01.flac".into();

        let mut second = track("song-b", 2);
        second.album = first.album.clone();
        second.artist = "Performer B".into();
        second.album_artist = "Performer B".into();
        second.path = "/music/Night Drive/02.flac".into();

        // A similarly titled release in a different folder is not part of it.
        let mut another_edition = track("song-c", 1);
        another_edition.album = first.album.clone();
        another_edition.artist = "Performer C".into();
        another_edition.album_artist = "Performer C".into();
        another_edition.path = "/music/Remaster/01.flac".into();

        // The source index is deliberately not ordered by track number.
        let albums = group_albums(&[second, another_edition, first]);
        assert_eq!(albums.len(), 2);
        let compilation = albums
            .iter()
            .find(|album| album.artist == "Various Artists")
            .expect("one inferred compilation");
        assert_eq!(compilation.track_ids, ["song-a", "song-b"]);
        assert_eq!(compilation.available_tracks, 2);
        assert_eq!(
            albums
                .iter()
                .find(|album| album.artist == "Performer C")
                .expect("separate edition")
                .track_ids,
            ["song-c"]
        );
    }

    #[test]
    fn separate_year_editions_do_not_merge_when_the_folders_also_differ() {
        let mut original = track("original", 1);
        original.album = "Same Title".into();
        original.year = Some(1998);
        original.path = "/music/Original/01.flac".into();

        let mut remaster = track("remaster", 1);
        remaster.album = original.album.clone();
        remaster.year = Some(2024);
        remaster.path = "/music/Remaster/01.flac".into();

        let albums = group_albums(&[original, remaster]);
        assert_eq!(albums.len(), 2);
        assert_ne!(albums[0].key, albums[1].key);
        assert_eq!(
            albums
                .iter()
                .map(|album| album.year)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([Some(1998), Some(2024)])
        );
    }

    #[test]
    fn ambiguous_compilation_numbering_is_not_automatically_merged() {
        let mut first = track("first", 1);
        first.artist = "First".into();
        first.album_artist = "First".into();
        first.path = "/music/Unsorted/first.flac".into();

        let mut second = track("second", 1);
        second.artist = "Second".into();
        second.album_artist = "Second".into();
        second.path = "/music/Unsorted/second.flac".into();

        let albums = group_albums(&[first, second]);
        assert_eq!(albums.len(), 2);
        assert!(albums.iter().all(|album| album.artist != "Various Artists"));
    }

    #[test]
    fn album_gain_for_inferred_compilation_includes_every_artist() -> Result<()> {
        let mut db = Database::open_memory()?;
        let mut first = track("first", 1);
        first.artist = "A".into();
        first.album_artist = "A".into();
        first.path = "/music/Mix/01.flac".into();

        let mut second = track("second", 2);
        second.artist = "B".into();
        second.album_artist = "B".into();
        second.path = "/music/Mix/02.flac".into();

        let album = group_albums(&[first.clone(), second.clone()])
            .into_iter()
            .next()
            .expect("album");
        assert_eq!(album.track_ids.len(), 2);
        db.upsert_scan(
            "s",
            Path::new("/music"),
            "Music",
            &[
                ScannedTrack {
                    track: first,
                    file_size: 100,
                    modified_ns: 1,
                },
                ScannedTrack {
                    track: second,
                    file_size: 100,
                    modified_ns: 1,
                },
            ],
            &[],
        )?;
        let first_gain = ReplayGainAnalysis {
            gain_db: -3.0,
            true_peak_db: -1.0,
        };
        db.save_gain("first", first_gain, 100, 1)?;
        assert_eq!(
            db.album_gain_for_tracks("first", &album.track_ids)?,
            Some(first_gain),
            "partial album uses its current track gain"
        );
        db.save_gain(
            "second",
            ReplayGainAnalysis {
                gain_db: -7.0,
                true_peak_db: -0.5,
            },
            100,
            1,
        )?;
        let combined = db
            .album_gain_for_tracks("first", &album.track_ids)?
            .expect("complete compilation gain");
        assert!(combined.gain_db < -3.0 && combined.gain_db > -7.0);
        assert_eq!(combined.true_peak_db, -0.5);
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
            &[],
        )?;
        assert_eq!(db.prune_missing_tracks()?, 1);
        assert!(db.load_tracks()?.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn marking_one_source_unavailable_leaves_other_sources_online() -> Result<()> {
        let mut db = Database::open_memory()?;
        let first = ScannedTrack {
            track: track("one", 1),
            file_size: 1,
            modified_ns: 1,
        };
        let mut second_track = track("two", 2);
        second_track.source_id = "other".into();
        second_track.path = "/other/two.flac".into();
        second_track.relative_path = "two.flac".into();
        let second = ScannedTrack {
            track: second_track,
            file_size: 1,
            modified_ns: 1,
        };

        db.upsert_scan("s", Path::new("/music"), "Music", &[first], &[])?;
        db.upsert_scan("other", Path::new("/other"), "Other", &[second], &[])?;
        assert!(db.mark_source_unavailable_by_root(Path::new("/music"))? > 0);

        let tracks = db.load_tracks()?;
        assert!(
            !tracks
                .iter()
                .find(|track| track.id == "one")
                .unwrap()
                .available
        );
        assert!(
            tracks
                .iter()
                .find(|track| track.id == "two")
                .unwrap()
                .available
        );
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
        db.upsert_scan("s", Path::new("/music"), "Music", &[scanned], &[])?;
        db.upsert_scan("s", Path::new("/music"), "Music", &[], &["one".into()])?;
        assert_eq!(
            db.prune_missing_for_source("s", &BTreeSet::from(["one.flac".into()]))?,
            1
        );
        let tracks = db.load_tracks()?;
        assert_eq!(tracks.len(), 1);
        assert!(tracks[0].available);
        db.upsert_scan("s", Path::new("/music"), "Music", &[], &["one".into()])?;
        assert_eq!(db.prune_missing_for_source("s", &BTreeSet::new())?, 1);
        assert!(db.load_tracks()?.is_empty());
        Ok(())
    }
}
