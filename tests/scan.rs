//! End-to-end coverage for the incremental library scanner.
//!
//! The scanner is the most intricate part of muscli and the part most recently
//! optimised, yet it had no test above the unit level. These tests drive the
//! real pipeline — walk, tag read, cover cache, database upsert, prune — against
//! generated fixtures on a throwaway directory tree.

mod common;

use std::{fs, path::PathBuf};

use muscli::{
    db::Database,
    library::{prune_cover_cache, scan_source_with_database, scan_to_database},
};

use common::{Fixture, TrackSpec, png_bytes, write_track};

const CACHE_LIMIT: u64 = 64 * 1024 * 1024;

fn open_db(fixture: &Fixture) -> Database {
    Database::open(&fixture.paths().database_file()).expect("opening the test database")
}

fn scan(fixture: &Fixture, db: &mut Database) -> muscli::library::ScanReport {
    let roots = vec![fixture.source()];
    scan_to_database(db, fixture.paths(), &roots, CACHE_LIMIT).expect("scanning the test source")
}

fn titles(db: &Database) -> Vec<String> {
    let mut titles: Vec<String> = db
        .load_tracks()
        .expect("loading tracks")
        .into_iter()
        .map(|track| track.title)
        .collect();
    titles.sort();
    titles
}

#[test]
fn first_scan_indexes_every_track_with_its_tags() {
    let fixture = Fixture::new();
    let source = fixture.source();
    write_track(
        &source.join("Artist/Album/01.flac"),
        &TrackSpec::new("Opening")
            .artist("Test Orchestra")
            .album("First Light")
            .genre("Ambient")
            .track_number(1),
    );
    write_track(
        &source.join("Artist/Album/02.flac"),
        &TrackSpec::new("Closing")
            .artist("Test Orchestra")
            .album("First Light")
            .genre("Ambient")
            .track_number(2),
    );

    let mut db = open_db(&fixture);
    let report = scan(&fixture, &mut db);

    assert_eq!(report.sources, 1);
    assert_eq!(report.tracks, 2);
    assert_eq!(report.skipped, 0);
    assert!(
        report.errors.is_empty(),
        "unexpected errors: {:?}",
        report.errors
    );

    let tracks = db.load_tracks().expect("loading tracks");
    assert_eq!(tracks.len(), 2);
    let opening = tracks
        .iter()
        .find(|t| t.title == "Opening")
        .expect("the first track");
    assert_eq!(opening.artist, "Test Orchestra");
    assert_eq!(opening.album, "First Light");
    assert_eq!(opening.album_artist, "Test Orchestra");
    assert_eq!(opening.genre, "Ambient");
    assert_eq!(opening.track_number, 1);
    assert_eq!(opening.year, Some(2024));
    assert!(opening.available);
    assert!(!opening.favorite);
    assert!(
        opening.duration_ms > 0,
        "duration should come from the stream"
    );
}

#[test]
fn rescanning_an_unchanged_library_rewrites_nothing() {
    let fixture = Fixture::new();
    write_track(&fixture.source().join("a.flac"), &TrackSpec::new("A"));
    write_track(&fixture.source().join("b.flac"), &TrackSpec::new("B"));

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);

    // A second pass must recognise both files through the fingerprint cache and
    // hand the database nothing to write.
    let second = scan_source_with_database(fixture.paths(), &fixture.source(), &db)
        .expect("rescanning the source");
    assert_eq!(second.track_count, 2, "both files should still be counted");
    assert!(
        second.tracks.is_empty(),
        "unchanged files must not be re-read"
    );
    assert!(second.missing_track_ids.is_empty());
    assert!(second.failed_paths.is_empty());
}

#[test]
fn touching_one_file_reindexes_only_that_file() {
    let fixture = Fixture::new();
    let changed = fixture.source().join("changed.flac");
    write_track(&changed, &TrackSpec::new("Before"));
    write_track(
        &fixture.source().join("stable.flac"),
        &TrackSpec::new("Stable"),
    );

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);

    // Rewrite one file with different tags; its size and mtime both move.
    write_track(&changed, &TrackSpec::new("After").millis(700));

    let second = scan_source_with_database(fixture.paths(), &fixture.source(), &db)
        .expect("rescanning the source");
    assert_eq!(
        second.tracks.len(),
        1,
        "only the rewritten file should be re-read"
    );
    assert_eq!(second.tracks[0].track.title, "After");

    scan(&fixture, &mut db);
    assert_eq!(titles(&db), ["After", "Stable"]);
}

