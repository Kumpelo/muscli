//! Bounded, allocation-free-per-frame DJ transition effects.
//!
//! Designed for the overlapping part of two audio streams. The preview and
//! eventual live mixer can share these primitives without buffering songs.

use std::f32::consts::TAU;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum MixFilter {
    /// AutoDJ chooses a sweep only for rhythmically compatible tracks.
    Auto,
    Off,
    LowPass,
    HighPass,
    Sweep,
}

impl Default for MixFilter {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EffectSettings {
    pub filter: MixFilter,
    pub intensity: f32,
    pub bass_db: f32,
    pub mid_db: f32,
    pub treble_db: f32,
}

#[derive(Debug)]
struct OnePole {
    state: Vec<f32>,
    alpha: f32,
}

impl OnePole {
    fn new(channels: usize) -> Self {
        Self {
            state: vec![0.0; channels],
            alpha: 1.0,
        }
    }

    fn cutoff(&mut self, hz: f32, rate: u32) {
        let cutoff = hz.clamp(15.0, rate as f32 * 0.45);
        self.alpha = 1.0 - (-TAU * cutoff / rate as f32).exp();
    }

    fn lowpass(&mut self, sample: f32, channel: usize) -> f32 {
        let old = self.state[channel];
        let next = old + self.alpha * (sample - old);
        self.state[channel] = next;
        next
    }
}

/// Constant state, O(channels) memory. Coefficients change only every
/// 64 frames, never allocating or calculating exponentials per sample.
pub struct TransitionFx {
    settings: EffectSettings,
    rate: u32,
    a_low: OnePole,
    b_low: OnePole,
    eq_bass: OnePole,
    eq_treble: OnePole,
    eq_gains: [f32; 3],
    eq_preamp: f32,
    frame_index: u64,
}

impl TransitionFx {
    pub fn new(settings: EffectSettings, channels: usize, rate: u32) -> Self {
        let eq_gains =
            [settings.bass_db, settings.mid_db, settings.treble_db].map(|db| 10f32.powf(db / 20.0));
        let max_gain = eq_gains.iter().copied().fold(1.0, f32::max);
        let mut eq_bass = OnePole::new(channels);
        let mut eq_treble = OnePole::new(channels);
        eq_bass.cutoff(250.0, rate);
        eq_treble.cutoff(2_500.0, rate);
        Self {
            settings,
            rate,
            a_low: OnePole::new(channels),
            b_low: OnePole::new(channels),
            eq_bass,
            eq_treble,
            eq_gains,
            eq_preamp: 1.0 / max_gain,
            frame_index: 0,
        }
    }

