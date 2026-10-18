//! End-to-end coverage for the incremental library scanner.
//!
//! Drives the real pipeline -- walk, tag read, cover cache, database upsert,
//! prune -- against generated fixtures in a throwaway directory tree.

mod common;

use std::{fs, path::PathBuf};

use muscli::{
    db::Database,
    config::{Config, sources_for_mounts},
    library::{ScanOptions, prune_cover_cache, scan_source_with_database, scan_to_database},
};

use common::{Fixture, TrackSpec, png_bytes, write_track};

fn options() -> ScanOptions {
    ScanOptions::default()
}

fn open_db(fixture: &Fixture) -> Database {
    Database::open(&fixture.paths().database_file()).expect("opening the test database")
}

fn scan(fixture: &Fixture, db: &mut Database) -> muscli::library::ScanReport {
    let roots = vec![fixture.source()];
    scan_to_database(db, fixture.paths(), &roots, &options()).expect("scanning the test source")
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
fn adding_a_flac_preserves_existing_volume_analysis_after_real_rescan() {
    let fixture = Fixture::new();
    write_track(&fixture.source().join("a.flac"), &TrackSpec::new("A"));
    write_track(&fixture.source().join("b.flac"), &TrackSpec::new("B"));
    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);
    for (id, _, size, modified) in db.gain_analysis_candidates(false).unwrap() {
        db.save_gain(
            &id,
            muscli::model::ReplayGainAnalysis {
                gain_db: -4.0,
                true_peak_db: -1.0,
            },
            size,
            modified,
        )
        .unwrap();
    }
    drop(db);
    let mut db = open_db(&fixture);
    write_track(&fixture.source().join("new.flac"), &TrackSpec::new("New"));
    let report = scan(&fixture, &mut db);
    assert!(report.errors.is_empty());
    let candidates = db.gain_analysis_candidates(false).unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].1.ends_with("new.flac"));
    let cached = db
        .load_tracks()
        .unwrap()
        .into_iter()
        .filter(|track| track.title != "New");
    for track in cached {
        assert!(db.track_gain(&track.id, false).unwrap().is_some());
    }
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
    let second = scan_source_with_database(fixture.paths(), &fixture.source(), &db, &options())
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

    let second = scan_source_with_database(fixture.paths(), &fixture.source(), &db, &options())
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
fn lucidaq_usb_reconnect_preserves_playlist_favorite_and_skips_ventoy_files() {
    let fixture = Fixture::new();
    let drive = fixture.source().parent().unwrap().join("Ventoy");
    let album = drive.join("Music/LucidaQ/Lechero Mon/Album");
    write_track(&album.join("01.flac"), &TrackSpec::new("First"));
    write_track(&drive.join("Downloads/extra.flac"), &TrackSpec::new("Outside"));
    fs::create_dir_all(drive.join("ISOs")).unwrap();
    fs::write(drive.join("ISOs/example.iso"), b"not music").unwrap();

    let config = Config::default();
    let mut db = open_db(&fixture);
    let sources = sources_for_mounts(&config, std::slice::from_ref(&drive));
    assert_eq!(sources, [drive.join("Music/LucidaQ")]);
    scan_to_database(&mut db, fixture.paths(), &sources, &options()).unwrap();
    let tracks = db.load_tracks().unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "First");
    let id = tracks[0].id.clone();
    db.toggle_favorite(&id).unwrap();
    let playlist = db.create_playlist("Portable").unwrap();
    db.add_to_playlist(playlist, &id).unwrap();

    // The filesystem disappears, but the library keeps the old metadata.
    fs::remove_dir_all(&drive).unwrap();
    let missing_sources = sources_for_mounts(&config, &[]);
    scan_to_database(&mut db, fixture.paths(), &missing_sources, &options()).unwrap();
    let offline = db.load_tracks().unwrap();
    assert_eq!(offline.len(), 1);
    assert!(!offline[0].available);
    assert!(offline[0].favorite);

    // The same volume is plugged in again with its music directory intact.
    write_track(&album.join("01.flac"), &TrackSpec::new("First"));
    let reconnected = sources_for_mounts(&config, std::slice::from_ref(&drive));
    scan_to_database(&mut db, fixture.paths(), &reconnected, &options()).unwrap();
    let after = db.load_tracks().unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, id);
    assert!(after[0].available);
    assert!(after[0].favorite);
    assert_eq!(db.load_playlists().unwrap()[0].track_ids, [id]);
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
    let report = scan_to_database(&mut db, fixture.paths(), &roots, &options())
        .expect("scanning both sources");

    assert_eq!(report.sources, 2);
    assert_eq!(report.tracks, 2);
    assert_eq!(titles(&db), ["First", "Second"]);
}

