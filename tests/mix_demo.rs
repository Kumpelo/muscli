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
fn dj_filter_eq_and_scratch_render_different_audio_without_changing_sources() {
    use muscli::audio::mix_fx::MixFilter;

    let dir = TempDir::new().expect("temp directory");
    let first = dir.path().join("first.flac");
    let second = dir.path().join("second.flac");
    write_tone(&first, 220.0, 3_000);
    write_tone(&second, 990.0, 3_000);
    let originals = (fs::read(&first).unwrap(), fs::read(&second).unwrap());

    let clean_file = dir.path().join("clean.wav");
    let clean = MixPreset {
        seconds: 1.0,
        ..MixPreset::default()
    };
    render_preview(&first, &second, &clean_file, &clean).expect("clean crossfade");
    let clean_audio = fs::read(&clean_file).expect("read clean");

    for filter in [MixFilter::LowPass, MixFilter::HighPass, MixFilter::Sweep] {
        let destination = dir.path().join(format!("{filter:?}.wav"));
        let settings = MixPreset {
            seconds: 1.0,
            filter,
            intensity: 1.0,
            bass_db: 6.0,
            mid_db: -4.0,
            treble_db: -6.0,
            scratch: true,
            ..MixPreset::default()
        };
        let report =
            render_preview(&first, &second, &destination, &settings).expect("render effect");
        assert_eq!(report.applied_filter, filter);
        assert!(report.scratch_applied);
        let audio = fs::read(&destination).expect("read effect render");
        assert_eq!(audio.len(), clean_audio.len());
        assert_ne!(&audio[44..], &clean_audio[44..]);
        assert_eq!(&audio[..4], b"RIFF");
        assert_eq!(&audio[8..12], b"WAVE");
    }
    assert_eq!(fs::read(&first).unwrap(), originals.0);
    assert_eq!(fs::read(&second).unwrap(), originals.1);
}

#[test]
fn scratch_can_be_combined_with_eq_or_disabled() {
    let dir = TempDir::new().expect("temp directory");
    let first = dir.path().join("a.flac");
    let second = dir.path().join("b.flac");
    write_tone(&first, 150.0, 1_000);
    write_tone(&second, 800.0, 1_000);

    let bare = dir.path().join("bare.wav");
    let with_scratch = dir.path().join("scratch.wav");
    let settings = MixPreset {
        seconds: 0.6,
        intensity: 1.0,
        bass_db: -5.0,
        treble_db: 4.0,
        ..MixPreset::default()
    };
    render_preview(&first, &second, &bare, &settings).unwrap();
    let output = render_preview(
        &first,
        &second,
        &with_scratch,
        &MixPreset {
            scratch: true,
            ..settings
        },
    )
    .expect("render scratch");
    assert!(output.scratch_applied);
    assert_ne!(fs::read(bare).unwrap(), fs::read(with_scratch).unwrap());
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

#[test]
fn boundary_analysis_measures_silence_at_the_actual_file_ends() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("boundaries.flac");
    let mut samples = vec![0i16; RATE as usize / 2];
    samples.extend(common::sine(RATE, 440.0, 8_000));
    samples.extend(vec![0i16; RATE as usize / 4]);
    fs::write(&path, common::flac_bytes(RATE, &samples)).unwrap();
    let info = muscli::audio::mix::analyze_track(&path).unwrap();
    let boundaries = info.boundaries.expect("decoded endpoints");
    assert!((490..=510).contains(&boundaries.leading_silence_ms));
    assert!((240..=260).contains(&boundaries.trailing_silence_ms));
    assert!(boundaries.intro_rms > 0.01);
    assert!(boundaries.outro_rms > 0.01);
}
