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
    muscli::library::scan_to_database(&mut db, fixture.paths(), &roots, &Default::default())
        .expect("first scan");

    std::fs::remove_dir_all(fixture.source()).expect("unplugging the source");
    muscli::library::scan_to_database(&mut db, fixture.paths(), &roots, &Default::default())
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

/// Play `track_id` `times` times, through the same calls playback makes.
fn play(db: &mut Database, track_id: &str, times: u64) {
    for _ in 0..times {
        let history_id = db
            .start_history(track_id)
            .expect("starting a history entry");
        db.update_history(muscli::db::HistoryUpdate {
            history_id,
            track_id,
            listened_delta_ms: 180_000,
            position_ms: 180_000,
            was_counted: false,
            count_now: true,
            completed: true,
        })
        .expect("recording the play");
    }
}

fn scanned_library() -> (common::Fixture, Database) {
    let fixture = common::Fixture::new();
    for (name, artist, title) in [
        ("one", "Alice", "One"),
        ("two", "Alice", "Two"),
        ("three", "Bob", "Three"),
    ] {
        common::write_track(
            &fixture.source().join(format!("{name}.flac")),
            &common::TrackSpec::new(title).artist(artist),
        );
    }
    let mut db = Database::open(&fixture.paths().database_file()).expect("opening the database");
    let roots = vec![fixture.source()];
    muscli::library::scan_to_database(&mut db, fixture.paths(), &roots, &Default::default())
        .expect("scanning");
    (fixture, db)
}

fn track_id(db: &Database, title: &str) -> String {
    db.load_tracks()
        .expect("loading tracks")
        .into_iter()
        .find(|track| track.title == title)
        .expect("the track")
        .id
}

#[test]
fn the_listening_summary_counts_only_what_was_played() {
    let (_fixture, mut db) = scanned_library();
    let one = track_id(&db, "One");
    let two = track_id(&db, "Two");
    play(&mut db, &one, 5);
    play(&mut db, &two, 3);
    // "Three" is never played.

    let summary = db.listening_summary(None).expect("summarising");

    assert_eq!(summary.plays, 8, "a track never played adds nothing");
    assert_eq!(
        summary.top_tracks.len(),
        2,
        "a track with no plays does not belong in a most-played list"
    );
    assert_eq!(summary.top_tracks[0].count, 5, "ordered by play count");
    assert_eq!(summary.top_artists[0].label, "Alice");
    assert_eq!(
        summary.top_artists[0].count, 8,
        "an artist's plays are summed across their tracks"
    );
    assert!(
        summary.listened_ms > 0,
        "listening time is accumulated alongside the count"
    );
}

#[test]
fn a_time_window_excludes_older_plays() {
    let (_fixture, mut db) = scanned_library();
    let one = track_id(&db, "One");
    play(&mut db, &one, 2);

    let future = chrono::Utc::now().timestamp() + 3_600;
    assert_eq!(
        db.listening_summary(Some(future))
            .expect("summarising")
            .plays,
        0,
        "a window that starts in the future contains nothing"
    );

    let past = chrono::Utc::now().timestamp() - 3_600;
    assert_eq!(
        db.listening_summary(Some(past)).expect("summarising").plays,
        2
    );
}