#[test]
fn pruning_leaves_in_flight_cover_writes_alone() {
    // cache_cover writes to a dot-prefixed scratch file before renaming it into
    // place. Pruning runs concurrently with scanning, and deleting a scratch
    // file mid-write would corrupt the cover that is about to be committed.
    let fixture = Fixture::new();
    let cache = fixture.paths().cover_cache_dir();
    let scratch = cache.join(".abc123.4242.0.tmp");
    let orphan = cache.join("orphan.png");
    fs::write(&scratch, [0u8; 16]).expect("writing a scratch file");
    fs::write(&orphan, [0u8; 16]).expect("writing an unreferenced cover");

    let removed = muscli::library::prune_unreferenced_covers(&cache, &Default::default())
        .expect("pruning unreferenced covers");

    assert_eq!(removed, [orphan], "only the committed orphan is removed");
    assert!(scratch.exists(), "an in-flight write must survive pruning");

    // The byte-budget pass must ignore them too, or it would delete the same
    // file by a different route.
    muscli::library::prune_cover_cache(&cache, 0).expect("pruning to an empty budget");
    assert!(
        scratch.exists(),
        "the budget pass must skip scratch files too"
    );
}

#[test]
fn one_thread_and_four_threads_produce_identical_results() {
    // The contract the parallel scanner has to keep. Results are reassembled in
    // walk order, so thread count must not change what is indexed, in what
    // order, or what the report says.
    let run = |threads: usize| {
        let fixture = Fixture::new();
        // Comfortably over the threshold below which scanning stays serial.
        for index in 0..120 {
            let mut spec = TrackSpec::new(&format!("Track {index:03}"))
                .artist(&format!("Artist {:02}", index % 7))
                .album(&format!("Album {:02}", index % 11))
                .track_number((index % 12) as u32 + 1)
                .millis(48);
            if index % 4 == 0 {
                // Shared artwork across several tracks, to exercise the
                // content-keyed cover cache under contention.
                let shade = (index % 3) as u8;
                spec = spec.cover(png_bytes(64, 64, [shade * 40, 100, 200]));
            }
            write_track(
                &fixture
                    .source()
                    .join(format!("Artist {:02}/{index:03}.flac", index % 7)),
                &spec,
            );
        }
        // One unreadable file, so the failure path is exercised in parallel too.
        fs::write(fixture.source().join("broken.flac"), b"not a FLAC stream")
            .expect("writing the broken fixture");

        let mut db =
            Database::open(&fixture.paths().database_file()).expect("opening the database");
        let roots = vec![fixture.source()];
        let report = scan_to_database(
            &mut db,
            fixture.paths(),
            &roots,
            &ScanOptions {
                threads,
                ..ScanOptions::default()
            },
        )
        .expect("scanning the test source");

        let rows: Vec<(String, String, String, bool)> = db
            .load_tracks()
            .expect("loading tracks")
            .into_iter()
            .map(|track| {
                (
                    track.album_artist,
                    track.album,
                    track.title,
                    track.cover_path.is_some(),
                )
            })
            .collect();
        let covers = fixture.cover_cache_files().len();
        (
            report.tracks,
            report.skipped,
            report.errors.len(),
            rows,
            covers,
        )
    };

    let serial = run(1);
    let parallel = run(4);

    assert_eq!(serial.0, 120, "every readable fixture should be indexed");
    assert_eq!(serial.1, 1, "and the broken one skipped");
    assert_eq!(
        serial, parallel,
        "scanning with four threads must produce exactly what one thread does"
    );
}

