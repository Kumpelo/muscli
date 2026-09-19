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
fn covers_cached_by_an_older_version_are_reused_not_replaced() {
    // Switching the cache format must not orphan an existing cache. PNGs
    // written by earlier builds decode perfectly well, so an upgrade should
    // keep using them rather than re-encoding every album on the next scan.
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

    assert!(legacy.exists(), "an existing PNG cover must be reused");
    assert_eq!(
        fixture.cover_cache_files(),
        std::slice::from_ref(&legacy),
        "no duplicate should be written alongside it"
    );
    assert_eq!(
        db.load_tracks().expect("loading tracks")[0].cover_path,
        Some(legacy),
        "and the row should point at the cover that was kept"
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
