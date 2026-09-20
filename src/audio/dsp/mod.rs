//! The processing between the decoder and the device.
//!
//! The stage order is load-bearing:
//!
//! 1. **ReplayGain**, first, so the equaliser works on a levelled signal.
//!    After it, a quiet track would be boosted into a limiter the loud track
//!    it was meant to match never reaches.
//! 2. **Equaliser**.
//! 3. **Limiter**, catching whatever the two pushed over full scale.
//! 4. **Dither**, only when the device takes fixed point.
//!
//! The listener's volume is not here: this chain runs half a second ahead of
//! what is heard, so a change made here would arrive with the buffer. It is
//! applied by the output instead (see [`native::sink`](crate::audio::native::sink)).
//!
//! With everything neutral the chain returns the decoded samples unchanged,
//! bit for bit.

pub mod biquad;
pub mod dither;
pub mod equalizer;
pub mod gain;
pub mod limiter;
pub mod resample;

use dither::Dither;
use equalizer::Equalizer;
use gain::Gain;
use limiter::Limiter;

/// What the chain should do, as the application sees it.
#[derive(Debug, Clone)]
pub struct Settings {
    /// `0.0` to `1.0`. Applied at the output rather than by the chain; this
    /// is the value a stream starts at.
    pub volume: f64,
    /// The current track's adjustment, in decibels.
    pub replay_gain_db: Option<f64>,
    /// Centre frequency and gain for each band.
    pub equalizer: Vec<(u32, f32)>,
    /// Where the limiter holds peaks, at or below `0.0`.
    pub ceiling_db: f64,
    /// Bit depth of the device, or `None` when it takes floats.
    pub output_bits: Option<u32>,
    pub noise_shaping: bool,
    /// Bypass every stage. Gives up ReplayGain and the equaliser.
    pub bit_perfect: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            replay_gain_db: None,
            equalizer: Vec::new(),
            ceiling_db: 0.0,
            output_bits: None,
            noise_shaping: true,
            bit_perfect: false,
        }
    }
}

pub struct Chain {
    sample_rate: u32,
    channels: usize,
    replay_gain: Gain,
    equalizer: Equalizer,
    limiter: Limiter,
    dither: Option<Dither>,
    bit_perfect: bool,
}

impl Chain {
    pub fn new(sample_rate: u32, channels: usize, settings: &Settings) -> Self {
        let channels = channels.max(1);
        let mut replay_gain = Gain::new(sample_rate, 1.0);
        replay_gain.set_db(settings.replay_gain_db);
        replay_gain.settle();

        Self {
            sample_rate,
            channels,
            replay_gain,
            equalizer: Equalizer::new(sample_rate, channels, &settings.equalizer),
            limiter: Limiter::new(sample_rate, channels, settings.ceiling_db),
            dither: settings
                .output_bits
                .map(|bits| Dither::new(bits, channels, settings.noise_shaping)),
            bit_perfect: settings.bit_perfect,
        }
    }

    /// Apply a track's ReplayGain, or remove it with `None`. Slides; at a
    /// track change call [`settle`](Self::settle) so it applies at once.
    pub fn set_replay_gain(&mut self, gain_db: Option<f64>) {
        self.replay_gain.set_db(gain_db);
    }

    /// Rebuild the equaliser. Its state is dropped, which is audible only as
    /// the absence of the old curve's tail.
    pub fn set_equalizer(&mut self, bands: &[(u32, f32)]) {
        self.equalizer = Equalizer::new(self.sample_rate, self.channels, bands);
    }

    /// Hand the samples through untouched, or stop doing so.
    pub fn set_bit_perfect(&mut self, bit_perfect: bool) {
        self.bit_perfect = bit_perfect;
    }

    /// Arrive at every pending target immediately.
    pub fn settle(&mut self) {
        self.replay_gain.settle();
    }

    /// Frames of delay the chain adds, which a reported position has to
    /// subtract. Bit-perfect adds none, because it does nothing.
    pub fn latency_frames(&self) -> usize {
        if self.bit_perfect {
            0
        } else {
            self.limiter.latency_frames()
        }
    }

    /// The largest reduction the limiter has applied since last asked.
    pub fn take_reduction_db(&mut self) -> f64 {
        self.limiter.take_reduction_db()
    }

    /// Drop every filter's memory, for a seek or a track change.
    pub fn reset(&mut self) {
        self.equalizer.reset();
        self.limiter.reset();
        if let Some(dither) = &mut self.dither {
            dither.reset();
        }
    }