#[test]
fn cached_covers_are_stored_compactly() {
    // The cover cache lives inside a fixed byte budget, so the encoding decides
    // how many albums fit before eviction starts. Album art is photographic, so
    // lossless storage buys nothing visible at 512 px.
    let fixture = Fixture::new();
    // A smooth gradient stands in for a photograph. High-frequency noise would
    // be adversarial for JPEG and equally bad for PNG, proving nothing.
    let mut art = image::RgbImage::new(700, 700);
    for (x, y, pixel) in art.enumerate_pixels_mut() {
        *pixel = image::Rgb([
            (x * 255 / 700) as u8,
            (y * 255 / 700) as u8,
            ((x + y) * 255 / 1400) as u8,
        ]);
    }
    let source = image::DynamicImage::ImageRgb8(art);
    let mut encoded = std::io::Cursor::new(Vec::new());
    source
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("encoding the source artwork");

    write_track(
        &fixture.source().join("art.flac"),
        &TrackSpec::new("Art").cover(encoded.into_inner()),
    );

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);

    let covers = fixture.cover_cache_files();
    assert_eq!(covers.len(), 1);
    assert_eq!(
        covers[0].extension().and_then(|e| e.to_str()),
        Some("jpg"),
        "new covers should be cached as JPEG"
    );

    // Compare against the same thumbnail stored losslessly, rather than against
    // an absolute size that would depend on the choice of test image.
    let mut lossless = std::io::Cursor::new(Vec::new());
    source
        .thumbnail(512, 512)
        .write_to(&mut lossless, image::ImageFormat::Png)
        .expect("encoding a lossless thumbnail");
    let png_bytes_len = lossless.into_inner().len() as u64;
    let cached_bytes = fixture.cover_cache_bytes();

    assert!(
        cached_bytes * 3 < png_bytes_len,
        "the cached cover should be far smaller than the lossless equivalent: \
         {cached_bytes} vs {png_bytes_len}"
    );

    // Downscaling still happens, whatever the container.
    let (width, height) = image::image_dimensions(&covers[0]).expect("reading the cached cover");
    assert!(width <= 512 && height <= 512, "got {width}x{height}");
}

#[test]
fn covers_cached_by_an_older_version_are_rebuilt_in_the_current_format() {
    // Versioned names let a scan replace entries written by an older cache
    // implementation instead of trusting them in concurrent reads.
    let fixture = Fixture::new();
    let artwork = png_bytes(96, 96, [12, 34, 56]);
    write_track(
        &fixture.source().join("one.flac"),
        &TrackSpec::new("One").cover(artwork.clone()),
    );

    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);
    // Rebuild the entry as an older build would have left it: the same
    // content-derived key, but genuine PNG bytes under a .png name.
    let cached = fixture.cover_cache_files().remove(0);
    let legacy = cached.with_extension("png");
    fs::write(&legacy, png_bytes(96, 96, [12, 34, 56]))
        .expect("writing a cache entry in the old format");
    fs::remove_file(&cached).expect("removing the new-format entry");

    // Rescan from an empty index, which is what an upgraded build sees: a cover
    // cache full of PNGs and no rows pointing anywhere yet.
    drop(db);
    fs::remove_file(fixture.paths().database_file()).expect("clearing the index");
    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);

    assert!(!legacy.exists(), "the legacy PNG should be retired");
    let covers = fixture.cover_cache_files();
    assert_eq!(covers.len(), 1, "one current cover should replace it");
    let current = &covers[0];
    assert_eq!(
        current.extension().and_then(|value| value.to_str()),
        Some("jpg")
    );
    assert!(
        current
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with("v2-")),
        "the replacement should carry the current cache version"
    );
    assert_eq!(
        db.load_tracks().expect("loading tracks")[0].cover_path,
        Some(current.clone()),
        "the row should point at the rebuilt cover"
    );
}

