//! Guards the fixture generator itself. Every other integration test builds
//! on `common::write_track`, so a FLAC writer bug would fail them all
//! confusingly; these assertions fail clearly instead.

mod common;

use lofty::{
    file::{AudioFile, TaggedFileExt},
    prelude::ItemKey,
    probe::Probe,
};

use common::{TrackSpec, png_bytes, write_track};

#[test]
fn generated_flac_is_readable_and_carries_its_tags() {
    let fixture = common::Fixture::new();
    let path = fixture.source().join("song.flac");
    write_track(
        &path,
        &TrackSpec::new("Nightfall")
            .artist("Test Orchestra")
            .album("First Light")
            .genre("Ambient")
            .track_number(7),
    );

    let tagged = Probe::open(&path)
        .expect("opening the generated FLAC")
        .guess_file_type()
        .expect("guessing the file type")
        .read()
        .expect("decoding the generated FLAC");

    let tag = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .expect("a tag");
    assert_eq!(tag.get_string(ItemKey::TrackTitle), Some("Nightfall"));
    assert_eq!(tag.get_string(ItemKey::TrackArtist), Some("Test Orchestra"));
    assert_eq!(tag.get_string(ItemKey::AlbumTitle), Some("First Light"));
    assert_eq!(tag.get_string(ItemKey::Genre), Some("Ambient"));
    assert_eq!(tag.get_string(ItemKey::TrackNumber), Some("7"));
}

#[test]
fn generated_flac_reports_the_requested_duration() {
    let fixture = common::Fixture::new();
    let path = fixture.source().join("long.flac");
    write_track(&path, &TrackSpec::new("Long").millis(1_500));

    let tagged = Probe::open(&path)
        .unwrap()
        .guess_file_type()
        .unwrap()
        .read()
        .unwrap();
    let duration = tagged.properties().duration().as_millis() as i64;
    // The encoder rounds the sample count, so allow a couple of milliseconds.
    assert!(
        (duration - 1_500).abs() <= 5,
        "expected about 1500 ms, got {duration} ms"
    );
}

#[test]
fn a_fixture_with_embedded_art_keeps_the_picture() {
    let fixture = common::Fixture::new();
    let path = fixture.source().join("art.flac");
    write_track(
        &path,
        &TrackSpec::new("Art").cover(png_bytes(64, 64, [10, 120, 200])),
    );

    let tagged = Probe::open(&path)
        .unwrap()
        .guess_file_type()
        .unwrap()
        .read()
        .unwrap();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag()).unwrap();
    assert_eq!(tag.pictures().len(), 1, "the embedded cover should survive");
    assert!(!tag.pictures()[0].data().is_empty());
}

#[test]
fn fixtures_stay_small_enough_to_keep_the_suite_fast() {
    let fixture = common::Fixture::new();
    let path = fixture.source().join("size.flac");
    write_track(&path, &TrackSpec::new("Size"));

    let bytes = std::fs::metadata(&path).unwrap().len();
    assert!(
        bytes < 16_384,
        "a default fixture grew to {bytes} bytes; verbatim subframes should keep it near 8 KB"
    );
}
