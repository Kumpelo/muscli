//! Lightweight, offline transition previews.
//!
//! This module does not modify music files or take over the live audio callback.
//! Both decoders and the WAV encoder stream frames through bounded buffers.

use std::{
    f64::consts::FRAC_PI_2,
    fs::File,
    io::{BufWriter, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use super::{
    decode::Decoder,
    mix_fx::{EffectSettings, MixFilter, TransitionFx, scratch_mix, scratch_position},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum MixMode {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum MixCurve {
    Linear,
    Smooth,
    EqualPower,
}

/// A preset is only a few numeric parameters: no duplicated audio or effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MixPreset {
    pub mode: MixMode,
    pub curve: MixCurve,
    /// Fallback duration for auto mode, or exact duration for manual mode.
    pub seconds: f64,
    /// Auto mode aligns the overlap duration to this many estimated beats.
    pub beats: u8,
    /// Optional manual override for automatic tempo estimation.
    pub bpm_hint: Option<f64>,
    /// Filter applied during the overlap; Auto selects a sweep only for compatible BPM.
    pub filter: MixFilter,
    /// Effect wet amount, from 0 (dry) to 1 (maximum).
    pub intensity: f32,
    /// Incoming-track three-band EQ, faded out before normal playback resumes.
    pub bass_db: f32,
    pub mid_db: f32,
    pub treble_db: f32,
    /// Short bounded forward/reverse vinyl gesture on the incoming track.
    pub scratch: bool,
}

impl Default for MixPreset {
    fn default() -> Self {
        Self {
            mode: MixMode::Manual,
            curve: MixCurve::Smooth,
            seconds: 6.0,
            beats: 8,
            bpm_hint: None,
            filter: MixFilter::Auto,
            intensity: 0.7,
            bass_db: 0.0,
            mid_db: 0.0,
            treble_db: 0.0,
            scratch: false,
        }
    }
}

impl MixPreset {
    pub fn validate(&self) -> Result<()> {
        if !self.seconds.is_finite() || !(0.0..=12.0).contains(&self.seconds) {
            bail!("transition duration must be between 0 and 12 seconds");
        }
        if !(1..=64).contains(&self.beats) {
            bail!("beat count must be between 1 and 64");
        }
        if let Some(bpm) = self.bpm_hint
            && (!bpm.is_finite() || !(50.0..=240.0).contains(&bpm))
        {
            bail!("BPM hint must be between 50 and 240");
        }
        if !self.intensity.is_finite() || !(0.0..=1.0).contains(&self.intensity) {
            bail!("MIX effect intensity must be between 0 and 1");
        }
        for db in [self.bass_db, self.mid_db, self.treble_db] {
            if !db.is_finite() || !(-12.0..=12.0).contains(&db) {
                bail!("MIX EQ band gains must be between -12 and +12 dB");
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct MixReport {
    pub first_bpm: Option<f64>,
    pub second_bpm: Option<f64>,
    pub transition_seconds: f64,
    pub output_seconds: f64,
    pub tempo_compatible: bool,
    pub applied_filter: MixFilter,
    pub scratch_applied: bool,
}

/// Peak-safe gains; equal-power starts with sine/cosine and compensates for
/// coherent peaks, avoiding hard clipping without a real-time limiter.
pub fn blend_gains(curve: MixCurve, position: f64) -> (f32, f32) {
    let t = position.clamp(0.0, 1.0);
    let (a, b) = match curve {
        MixCurve::Linear => (1.0 - t, t),
        MixCurve::Smooth => {
            let s = t * t * (3.0 - 2.0 * t);
            (1.0 - s, s)
        }
        MixCurve::EqualPower => ((t * FRAC_PI_2).cos(), (t * FRAC_PI_2).sin()),
    };
    let peak_safe = (a + b).max(1.0);
    ((a / peak_safe) as f32, (b / peak_safe) as f32)
}

struct FrameReader {
    decoder: Decoder,
    channels: usize,
    pending: Vec<f32>,
    offset: usize,
}

impl FrameReader {
    fn open(path: &Path) -> Result<Self> {
        let decoder = Decoder::open(path)?;
        let channels = usize::from(decoder.spec().channels);
        if channels == 0 {
            bail!("audio file has no channels");
        }
        Ok(Self {
            decoder,
            channels,
            pending: Vec::new(),
            offset: 0,
        })
    }

    fn next_frame(&mut self, frame: &mut [f32]) -> Result<bool> {
        if frame.len() != self.channels {
            bail!("frame channels do not match decoder");
        }
        while self.offset + self.channels > self.pending.len() {
            match self.decoder.next_block()? {
                Some(samples) => {
                    self.pending.clear();
                    self.pending.extend_from_slice(samples);
                    self.offset = 0;
                    if self.pending.is_empty() {
                        continue;
                    }
                }
                None => return Ok(false),
            }
        }
        frame.copy_from_slice(&self.pending[self.offset..self.offset + self.channels]);
        self.offset += self.channels;
        Ok(true)
    }
}

fn frame_count(path: &Path) -> Result<u64> {
    let mut reader = FrameReader::open(path)?;
    let mut frame = vec![0.0; reader.channels];
    let mut count = 0u64;
    while reader.next_frame(&mut frame)? {
        count += 1;
    }
    Ok(count)
}

/// Persisted musical descriptors. A missing BPM is a valid outcome for music
/// without a confidently detectable periodic beat, and is cached as such.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackAnalysis {
    pub bpm: Option<f64>,
    /// Normalized onset autocorrelation (0..1), not a probability.
    pub confidence: f64,
    /// RMS of the first 30 seconds; a quick energy descriptor, not LUFS.
    pub energy_rms: f64,
}

/// Increment when the extractor changes so previously cached rows are
/// correctly reanalyzed after an upgrade.
pub const ANALYSIS_VERSION: i64 = 1;

/// Analyze a bounded excerpt rather than decoding whole songs or keeping PCM.
/// A worker thread owns the decoder; the playback callback never runs this.
pub fn analyze_track(path: &Path) -> Result<TrackAnalysis> {
    let mut reader = FrameReader::open(path)?;
    let rate = reader.decoder.spec().sample_rate;
    let bin_frames = (rate / 100).max(1) as usize;
    let mut frame = vec![0.0; reader.channels];
    let mut bins = Vec::with_capacity(3_001);
    let mut energy = 0.0f64;
    let mut total_energy = 0.0f64;
    let mut frames = 0usize;
    let mut total_frames = 0u64;
    for _ in 0..u64::from(rate) * 30 {
        if !reader.next_frame(&mut frame)? {
            break;
        }
        let power = frame
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / reader.channels as f64;
        energy += power;
        total_energy += power;
        total_frames += 1;
        frames += 1;
        if frames == bin_frames {
            bins.push((energy / frames as f64).sqrt());
            energy = 0.0;
            frames = 0;
        }
    }
    let energy_rms = (total_energy / total_frames.max(1) as f64).sqrt();
    if bins.len() < 800 {
        return Ok(TrackAnalysis {
            bpm: None,
            confidence: 0.0,
            energy_rms,
        });
    }
    let onsets = bins
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).max(0.0))
        .collect::<Vec<_>>();
    let activity: f64 = onsets.iter().map(|v| v * v).sum();
    if activity < 1e-6 {
        return Ok(TrackAnalysis {
            bpm: None,
            confidence: 0.0,
            energy_rms,
        });
    }
    let bins_per_sec = f64::from(rate) / bin_frames as f64;
    let mut best = (0.0, 0.0);
    for bpm in 70..=180 {
        let lag = (60.0 * bins_per_sec / f64::from(bpm)).round() as usize;
        if lag >= onsets.len() {
            continue;
        }
        let mut dot = 0.0;
        let mut left = 0.0;
        let mut right = 0.0;
        for i in lag..onsets.len() {
            let a = onsets[i];
            let b = onsets[i - lag];
            dot += a * b;
            left += a * a;
            right += b * b;
        }
        let score = dot / (left * right).sqrt().max(f64::MIN_POSITIVE);
        if score > best.1 {
            best = (f64::from(bpm), score);
        }
    }
    Ok(TrackAnalysis {
        bpm: (best.1 >= 0.25).then_some(best.0),
        confidence: best.1.clamp(0.0, 1.0),
        energy_rms,
    })
}

/// Backward-compatible BPM helper used by the MIX preview.
pub fn estimate_bpm(path: &Path) -> Result<Option<f64>> {
    Ok(analyze_track(path)?.bpm)
}

struct WaveWriter {
    writer: BufWriter<File>,
    channels: u16,
    rate: u32,
    frames: u64,
}

impl WaveWriter {
    fn create(path: &Path, channels: u16, rate: u32) -> Result<Self> {
        let mut writer = BufWriter::new(
            File::create_new(path)
                .with_context(|| format!("output already exists: {}", path.display()))?,
        );
        writer.write_all(&[0; 44])?;
        Ok(Self {
            writer,
            channels,
            rate,
            frames: 0,
        })
    }

    fn frame(&mut self, samples: &[f32]) -> Result<()> {
        for sample in samples {
            let pcm = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            self.writer.write_all(&pcm.to_le_bytes())?;
        }
        self.frames += 1;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        let data_size = self.frames * u64::from(self.channels) * 2;
        if data_size > u64::from(u32::MAX) - 36 {
            bail!("WAV exceeds the 4 GiB RIFF limit; use shorter tracks");
        }
        let size = data_size as u32;
        let bytes_per_second = self.rate * u32::from(self.channels) * 2;
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(size + 36).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&self.channels.to_le_bytes());
        header.extend_from_slice(&self.rate.to_le_bytes());
        header.extend_from_slice(&bytes_per_second.to_le_bytes());
        header.extend_from_slice(&(self.channels * 2).to_le_bytes());
        header.extend_from_slice(&16u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&size.to_le_bytes());
        self.writer.flush()?;
        self.writer.get_mut().seek(SeekFrom::Start(0))?;
        self.writer.get_mut().write_all(&header)?;
        self.writer.get_mut().sync_all()?;
        Ok(())
    }
}

/// Render an audible overlap to a new WAV. Inputs are never modified.
/// For this preview, both inputs must have the same rate and channel count;
/// real-time rate conversion, key-lock and beat-phase alignment come later.
pub fn render_preview(
    first: &Path,
    second: &Path,
    output: &Path,
    preset: &MixPreset,
) -> Result<MixReport> {
    render_preview_with_analysis(first, second, output, preset, None, None)
}

/// Use cached musical descriptors when available, so previews never repeat
/// expensive onset analysis after the library worker has already completed it.
pub fn render_preview_with_analysis(
    first: &Path,
    second: &Path,
    output: &Path,
    preset: &MixPreset,
    first_cached: Option<TrackAnalysis>,
    second_cached: Option<TrackAnalysis>,
) -> Result<MixReport> {
    preset.validate()?;
    if output.exists() {
        bail!("output already exists: {}", output.display());
    }
    if first == output || second == output {
        bail!("output must not be one of the input tracks");
    }
    let mut a = FrameReader::open(first)?;
    let mut b = FrameReader::open(second)?;
    let spec = a.decoder.spec();
    let other = b.decoder.spec();
    if spec.sample_rate != other.sample_rate || spec.channels != other.channels {
        bail!(
            "demo requires matching sample rates and channels ({} Hz/{} ch vs {} Hz/{} ch)",
            spec.sample_rate,
            spec.channels,
            other.sample_rate,
            other.channels
        );
    }

    let first_frames = frame_count(first)?;
    let second_frames = frame_count(second)?;
    if first_frames == 0 || second_frames == 0 {
        bail!("both tracks need decoded audio");
    }

    let (first_bpm, second_bpm) = if preset.mode == MixMode::Auto {
        (
            first_cached.map_or_else(|| estimate_bpm(first), |analysis| Ok(analysis.bpm))?,
            second_cached.map_or_else(|| estimate_bpm(second), |analysis| Ok(analysis.bpm))?,
        )
    } else {
        (None, None)
    };
    let bpm = preset.bpm_hint.or(first_bpm).or(second_bpm);
    let tempo_compatible = match (first_bpm, second_bpm) {
        (Some(a), Some(b)) => (a - b).abs() / a.max(b) <= 0.08,
        _ => false,
    };
    let seconds = if preset.mode == MixMode::Auto {
        bpm.map_or(preset.seconds, |bpm| f64::from(preset.beats) * 60.0 / bpm)
            .min(12.0)
    } else {
        preset.seconds
    };
    let overlap = (seconds * f64::from(spec.sample_rate)).round() as u64;
    let overlap = overlap
        .min(first_frames.saturating_sub(1))
        .min(second_frames.saturating_sub(1));
    let channels = usize::from(spec.channels);
    let mut writer = WaveWriter::create(output, spec.channels, spec.sample_rate)?;
    let mut left = vec![0.0f32; channels];
    let mut right = vec![0.0f32; channels];
    let mut mixed = vec![0.0f32; channels];
    let filter = match preset.filter {
        MixFilter::Auto if preset.mode == MixMode::Auto && tempo_compatible => MixFilter::Sweep,
        MixFilter::Auto => MixFilter::Off,
        explicit => explicit,
    };
    let mut fx = TransitionFx::new(
        EffectSettings {
            filter,
            intensity: preset.intensity,
            bass_db: preset.bass_db,
            mid_db: preset.mid_db,
            treble_db: preset.treble_db,
        },
        channels,
        spec.sample_rate,
    );

    // A bounded incoming-track intro, not a decoded full song.
    // Consumed once: after the gesture, B resumes from the exact next frame.
    let scratch_frames = if preset.scratch && preset.intensity > 0.0 {
        (u64::from(spec.sample_rate) * 300 / 1_000)
            .min(overlap / 3)
            .min(second_frames.saturating_sub(1))
    } else {
        0
    };
    let mut scratch_intro = vec![0.0f32; scratch_frames as usize * channels];
    for frame in scratch_intro.chunks_exact_mut(channels) {
        if !b.next_frame(frame)? {
            bail!("second decoder ended while preloading scratch gesture");
        }
    }

    for _ in 0..first_frames - overlap {
        if !a.next_frame(&mut left)? {
            bail!("first decoder ended before measured length");
        }
        writer.frame(&left)?;
    }
    for index in 0..overlap {
        if !a.next_frame(&mut left)? {
            bail!("first decoder ended during transition");
        }
        if index < scratch_frames {
            let len = scratch_frames as usize;
            let raw_index = index as usize;
            let progress = if len <= 1 {
                1.0
            } else {
                raw_index as f32 / (len - 1) as f32
            };
            let warped = scratch_position(progress) * (len - 1) as f32;
            let before = warped.floor() as usize;
            let after = (before + 1).min(len - 1);
            let frac = warped - before as f32;
            let amount = scratch_mix(progress, preset.intensity);
            for channel in 0..channels {
                let dry = scratch_intro[raw_index * channels + channel];
                let a = scratch_intro[before * channels + channel];
                let b = scratch_intro[after * channels + channel];
                let scratched = a + (b - a) * frac;
                right[channel] = dry + (scratched - dry) * amount;
            }
        } else if !b.next_frame(&mut right)? {
            bail!("second decoder ended during transition");
        }
        let t = if overlap <= 1 {
            1.0
        } else {
            index as f64 / (overlap - 1) as f64
        };
        fx.process(&mut left, &mut right, t as f32);
        let (a_gain, b_gain) = blend_gains(preset.curve, t);
        for c in 0..channels {
            mixed[c] = left[c] * a_gain + right[c] * b_gain;
        }
        writer.frame(&mixed)?;
    }
    for _ in overlap..second_frames {
        if !b.next_frame(&mut right)? {
            bail!("second decoder ended before measured length");
        }
        writer.frame(&right)?;
    }
    writer.finish()?;
    Ok(MixReport {
        first_bpm,
        second_bpm,
        transition_seconds: overlap as f64 / f64::from(spec.sample_rate),
        output_seconds: (first_frames + second_frames - overlap) as f64
            / f64::from(spec.sample_rate),
        tempo_compatible,
        applied_filter: filter,
        scratch_applied: scratch_frames > 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_curves_begin_and_end_on_the_correct_track() {
        for curve in [MixCurve::Linear, MixCurve::Smooth, MixCurve::EqualPower] {
            assert_eq!(blend_gains(curve, 0.0), (1.0, 0.0));
            let (a, b) = blend_gains(curve, 1.0);
            assert!(a.abs() < 0.000_001 && (b - 1.0).abs() < 0.000_001);
            for n in 0..=1_000 {
                let (a, b) = blend_gains(curve, n as f64 / 1_000.0);
                assert!(a >= 0.0 && b >= 0.0 && a + b <= 1.000_001);
            }
        }
    }

    #[test]
    fn presets_reject_invalid_lengths_and_tempos() {
        let mut preset = MixPreset::default();
        assert!(preset.validate().is_ok());
        preset.seconds = 13.0;
        assert!(preset.validate().is_err());
        preset.seconds = 6.0;
        preset.bpm_hint = Some(f64::NAN);
        assert!(preset.validate().is_err());
    }

    #[test]
    fn presets_are_just_serialized_parameters() {
        let preset = MixPreset {
            mode: MixMode::Auto,
            beats: 16,
            curve: MixCurve::EqualPower,
            ..MixPreset::default()
        };
        let text = toml::to_string(&preset).expect("serialize");
        assert!(text.len() < 300);
        let loaded: MixPreset = toml::from_str(&text).expect("deserialize");
        assert_eq!(loaded.mode, MixMode::Auto);
        assert_eq!(loaded.beats, 16);
    }
}
