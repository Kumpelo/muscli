//! Schema migration coverage: `Database::migrate` walks user_version 0
//! through 4 and refuses anything newer.

mod common;

use std::{fs, path::Path};

use muscli::db::Database;
use rusqlite::Connection;

/// The newest schema version this build understands.
const CURRENT_VERSION: i64 = 6;

fn user_version(path: &Path) -> i64 {
    let conn = Connection::open(path).expect("opening the database directly");
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("reading user_version")
}

fn table_names(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).expect("opening the database directly");
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("listing tables");
    stmt.query_map([], |row| row.get::<_, String>(0))
        .expect("querying tables")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collecting table names")
}

fn index_names(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).expect("opening the database directly");
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("listing indexes");
    stmt.query_map([], |row| row.get::<_, String>(0))
        .expect("querying indexes")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collecting index names")
}

/// Build a database at the given historical version from a checked-in schema.
fn seed_from_fixture(path: &Path, fixture: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("creating the database directory");
    }
    let sql = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
    )
    .expect("reading the schema fixture");
    let conn = Connection::open(path).expect("creating the historical database");
    conn.execute_batch(&sql)
        .expect("applying the schema fixture");
}

#[test]
fn a_fresh_database_lands_on_the_current_schema() {
    let fixture = common::Fixture::new();
    let path = fixture.paths().database_file();

    let db = Database::open(&path).expect("creating a fresh database");
    drop(db);

    assert_eq!(user_version(&path), CURRENT_VERSION);

    let tables = table_names(&path);
    for expected in [
        "app_state",
        "history",
        "playlist_tracks",
        "playlists",
        "saved_queue_tracks",
        "saved_queues",
        "smart_playlists",
        "sources",
        "track_stats",
        "tracks",
    ] {
        assert!(
            tables.iter().any(|name| name == expected),
            "missing table {expected}; got {tables:?}"
        );
    }

    let indexes = index_names(&path);
    for expected in [
        "history_recent",
        "tracks_album",
        "tracks_artist",
        "tracks_available",
        "tracks_library_order",
        "tracks_source_scan",
    ] {
        assert!(
            indexes.iter().any(|name| name == expected),
            "missing index {expected}; got {indexes:?}"
        );
    }
}

#[test]
fn opening_twice_is_idempotent() {
    let fixture = common::Fixture::new();
    let path = fixture.paths().database_file();

    drop(Database::open(&path).expect("first open"));
    let tables_after_first = table_names(&path);
    let indexes_after_first = index_names(&path);

    drop(Database::open(&path).expect("second open"));

    assert_eq!(user_version(&path), CURRENT_VERSION);
    assert_eq!(table_names(&path), tables_after_first);
    assert_eq!(index_names(&path), indexes_after_first);
}

#[test]
fn a_version_one_library_upgrades_and_keeps_its_rows() {
    let fixture = common::Fixture::new();
    let path = fixture.paths().database_file();
    seed_from_fixture(&path, "schema_v1.sql");
    assert_eq!(user_version(&path), 1, "the fixture should start at v1");

    // Populate the old schema with a source, a track, and a playlist entry.
    {
        let conn = Connection::open(&path).expect("opening the v1 database");
        conn.execute_batch(
            "INSERT INTO sources(id, root, label) VALUES('src', '/music', 'Music');
             INSERT INTO tracks(id, source_id, relative_path, path, title, artist,
                                album_artist, album, favorite)
             VALUES('trk', 'src', 'a.flac', '/music/a.flac', 'Old Song', 'Old Artist',
                    'Old Artist', 'Old Album', 1);
             INSERT INTO playlists(name) VALUES('Legacy');
             INSERT INTO playlist_tracks(playlist_id, position, track_id)
             VALUES(1, 0, 'trk');",
        )
        .expect("seeding v1 rows");
    }

    let db = Database::open(&path).expect("upgrading the v1 database");

    assert_eq!(user_version(&path), CURRENT_VERSION);

    let tracks = db.load_tracks().expect("loading tracks after the upgrade");
    assert_eq!(tracks.len(), 1, "the upgrade must not drop rows");
    assert_eq!(tracks[0].title, "Old Song");
    assert!(tracks[0].favorite, "user state must survive the upgrade");

    let playlists = db.load_playlists().expect("loading playlists");
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].name, "Legacy");
    assert_eq!(playlists[0].track_ids, ["trk"]);

    // The tables the v1 -> v2 step introduces must now exist and be usable.
    let tables = table_names(&path);
    for expected in [
        "history",
        "saved_queue_tracks",
        "saved_queues",
        "smart_playlists",
        "track_stats",
    ] {
        assert!(
            tables.iter().any(|name| name == expected),
            "the upgrade should have created {expected}"
        );
    }
}