    /// Process a block of interleaved samples in place.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        if self.bit_perfect {
            return;
        }
        self.replay_gain.process(interleaved, self.channels);
        self.equalizer.process(interleaved);
        self.limiter.process(interleaved);
        if let Some(dither) = &mut self.dither {
            dither.process(interleaved, self.channels);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio::measure::{db, peak, thd_n, tone, tone_frequency},
        config::EQUALIZER_BANDS,
    };

    const RATE: u32 = 48_000;
    const FRAMES: usize = 1 << 15;
    const CYCLES: usize = 683;

    fn bands(gains: [f32; 8]) -> Vec<(u32, f32)> {
        EQUALIZER_BANDS.iter().copied().zip(gains).collect()
    }

    #[test]
    fn a_neutral_chain_gives_back_what_it_was_given() {
        let mut chain = Chain::new(RATE, 2, &Settings::default());
        let mono = tone(FRAMES, CYCLES, 0.8);
        let original: Vec<f32> = mono.iter().flat_map(|s| [*s, *s]).collect();
        let mut signal = original.clone();
        chain.process(&mut signal);

        let skip = chain.latency_frames() * 2;
        assert_eq!(&signal[skip..], &original[..original.len() - skip]);
    }

    #[test]
    fn bit_perfect_does_not_even_delay() {
        let settings = Settings {
            volume: 0.3,
            replay_gain_db: Some(-8.0),
            equalizer: bands([6.0; 8]),
            bit_perfect: true,
            ..Settings::default()
        };
        let mut chain = Chain::new(RATE, 2, &settings);
        assert_eq!(chain.latency_frames(), 0);

        let original = tone(FRAMES, CYCLES, 0.8);
        let mut signal = original.clone();
        chain.process(&mut signal);
        assert_eq!(signal, original);
    }

    #[test]
    fn replay_gain_runs_before_the_equaliser() {
        // Eight decibels down with a six decibel boost leaves headroom, so
        // the limiter must not engage. The other order would clip it back.
        let settings = Settings {
            replay_gain_db: Some(-8.0),
            equalizer: bands([0.0, 0.0, 0.0, 6.0, 0.0, 0.0, 0.0, 0.0]),
            ..Settings::default()
        };
        let mut chain = Chain::new(RATE, 1, &settings);

        let mut signal = tone(FRAMES, CYCLES, 1.0);
        chain.process(&mut signal);
        assert_eq!(
            chain.take_reduction_db(),
            0.0,
            "the limiter engaged on a signal that never reached full scale"
        );

        let level = db(peak(&signal));
        assert!(
            (level + 2.0).abs() < 0.3,
            "a full-scale tone at -8 dB with +6 dB of lift came out at {level:.2} dB"
        );
    }

    #[test]
    fn the_listeners_volume_is_not_applied_here() {
        // Applied at the output instead. Here it would be applied twice, and
        // a change would have to wait for the buffer to drain.
        let settings = Settings {
            volume: 0.25,
            ..Settings::default()
        };
        let mut chain = Chain::new(RATE, 1, &settings);

        let original = tone(FRAMES, CYCLES, 0.5);
        let mut signal = original.clone();
        chain.process(&mut signal);

        let skip = chain.latency_frames();
        assert_eq!(&signal[skip..], &original[..original.len() - skip]);
    }

    #[test]
    fn the_equaliser_cannot_push_anything_past_the_ceiling() {
        let settings = Settings {
            equalizer: bands([0.0, 0.0, 0.0, 12.0, 0.0, 0.0, 0.0, 0.0]),
            ..Settings::default()
        };
        let mut chain = Chain::new(RATE, 1, &settings);

        let mut signal = tone(FRAMES, CYCLES, 0.9);
        chain.process(&mut signal);
        assert!(peak(&signal) <= 1.0 + 1e-6, "peaked at {}", peak(&signal));
        assert!(
            chain.take_reduction_db() < -1.0,
            "the limiter should have worked"
        );
    }

    #[test]
    fn a_working_chain_adds_nothing_audible() {
        // Everything on, nothing near the ceiling, so what is left is the
        // chain's own arithmetic: -144.8 dB, under the f32 floor itself.
        let settings = Settings {
            volume: 0.7,
            replay_gain_db: Some(-4.0),
            equalizer: bands([3.0, -2.0, 0.0, 4.0, 0.0, -3.0, 2.0, 0.0]),
            ..Settings::default()
        };
        let mut chain = Chain::new(RATE, 1, &settings);

        let mut signal = tone(2 * FRAMES, 2 * CYCLES, 0.5);
        chain.process(&mut signal);

        let measured = thd_n(
            &signal[FRAMES..],
            RATE,
            tone_frequency(RATE, FRAMES, CYCLES),
        );
        assert!(
            measured < -140.0,
            "the chain added distortion at {measured:.1} dB"
        );
    }
}
