//! Writing ReplayGain results back into audio files.
//!
//! This is the only operation muscli performs that modifies the user's files,
//! so what it writes and what it leaves alone both matter.

mod common;

use lofty::{file::TaggedFileExt, prelude::ItemKey, probe::Probe};
use muscli::{model::ReplayGainAnalysis, replaygain};

use common::{Fixture, TrackSpec, write_track};

fn tag_value(path: &std::path::Path, key: ItemKey) -> Option<String> {
    let tagged = Probe::open(path)
        .expect("opening the file")
        .guess_file_type()
        .expect("guessing the type")
        .read()
        .expect("reading tags");
    tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())?
        .get_string(key)
        .map(str::to_owned)
}

#[test]
fn gain_and_peak_are_written_in_the_units_the_specification_uses() {
    let fixture = Fixture::new();
    let path = fixture.source().join("song.flac");
    write_track(&path, &TrackSpec::new("Song").artist("Someone"));

    replaygain::write_tags(
        &path,
        ReplayGainAnalysis {
            gain_db: -4.5,
            // -6 dBFS is a linear amplitude of about 0.501.
            true_peak_db: -6.0,
        },
    )
    .expect("writing ReplayGain tags");

    assert_eq!(
        tag_value(&path, ItemKey::ReplayGainTrackGain).as_deref(),
        Some("-4.50 dB"),
        "gain is written in decibels, with the unit"
    );
    let peak: f64 = tag_value(&path, ItemKey::ReplayGainTrackPeak)
        .expect("a peak tag")
        .parse()
        .expect("the peak is a bare number");
    assert!(
        (peak - 0.501_187).abs() < 0.000_01,
        "peak is written as linear amplitude, not decibels; got {peak}"
    );
}

#[test]
fn the_rest_of_the_tags_survive() {
    // Writing loudness data must not cost the user their metadata.
    let fixture = Fixture::new();
    let path = fixture.source().join("song.flac");
    write_track(
        &path,
        &TrackSpec::new("Keep This")
            .artist("Keep That")
            .album("And This")
            .genre("Ambient"),
    );

    replaygain::write_tags(
        &path,
        ReplayGainAnalysis {
            gain_db: 1.0,
            true_peak_db: -1.0,
        },
    )
    .expect("writing ReplayGain tags");

    assert_eq!(
        tag_value(&path, ItemKey::TrackTitle).as_deref(),
        Some("Keep This")
    );
    assert_eq!(
        tag_value(&path, ItemKey::TrackArtist).as_deref(),
        Some("Keep That")
    );
    assert_eq!(
        tag_value(&path, ItemKey::AlbumTitle).as_deref(),
        Some("And This")
    );
    assert_eq!(tag_value(&path, ItemKey::Genre).as_deref(), Some("Ambient"));
}

#[test]
fn writing_twice_replaces_rather_than_accumulates() {
    let fixture = Fixture::new();
    let path = fixture.source().join("song.flac");
    write_track(&path, &TrackSpec::new("Song"));

    for gain in [-3.0, -7.25] {
        replaygain::write_tags(
            &path,
            ReplayGainAnalysis {
                gain_db: gain,
                true_peak_db: -2.0,
            },
        )
        .expect("writing ReplayGain tags");
    }

    assert_eq!(
        tag_value(&path, ItemKey::ReplayGainTrackGain).as_deref(),
        Some("-7.25 dB"),
        "a re-analysis should overwrite the old value, not sit beside it"
    );
}