#[test]
fn a_partial_scan_does_not_declare_other_sources_missing() {
    // The trap in rescanning only what changed: mark_missing_sources marks
    // every source that was not visited as unplugged. Running it on a partial
    // pass would take the rest of the library offline because one folder was
    // touched.
    let fixture = Fixture::new();
    let other = fixture.source().parent().unwrap().join("second-drive");
    write_track(
        &fixture.source().join("first.flac"),
        &TrackSpec::new("First"),
    );
    write_track(&other.join("second.flac"), &TrackSpec::new("Second"));

    let mut db = open_db(&fixture);
    let both: Vec<PathBuf> = vec![fixture.source(), other.clone()];
    scan_to_database(&mut db, fixture.paths(), &both, &options()).expect("full scan");
    assert!(
        db.load_tracks()
            .unwrap()
            .iter()
            .all(|track| track.available),
        "both sources start available"
    );

    // Now rescan only the first source, as a file change there would.
    let partial = ScanOptions {
        full: false,
        ..options()
    };
    scan_to_database(&mut db, fixture.paths(), &[fixture.source()], &partial)
        .expect("partial scan");

    let tracks = db.load_tracks().expect("loading tracks");
    assert_eq!(tracks.len(), 2);
    assert!(
        tracks.iter().all(|track| track.available),
        "a partial scan must leave sources it did not visit alone"
    );

    // A full pass is still allowed to notice a source really has gone.
    fs::remove_dir_all(&other).expect("unplugging the second drive");
    scan_to_database(&mut db, fixture.paths(), &both, &options()).expect("full rescan");
    let tracks = db.load_tracks().expect("loading tracks");
    assert_eq!(
        tracks.iter().filter(|track| !track.available).count(),
        1,
        "the drive that actually went away is marked unavailable"
    );
}

#[test]
fn a_scan_reports_how_far_along_it_is() {
    // A first import of a large drive used to show nothing until the whole
    // source finished. The count must reach the number of files and never
    // exceed it, from however many threads report concurrently.
    let fixture = Fixture::new();
    const FILES: usize = 150;
    for index in 0..FILES {
        write_track(
            &fixture.source().join(format!("{index:03}.flac")),
            &TrackSpec::new(&format!("Track {index:03}")).millis(32),
        );
    }

    let db = Database::open(&fixture.paths().database_file()).expect("opening the database");
    let seen = std::sync::Mutex::new(Vec::new());
    let report = |done: usize, total: usize| {
        seen.lock().expect("progress lock").push((done, total));
    };

    let scan = muscli::library::scan_source_reporting(
        fixture.paths(),
        &fixture.source(),
        &db,
        &ScanOptions {
            threads: 4,
            ..options()
        },
        &report,
    )
    .expect("scanning with progress");

    assert_eq!(scan.track_count, FILES);
    let seen = seen.into_inner().expect("progress lock");
    assert!(!seen.is_empty(), "progress should have been reported");
    assert!(
        seen.iter().all(|(_, total)| *total == FILES),
        "the total must be the file count throughout"
    );
    assert!(
        seen.iter().all(|(done, _)| *done <= FILES),
        "progress must never overshoot: {seen:?}"
    );
    assert_eq!(
        seen.iter().map(|(done, _)| *done).max(),
        Some(FILES),
        "and must reach the end"
    );
}