#[test]
fn deleting_a_file_from_a_connected_source_removes_its_row() {
    let fixture = Fixture::new();
    let doomed = fixture.source().join("doomed.flac");
    write_track(&doomed, &TrackSpec::new("Doomed"));
    write_track(&fixture.source().join("kept.flac"), &TrackSpec::new("Kept"));

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);
    assert_eq!(titles(&db), ["Doomed", "Kept"]);

    fs::remove_file(&doomed).expect("removing the fixture");
    scan(&fixture, &mut db);

    // The source is connected, so the row is not merely marked unavailable:
    // it is pruned outright.
    assert_eq!(titles(&db), ["Kept"]);
}

#[test]
fn an_unplugged_source_keeps_its_tracks_for_offline_playlists() {
    let fixture = Fixture::new();
    write_track(
        &fixture.source().join("offline.flac"),
        &TrackSpec::new("Offline"),
    );

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);
    assert_eq!(titles(&db), ["Offline"]);

    // Simulate pulling the drive: the root disappears entirely.
    fs::remove_dir_all(fixture.source()).expect("removing the source");
    let report = scan(&fixture, &mut db);
    assert_eq!(report.sources, 0, "the missing source cannot be scanned");

    let tracks = db.load_tracks().expect("loading tracks");
    assert_eq!(tracks.len(), 1, "rows must survive an unplugged drive");
    assert!(
        !tracks[0].available,
        "but they should be marked unavailable"
    );
}

#[test]
fn moving_a_file_carries_favourites_and_playlists_to_the_new_row() {
    let fixture = Fixture::new();
    let original = fixture.source().join("Album/01.flac");
    write_track(&original, &TrackSpec::new("Travelling"));

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);

    let old_id = db.load_tracks().expect("loading tracks")[0].id.clone();
    assert!(db.toggle_favorite(&old_id).expect("marking a favourite"));
    let playlist = db.create_playlist("Roadtrip").expect("creating a playlist");
    db.add_to_playlist(playlist, &old_id)
        .expect("adding to the playlist");
    db.save_queue("Saved", std::slice::from_ref(&old_id))
        .expect("saving a queue");

    // Move the file to a different relative path inside the same source. The
    // track id is derived from that path, so the row has to be reconciled.
    let moved = fixture.source().join("Renamed/01.flac");
    fs::create_dir_all(moved.parent().unwrap()).expect("creating the new directory");
    fs::rename(&original, &moved).expect("moving the fixture");

    scan(&fixture, &mut db);

    let tracks = db.load_tracks().expect("loading tracks");
    assert_eq!(tracks.len(), 1, "the move must not duplicate the track");
    let new_id = tracks[0].id.clone();
    assert_ne!(new_id, old_id, "a new path means a new id");
    assert!(
        tracks[0].favorite,
        "the favourite flag should follow the file"
    );

    let playlists = db.load_playlists().expect("loading playlists");
    assert_eq!(playlists[0].track_ids, std::slice::from_ref(&new_id));
    let queues = db.load_saved_queues().expect("loading saved queues");
    assert_eq!(queues[0].track_ids, [new_id]);
}

#[test]
fn tracks_sharing_artwork_share_a_single_cached_cover() {
    let fixture = Fixture::new();
    let artwork = png_bytes(96, 96, [200, 40, 40]);
    write_track(
        &fixture.source().join("one.flac"),
        &TrackSpec::new("One").cover(artwork.clone()),
    );
    write_track(
        &fixture.source().join("two.flac"),
        &TrackSpec::new("Two").cover(artwork),
    );

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);

    assert_eq!(
        fixture.cover_cache_files().len(),
        1,
        "identical artwork is keyed by content, so it should be cached once"
    );
    let tracks = db.load_tracks().expect("loading tracks");
    assert_eq!(tracks[0].cover_path, tracks[1].cover_path);
    assert!(tracks[0].cover_path.is_some());
}

#[test]
fn an_external_cover_is_used_when_a_track_has_no_embedded_art() {
    let fixture = Fixture::new();
    let album = fixture.source().join("Album");
    write_track(&album.join("01.flac"), &TrackSpec::new("Bare"));
    fs::write(album.join("cover.png"), png_bytes(80, 80, [20, 160, 90]))
        .expect("writing the external cover");

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);

    let tracks = db.load_tracks().expect("loading tracks");
    assert!(
        tracks[0].cover_path.is_some(),
        "a cover.png beside the track should be picked up"
    );
    assert_eq!(fixture.cover_cache_files().len(), 1);
}