#[test]
fn upgrading_from_version_one_leaves_a_backup_behind() {
    let fixture = common::Fixture::new();
    let path = fixture.paths().database_file();
    seed_from_fixture(&path, "schema_v1.sql");

    drop(Database::open(&path).expect("upgrading the v1 database"));

    let backups: Vec<_> = fs::read_dir(path.parent().unwrap())
        .expect("listing the data directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".backup."))
        .collect();

    assert_eq!(
        backups.len(),
        1,
        "a v1 upgrade should snapshot the database first; found {backups:?}"
    );
}

#[test]
fn a_newer_database_is_refused_instead_of_corrupted() {
    let fixture = common::Fixture::new();
    let path = fixture.paths().database_file();

    drop(Database::open(&path).expect("creating a current database"));
    {
        let conn = Connection::open(&path).expect("opening the database directly");
        conn.pragma_update(None, "user_version", CURRENT_VERSION + 1)
            .expect("faking a newer schema");
    }

    let message = match Database::open(&path) {
        Ok(_) => panic!("a newer schema must be refused, but the open succeeded"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        message.contains("newer than this muscli build"),
        "the refusal should say why; got {message}"
    );
}

#[test]
fn the_default_smart_playlists_are_seeded_once() {
    let fixture = common::Fixture::new();
    let path = fixture.paths().database_file();

    let db = Database::open(&path).expect("first open");
    let first = db.load_smart_playlists().expect("loading smart playlists");
    assert!(
        !first.is_empty(),
        "opening a library should seed the default smart playlists"
    );
    drop(db);

    let db = Database::open(&path).expect("second open");
    let second = db.load_smart_playlists().expect("loading smart playlists");
    assert_eq!(
        first.len(),
        second.len(),
        "reopening must not duplicate the seeded playlists"
    );
}

#[test]
fn seeded_playlists_gain_a_translation_key_on_upgrade() {
    // The five default smart playlists are rows in the database, so their
    // Spanish names outlived any change of interface language. The v5 step
    // attaches a key to each so the displayed name can follow the language,
    // while a playlist the user made keeps the name they chose.
    let fixture = common::Fixture::new();
    let path = fixture.paths().database_file();
    seed_from_fixture(&path, "schema_v1.sql");
    {
        let conn = Connection::open(&path).expect("opening the v1 database");
        conn.execute_batch(
            "CREATE TABLE smart_playlists (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL COLLATE NOCASE UNIQUE,
                match_mode TEXT NOT NULL,
                rules_json TEXT NOT NULL,
                sort_field TEXT NOT NULL DEFAULT 'title',
                descending INTEGER NOT NULL DEFAULT 0,
                item_limit INTEGER
             );
             INSERT INTO smart_playlists(name, match_mode, rules_json)
                VALUES('Metal favorito', 'all', '[]');
             INSERT INTO smart_playlists(name, match_mode, rules_json)
                VALUES('Agregadas recientemente', 'all', '[]');
             INSERT INTO smart_playlists(name, match_mode, rules_json)
                VALUES('No escuchadas', 'all', '[]');
             INSERT INTO smart_playlists(name, match_mode, rules_json)
                VALUES('Más reproducidas', 'all', '[]');
             INSERT INTO smart_playlists(name, match_mode, rules_json)
                VALUES('Más de 8 minutos', 'all', '[]');
             INSERT INTO smart_playlists(name, match_mode, rules_json)
                VALUES('Mis rarezas', 'all', '[]');
             PRAGMA user_version = 4;",
        )
        .expect("seeding a pre-v5 database");
    }

    let db = Database::open(&path).expect("upgrading to v5");
    let playlists = db.load_smart_playlists().expect("loading smart playlists");

    let seeded = playlists
        .iter()
        .filter(|playlist| playlist.preset_key.is_some())
        .collect::<Vec<_>>();
    assert_eq!(
        seeded.len(),
        5,
        "the five translated legacy presets must stay five rows after seeding"
    );
    let keys = seeded
        .iter()
        .filter_map(|playlist| playlist.preset_key.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys.len(),
        5,
        "each built-in preset key must identify exactly one row"
    );
    assert_eq!(
        playlists.len(),
        6,
        "the five presets plus the user's playlist are the only rows"
    );

    let metal = playlists
        .iter()
        .find(|playlist| playlist.name == "Metal favorito")
        .expect("the seeded playlist survives");
    assert_eq!(
        metal.preset_key.as_deref(),
        Some("preset.metal_favorites"),
        "a playlist muscli seeded should be recognised"
    );

    let mine = playlists
        .iter()
        .find(|playlist| playlist.name == "Mis rarezas")
        .expect("the user's playlist survives");
    assert_eq!(
        mine.preset_key, None,
        "a playlist the user made must not be renamed by a language change"
    );
    assert_eq!(mine.display_name(), "Mis rarezas");
}