#[test]
fn more_than_flac_is_indexed() {
    // lofty tags and mpv plays all of these, so the only thing that ever kept
    // them out was the extension filter.
    let fixture = Fixture::new();
    write_track(&fixture.source().join("song.flac"), &TrackSpec::new("Flac"));
    // The generator writes FLAC streams; renaming is enough to prove the
    // filter admits the extension, and lofty identifies the content itself.
    for extension in ["mp3", "ogg", "wav"] {
        let source = fixture.source().join("song.flac");
        let target = fixture.source().join(format!("song.{extension}"));
        fs::copy(&source, &target).expect("copying the fixture");
    }

    let mut db = open_db(&fixture);
    let report = scan(&fixture, &mut db);

    assert_eq!(
        report.tracks + report.skipped,
        4,
        "every audio extension should be offered to the tag reader"
    );
    assert!(
        report.tracks >= 1,
        "and at least the genuine FLAC must be indexed"
    );
}

#[test]
fn the_extension_list_can_be_narrowed() {
    let fixture = Fixture::new();
    write_track(&fixture.source().join("keep.flac"), &TrackSpec::new("Keep"));
    let source = fixture.source().join("keep.flac");
    fs::copy(&source, fixture.source().join("skip.mp3")).expect("copying the fixture");

    let mut db = open_db(&fixture);
    let narrowed = ScanOptions {
        extensions: vec!["flac".into()],
        ..options()
    };
    let report = scan_to_database(&mut db, fixture.paths(), &[fixture.source()], &narrowed)
        .expect("scanning with a narrowed list");

    assert_eq!(report.tracks, 1);
    assert_eq!(titles(&db), ["Keep"]);
}

#[test]
fn portable_lyrics_are_read_without_being_copied_into_the_cache() {
    // The scanner indexes only audio; playback can read a portable sidecar.
    let fixture = Fixture::new();
    write_track(&fixture.source().join("song.flac"), &TrackSpec::new("Song"));
    fs::write(
        fixture.source().join("song.lrc"),
        "[00:01.00]a line beside the audio\n",
    )
    .expect("writing the stray lyrics file");

    let mut db = open_db(&fixture);
    let report = scan(&fixture, &mut db);

    assert_eq!(report.tracks, 1);
    assert_eq!(report.skipped, 0, "the .lrc is ignored, not skipped");
    assert!(
        !fixture.paths().lyrics_dir().join("song.lrc").exists(),
        "nothing may be copied into the lyrics directory by a scan"
    );

    let track = &db.load_tracks().expect("loading tracks")[0];
    assert!(
        muscli::lyrics::load(fixture.paths(), track).is_some(),
        "portable lyrics should be available directly beside the audio"
    );

    // Explicit import can additionally cache a copy on this computer.
    muscli::lyrics::import(
        fixture.paths(),
        &fixture.source().join("song.lrc"),
        &format!("{}.lrc", track.id),
    )
    .expect("importing the lyrics");
    let lyrics = muscli::lyrics::load(fixture.paths(), track).expect("the imported lyrics");
    assert_eq!(lyrics.lines.len(), 1);
}