#[test]
fn the_cover_cache_is_pruned_down_to_its_byte_budget() {
    let fixture = Fixture::new();
    for (index, colour) in [[10u8, 20, 30], [40, 50, 60], [70, 80, 90]]
        .into_iter()
        .enumerate()
    {
        write_track(
            &fixture.source().join(format!("track{index}.flac")),
            &TrackSpec::new(&format!("Track {index}")).cover(png_bytes(128, 128, colour)),
        );
    }

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);
    assert_eq!(
        fixture.cover_cache_files().len(),
        3,
        "three distinct covers"
    );

    let budget = fixture.cover_cache_bytes() / 2;
    let removed = prune_cover_cache(&fixture.paths().cover_cache_dir(), budget)
        .expect("pruning the cover cache");

    assert!(!removed.is_empty(), "pruning should have evicted something");
    assert!(
        fixture.cover_cache_bytes() <= budget,
        "the cache should end up within its budget"
    );
}

#[test]
fn a_file_that_cannot_be_parsed_is_reported_but_does_not_abort_the_scan() {
    let fixture = Fixture::new();
    write_track(&fixture.source().join("good.flac"), &TrackSpec::new("Good"));
    fs::write(
        fixture.source().join("broken.flac"),
        b"this is not a FLAC stream",
    )
    .expect("writing the broken fixture");

    let mut db = open_db(&fixture);
    let report = scan(&fixture, &mut db);

    assert_eq!(report.tracks, 1, "the readable track is still indexed");
    assert_eq!(report.skipped, 1, "the broken file is counted as skipped");
    assert_eq!(report.errors.len(), 1, "and reported");
    assert_eq!(titles(&db), ["Good"]);
}

#[test]
fn only_audio_extensions_are_indexed() {
    let fixture = Fixture::new();
    write_track(&fixture.source().join("song.flac"), &TrackSpec::new("Song"));
    fs::write(fixture.source().join("notes.txt"), b"not audio").expect("writing a stray file");
    fs::write(
        fixture.source().join("cover.png"),
        png_bytes(16, 16, [1, 2, 3]),
    )
    .expect("writing a stray image");

    let mut db = open_db(&fixture);
    let report = scan(&fixture, &mut db);

    assert_eq!(report.tracks, 1);
    assert_eq!(
        report.skipped, 0,
        "non-audio files are ignored, not skipped"
    );
    assert_eq!(titles(&db), ["Song"]);
}

#[test]
fn scanning_is_deterministic_across_repeated_cold_runs() {
    // Guards the ordering contract the library relies on: two independent scans
    // of the same tree must produce the same rows in the same order. This is the
    // assertion that a parallel scanner has to keep satisfying.
    let build = || {
        let fixture = Fixture::new();
        for index in 0..12 {
            write_track(
                &fixture
                    .source()
                    .join(format!("Disc{}/{index}.flac", index % 3)),
                &TrackSpec::new(&format!("Track {index:02}"))
                    .artist(&format!("Artist {}", index % 4))
                    .album(&format!("Album {}", index % 3))
                    .track_number(index as u32),
            );
        }
        let mut db = open_db(&fixture);
        scan(&fixture, &mut db);
        let ordered: Vec<(String, String, String)> = db
            .load_tracks()
            .expect("loading tracks")
            .into_iter()
            .map(|track| (track.album_artist, track.album, track.title))
            .collect();
        ordered
    };

    let first = build();
    let second = build();
    assert_eq!(first.len(), 12);
    assert_eq!(
        first, second,
        "scan output must not depend on run-to-run timing"
    );
}

#[test]
fn a_second_source_is_indexed_independently() {
    let fixture = Fixture::new();
    let other = fixture.source().parent().unwrap().join("other-drive");
    write_track(
        &fixture.source().join("first.flac"),
        &TrackSpec::new("First"),
    );
    write_track(&other.join("second.flac"), &TrackSpec::new("Second"));

    let mut db = open_db(&fixture);
    let roots: Vec<PathBuf> = vec![fixture.source(), other];
    let report = scan_to_database(&mut db, fixture.paths(), &roots, CACHE_LIMIT)
        .expect("scanning both sources");

    assert_eq!(report.sources, 2);
    assert_eq!(report.tracks, 2);
    assert_eq!(titles(&db), ["First", "Second"]);
}
