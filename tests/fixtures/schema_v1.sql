-- The muscli library schema as it stood at user_version 1.
--
-- The v1 -> v2 step in src/db.rs only carries ALTER TABLE / CREATE TABLE
-- statements, so the original shape is no longer described anywhere in the
-- source. It is reconstructed here so the upgrade path stays testable: this is
-- v2 minus everything that migration adds (the five gain/added_at columns on
-- tracks, and the track_stats, history, smart_playlists and saved_queue tables).

CREATE TABLE sources (
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

PRAGMA user_version = 1;