    pub fn process(&mut self, outgoing: &mut [f32], incoming: &mut [f32], progress: f32) {
        let progress = progress.clamp(0.0, 1.0);
        let wet = self.settings.intensity * (1.0 - progress);
        if self.frame_index.is_multiple_of(64) {
            let (a_hz, b_hz) = match self.settings.filter {
                MixFilter::Auto | MixFilter::Off => (16_000.0, 16_000.0),
                MixFilter::LowPass => (16_000.0, sweep(180.0, 16_000.0, progress)),
                MixFilter::HighPass => (16_000.0, sweep(5_000.0, 30.0, progress)),
                MixFilter::Sweep => (
                    sweep(16_000.0, 150.0, progress),
                    sweep(5_000.0, 30.0, progress),
                ),
            };
            self.a_low.cutoff(a_hz, self.rate);
            self.b_low.cutoff(b_hz, self.rate);
        }
        self.frame_index += 1;

        let have_eq = self.settings.bass_db != 0.0
            || self.settings.mid_db != 0.0
            || self.settings.treble_db != 0.0;

        for (channel, (out, incoming)) in outgoing.iter_mut().zip(incoming.iter_mut()).enumerate() {
            let b_dry = *incoming;

            if have_eq {
                let bass = self.eq_bass.lowpass(b_dry, channel);
                let mid_and_bass = self.eq_treble.lowpass(b_dry, channel);
                let eq = (bass * self.eq_gains[0]
                    + (mid_and_bass - bass) * self.eq_gains[1]
                    + (b_dry - mid_and_bass) * self.eq_gains[2])
                    * self.eq_preamp;
                *incoming = b_dry + (eq - b_dry) * (1.0 - progress);
            }

            match self.settings.filter {
                MixFilter::Off | MixFilter::Auto => {}
                MixFilter::LowPass => {
                    let filtered = self.b_low.lowpass(*incoming, channel);
                    *incoming += (filtered - *incoming) * wet;
                }
                MixFilter::HighPass => {
                    let high = *incoming - self.b_low.lowpass(*incoming, channel);
                    *incoming += (high - *incoming) * wet;
                }
                MixFilter::Sweep => {
                    let filtered = self.a_low.lowpass(*out, channel);
                    *out += (filtered - *out) * self.settings.intensity * progress;
                    let high = *incoming - self.b_low.lowpass(*incoming, channel);
                    *incoming += (high - *incoming) * wet;
                }
            }
        }
    }
}

fn sweep(from: f32, to: f32, position: f32) -> f32 {
    from * (to / from).powf(position)
}

/// A baby-scratch: forward -> reverse -> forward playback of a short
/// already-decoded intro. Playback starts at zero, reverses after 30%,
/// and catches back up to the original timeline at 100%.
pub fn scratch_position(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.30 {
        progress / 0.30 * 0.75
    } else if progress < 0.58 {
        0.75 - (progress - 0.30) / 0.28 * 0.50
    } else {
        0.25 + (progress - 0.58) / 0.42 * 0.75
    }
}

/// Reverse motion is strongest early, but it blends back to the normal
/// timeline at the end so the incoming track resumes without a splice.
pub fn scratch_mix(progress: f32, strength: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    let release = ((1.0 - progress) * 5.0).clamp(0.0, 1.0);
    strength * release
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn response(filter: MixFilter, hz: f32, rate: u32) -> f32 {
        let mut fx = TransitionFx::new(
            EffectSettings {
                filter,
                intensity: 1.0,
                bass_db: 0.0,
                mid_db: 0.0,
                treble_db: 0.0,
            },
            1,
            rate,
        );
        let mut dry = Vec::new();
        let mut wet = Vec::new();
        for n in 0..rate {
            let value = (TAU * hz * n as f32 / rate as f32).sin() * 0.2;
            let mut a = [0.0];
            let mut b = [value];
            fx.process(&mut a, &mut b, 0.0);
            if n >= rate / 2 {
                dry.push(value);
                wet.push(b[0]);
            }
        }
        rms(&wet) / rms(&dry)
    }

    #[test]
    fn low_pass_reduces_treble_not_bass() {
        let rate = 48_000;
        let bass = response(MixFilter::LowPass, 90.0, rate);
        let treble = response(MixFilter::LowPass, 6_000.0, rate);
        assert!(bass > 0.75, "bass {bass}");
        assert!(treble < 0.20, "treble {treble}");
    }

    #[test]
    fn high_pass_reduces_bass_not_treble() {
        let rate = 48_000;
        let bass = response(MixFilter::HighPass, 90.0, rate);
        let treble = response(MixFilter::HighPass, 8_000.0, rate);
        assert!(bass < 0.08, "bass {bass}");
        assert!(treble > 0.55, "treble {treble}");
    }

    #[test]
    fn scratch_reverses_and_finishes_on_clean_audio() {
        assert_eq!(scratch_position(0.0), 0.0);
        assert!((scratch_position(0.30) - 0.75).abs() < 0.00001);
        assert!((scratch_position(0.58) - 0.25).abs() < 0.00001);
        assert!((scratch_position(1.0) - 1.0).abs() < 0.00001);
        assert_eq!(scratch_mix(1.0, 1.0), 0.0);
        assert!(scratch_position(0.5) < scratch_position(0.4));
    }

    #[test]
    fn zero_effects_do_not_modify_input() {
        let mut fx = TransitionFx::new(
            EffectSettings {
                filter: MixFilter::Off,
                intensity: 0.75,
                bass_db: 0.0,
                mid_db: 0.0,
                treble_db: 0.0,
            },
            2,
            48_000,
        );
        let mut a = [0.1, -0.1];
        let mut b = [0.3, -0.3];
        fx.process(&mut a, &mut b, 0.6);
        assert_eq!(a, [0.1, -0.1]);
        assert_eq!(b, [0.3, -0.3]);
    }

    #[test]
    fn eq_is_headroom_limited_and_fades_out() {
        let mut fx = TransitionFx::new(
            EffectSettings {
                filter: MixFilter::Off,
                intensity: 1.0,
                bass_db: 12.0,
                mid_db: -12.0,
                treble_db: 6.0,
            },
            1,
            48_000,
        );
        for index in 0..48_000 {
            let mut a = [0.0];
            let mut b = [(TAU * 90.0 * index as f32 / 48_000.0).sin()];
            fx.process(&mut a, &mut b, index as f32 / 48_000.0);
            assert!(b[0].is_finite() && b[0].abs() <= 1.00001);
        }
    }
}