#[test]
fn release_metadata_refresh_preserves_track_references_and_cached_analysis() {
    let fixture = Fixture::new();
    let mut spec = TrackSpec::new("First").artist("Guest One");
    spec.album_artist = "Various Artists".into();
    spec.release_id = "c1234567-1234-1234-1234-123456789abc".into();
    spec.compilation = true;
    write_track(&fixture.source().join("Compilation/01.flac"), &spec);
    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);
    let original = db.load_tracks().unwrap().remove(0);
    assert_eq!(original.release_id, spec.release_id);
    assert!(original.compilation);
    let playlist = db.create_playlist("Keep").unwrap();
    db.add_to_playlist(playlist, &original.id).unwrap();
    db.toggle_favorite(&original.id).unwrap();
    let history = db.start_history(&original.id).unwrap();
    let (id, _, size, modified) = db.mix_analysis_candidates(false).unwrap().remove(0);
    let analysis = muscli::audio::mix::TrackAnalysis {
        bpm: Some(120.0),
        confidence: 0.8,
        energy_rms: 0.1,
        boundaries: Some(muscli::audio::mix::BoundaryAnalysis {
            intro_rms: 0.1,
            outro_rms: 0.12,
            leading_silence_ms: 0,
            trailing_silence_ms: 10,
        }),
    };
    db.save_mix_analysis(&id, analysis, size, modified).unwrap();
    // A migrated row must have its tags read again even if the file is unchanged.
    let conn = rusqlite::Connection::open(fixture.paths().database_file()).unwrap();
    conn.execute("UPDATE tracks SET release_id='', metadata_version=0", [])
        .unwrap();
    scan(&fixture, &mut db);
    let tracks = db.load_tracks().unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].id, original.id);
    assert_eq!(tracks[0].release_id, spec.release_id);
    assert!(tracks[0].favorite);
    assert_eq!(
        db.load_playlists().unwrap()[0].track_ids.as_slice(),
        std::slice::from_ref(&original.id)
    );
    assert!(history > 0);
    assert_eq!(
        db.mix_analysis_for_path(&original.path).unwrap(),
        Some(analysis)
    );
    assert!(scan(&fixture, &mut db).errors.is_empty());
}

#[test]
fn artist_sorted_compilation_groups_without_retagging_or_rebuilding_user_data() {
    use muscli::{db::group_albums, model::ReplayGainAnalysis};
    let fixture = Fixture::new();
    let cover = png_bytes(64, 64, [40, 90, 140]);
    for (number, artist) in [(1, "Producer"), (2, "Guest"), (3, "Producer")] {
        let mut spec = TrackSpec::new(&format!("Piece {number}"))
            .artist(artist)
            .album("Shared Collection")
            .track_number(number)
            .cover(cover.clone());
        spec.album_artist.clear();
        spec.disc_number = 0;
        write_track(
            &fixture
                .source()
                .join(format!("{artist}/Shared Collection (2024)/{number}.flac")),
            &spec,
        );
    }
    let mut db = open_db(&fixture);
    scan(&fixture, &mut db);
    let playlist = db.create_playlist("Keep all references").unwrap();
    let gain = ReplayGainAnalysis {
        gain_db: -4.0,
        true_peak_db: -0.5,
    };
    for (id, _, size, modified) in db.gain_analysis_candidates(false).unwrap() {
        db.save_gain(&id, gain, size, modified).unwrap();
        db.add_to_playlist(playlist, &id).unwrap();
    }
    let before = db.load_tracks().unwrap();
    db.start_history(&before[0].id).unwrap();
    let history = db.load_history(500).unwrap();
    let playlists = db.load_playlists().unwrap();
    let originals: Vec<_> = before
        .iter()
        .map(|track| fs::read(&track.path).unwrap())
        .collect();
    let albums = group_albums(&before);
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].track_ids.len(), 3);
    assert_eq!(albums[0].artist, "Various Artists");
    let cached =
        scan_source_with_database(fixture.paths(), &fixture.source(), &db, &options()).unwrap();
    assert!(
        cached.tracks.is_empty(),
        "grouping must work with the existing scan cache"
    );
    scan(&fixture, &mut db);
    assert_eq!(
        db.load_tracks().unwrap(),
        before,
        "IDs and individual artists stay intact"
    );
    assert_eq!(db.load_playlists().unwrap(), playlists);
    assert_eq!(db.load_history(500).unwrap(), history);
    assert!(db.gain_analysis_candidates(false).unwrap().is_empty());
    for (track, original) in before.iter().zip(originals) {
        assert_eq!(db.track_gain(&track.id, false).unwrap(), Some(gain));
        assert_eq!(
            fs::read(&track.path).unwrap(),
            original,
            "original tags/audio must not change"
        );
    }
}
