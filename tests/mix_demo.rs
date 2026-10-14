//! End-to-end local MIX preview: two decoded files, overlap and WAV output.

mod common;

use std::{fs, path::Path};

use muscli::audio::mix::{MixCurve, MixMode, MixPreset, estimate_bpm, render_preview};
use tempfile::TempDir;

const RATE: u32 = 8_000;

fn write_tone(path: &Path, frequency: f64, millis: usize) {
    let samples = common::sine(RATE, frequency, millis as u32);
    fs::write(path, common::flac_bytes(RATE, &samples)).expect("write fixture");
}

#[test]
fn manual_crossfade_renders_valid_wav_without_touching_the_source_files() {
    let dir = TempDir::new().expect("temp directory");
    let first = dir.path().join("first.flac");
    let second = dir.path().join("second.flac");
    let output = dir.path().join("preview.wav");
    write_tone(&first, 220.0, 3_000);
    write_tone(&second, 440.0, 3_000);
    let first_original = fs::read(&first).expect("read first");
    let second_original = fs::read(&second).expect("read second");

    let preset = MixPreset {
        seconds: 1.0,
        curve: MixCurve::EqualPower,
        ..MixPreset::default()
    };
    let report = render_preview(&first, &second, &output, &preset).expect("render");
    assert!((report.transition_seconds - 1.0).abs() < 0.001);
    assert!((report.output_seconds - 5.0).abs() < 0.001);

    let wav = fs::read(&output).expect("read preview");
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes(wav[40..44].try_into().expect("size")),
        5 * RATE * 2
    );
    assert_eq!(wav.len(), 44 + 5 * RATE as usize * 2);
    assert_eq!(first_original, fs::read(&first).expect("first unchanged"));
    assert_eq!(
        second_original,
        fs::read(&second).expect("second unchanged")
    );

    assert!(render_preview(&first, &second, &output, &preset).is_err());
}

#[test]
fn auto_mix_detects_a_click_track_and_quantizes_to_beats() {
    let dir = TempDir::new().expect("temp directory");
    let first = dir.path().join("beat_a.flac");
    let second = dir.path().join("beat_b.flac");
    let output = dir.path().join("auto.wav");

    let mut clicks = vec![0i16; RATE as usize * 12];
    for start in (0..clicks.len()).step_by(RATE as usize / 2) {
        let end = (start + 120).min(clicks.len());
        for sample in &mut clicks[start..end] {
            *sample = 15_000;
        }
    }
    let fixture = common::flac_bytes(RATE, &clicks);
    fs::write(&first, &fixture).expect("first click");
    fs::write(&second, &fixture).expect("second click");

    let bpm = estimate_bpm(&first)
        .expect("analyze")
        .expect("regular tempo");
    assert!((bpm - 120.0).abs() <= 2.0, "detected {bpm} instead of 120");

    let preset = MixPreset {
        mode: MixMode::Auto,
        beats: 4,
        ..MixPreset::default()
    };
    let report = render_preview(&first, &second, &output, &preset).expect("auto preview");
    assert!((report.transition_seconds - 2.0).abs() <= 0.04);
    assert!(report.tempo_compatible);
}

#[test]
fn mixed_sample_rates_are_rejected_before_writing_output() {
    let dir = TempDir::new().expect("temp directory");
    let first = dir.path().join("first.flac");
    let second = dir.path().join("second.flac");
    let output = dir.path().join("result.wav");
    write_tone(&first, 220.0, 500);
    let samples = common::sine(16_000, 330.0, 500);
    fs::write(&second, common::flac_bytes(16_000, &samples)).expect("second");
    let error = render_preview(&first, &second, &output, &MixPreset::default())
        .expect_err("needs matching sample rates");
    assert!(error.to_string().contains("matching sample rates"));
    assert!(!output.exists(), "a rejected render created an output file");
}
