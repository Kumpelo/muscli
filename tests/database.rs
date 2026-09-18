//! Database behaviour that is easy to get wrong and was previously untested.

mod common;

use muscli::db::Database;

#[test]
fn an_unknown_track_has_no_gain_rather_than_an_error() {
    // track_gain used to propagate QueryReturnedNoRows, so asking about a track
    // the scanner had just pruned aborted playback instead of simply playing it
    // without ReplayGain.
    let db = Database::open_memory().expect("opening an in-memory database");

    assert_eq!(
        db.track_gain("no-such-track", false)
            .expect("an unknown track is a valid question"),
        None
    );
    assert_eq!(
        db.track_gain("no-such-track", true)
            .expect("the same holds in album mode"),
        None
    );
}

#[test]
fn the_in_memory_database_enforces_foreign_keys_like_the_real_one() {
    // open_memory applied none of open's pragmas, so every unit test ran with
    // foreign keys off while production ran with them on: the suite could not
    // observe a constraint violation a user would hit.
    let db = Database::open_memory().expect("opening an in-memory database");
    let playlist = db.create_playlist("Orphans").expect("creating a playlist");

    assert!(
        db.add_to_playlist(playlist, "no-such-track").is_err(),
        "a playlist entry pointing at a missing track must be rejected"
    );
}

#[test]
fn an_unplugged_source_and_its_tracks_agree_about_availability() {
    let fixture = common::Fixture::new();
    common::write_track(
        &fixture.source().join("a.flac"),
        &common::TrackSpec::new("A"),
    );
    common::write_track(
        &fixture.source().join("b.flac"),
        &common::TrackSpec::new("B"),
    );

    let mut db = Database::open(&fixture.paths().database_file()).expect("opening the database");
    let roots = vec![fixture.source()];
    muscli::library::scan_to_database(&mut db, fixture.paths(), &roots, Default::default())
        .expect("first scan");

    std::fs::remove_dir_all(fixture.source()).expect("unplugging the source");
    muscli::library::scan_to_database(&mut db, fixture.paths(), &roots, Default::default())
        .expect("rescan after unplug");

    let health = db.library_health().expect("reading library health");
    let tracks = db.load_tracks().expect("loading tracks");

    assert_eq!(tracks.len(), 2, "rows survive an unplugged drive");
    assert!(
        tracks.iter().all(|track| !track.available),
        "every track on a missing source must be marked unavailable together \
         with the source itself"
    );
    assert_eq!(health.unavailable_tracks, 2);
    assert!(
        health.sources.iter().all(|(_, available)| !*available),
        "the source itself must be marked unavailable too"
    );
}
