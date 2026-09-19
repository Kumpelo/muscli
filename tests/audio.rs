//! The float decoding path, checked against a stream whose samples are known
//! exactly because the test wrote them.

mod common;

use std::fs;

use muscli::audio::decode::Decoder;
use tempfile::TempDir;

const RATE: u32 = 44_100;

fn write_flac(directory: &TempDir, samples: &[i16]) -> std::path::PathBuf {
    let path = directory.path().join("tone.flac");
    fs::write(&path, common::flac_bytes(RATE, samples)).expect("write the fixture");
    path
}

fn decode_all(path: &std::path::Path) -> Vec<f32> {
    let mut decoder = Decoder::open(path).expect("open the fixture");
    let mut samples = Vec::new();
    while let Some(block) = decoder.next_block().expect("decode a block") {
        samples.extend_from_slice(block);
    }
    samples
}

#[test]
fn the_declared_stream_matches_the_file() {
    let directory = TempDir::new().expect("a temporary directory");
    let path = write_flac(&directory, &common::sine(RATE, 440.0, 200));

    let spec = Decoder::open(&path).expect("open the fixture").spec();
    assert_eq!(spec.sample_rate, RATE);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, Some(16));
}

#[test]
fn decoding_returns_every_sample_unchanged() {
    let directory = TempDir::new().expect("a temporary directory");
    let source = common::sine(RATE, 440.0, 500);
    let path = write_flac(&directory, &source);

    let decoded = decode_all(&path);
    assert_eq!(decoded.len(), source.len());
    for (index, (written, read)) in source.iter().zip(&decoded).enumerate() {
        // Full scale for a 16-bit sample is 32768, not 32767: the format is
        // asymmetric, and dividing by the wrong one would put a gain of
        // 0.00026 dB on everything the player ever decodes.
        let expected = *written as f32 / 32_768.0;
        assert_eq!(
            *read, expected,
            "sample {index} came back as {read} instead of {expected}"
        );
    }
}

#[test]
fn seeking_lands_where_it_was_asked_and_the_samples_follow() {
    let directory = TempDir::new().expect("a temporary directory");
    let source = common::sine(RATE, 440.0, 2_000);
    let path = write_flac(&directory, &source);

    let mut decoder = Decoder::open(&path).expect("open the fixture");
    decoder.seek_ms(1_000).expect("seek a second in");

    let landed = decoder.position_ms();
    assert!(
        landed.abs_diff(1_000) < 100,
        "a seek to 1000 ms landed at {landed} ms"
    );

    let first = decoder.position_frames() as usize;
    let block = decoder
        .next_block()
        .expect("decode after the seek")
        .expect("there is more audio after one second")
        .to_vec();
    for (offset, read) in block.iter().enumerate().take(64) {
        let expected = source[first + offset] as f32 / 32_768.0;
        assert_eq!(
            *read,
            expected,
            "sample {} after the seek is wrong",
            first + offset
        );
    }
}

#[test]
fn a_file_that_is_not_audio_is_refused_rather_than_guessed() {
    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("notes.flac");
    fs::write(&path, b"this is not a FLAC stream").expect("write the decoy");

    assert!(Decoder::open(&path).is_err());
}
