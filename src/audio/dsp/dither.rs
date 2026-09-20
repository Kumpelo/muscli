//! Dither, used only when the output is fixed point.
//!
//! Rounding to an integer without it makes the error a function of the signal,
//! which is distortion rather than noise. Adding noise first breaks that
//! correlation; noise shaping then moves the hiss out of the audible band.

/// Error-feedback coefficients giving a noise transfer function of
/// `(1 - z^-1)^2`: a double zero at DC, so the noise is pulled down across the
/// audible band and piled up near Nyquist where nobody hears it.
const SHAPING: [f64; 2] = [2.0, -1.0];

/// The feedback is clamped to a few steps. A second-order error feedback loop
/// is only conditionally stable, and a burst of clipping can otherwise set it
/// ringing.
const FEEDBACK_LIMIT: f64 = 4.0;

pub struct Dither {
    /// One quantisation step, as a fraction of full scale.
    step: f64,
    shaped: bool,
    /// Two previous errors per channel, in steps.
    error: Vec<[f64; 2]>,
    state: u64,
}

impl Dither {
    /// A ditherer for `bits`-deep output across `channels`.
    pub fn new(bits: u32, channels: usize, shaped: bool) -> Self {
        let bits = bits.clamp(1, 32);
        Self {
            step: 1.0 / (1u64 << (bits - 1)) as f64,
            shaped,
            error: vec![[0.0; 2]; channels.max(1)],
            // Any odd seed will do; a fixed one keeps the tests repeatable,
            // and dither does not need to be unpredictable, only uncorrelated.
            state: 0x2545_F491_4F6C_DD1D,
        }
    }

    /// Clear the shaper's memory.
    pub fn reset(&mut self) {
        self.error.fill([0.0; 2]);
    }

    /// xorshift64*, for one uniform value in `[-0.5, 0.5)`.
    #[inline]
    fn uniform(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let value = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (value >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    }

    /// Dither and quantise a block in place. Still `f32`, but every value now
    /// sits exactly on an output step, so the conversion cannot round again.
    pub fn process(&mut self, interleaved: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        for frame in interleaved.chunks_mut(channels) {
            for (channel, sample) in frame.iter_mut().enumerate() {
                // Two uniforms summed give a triangular distribution, which
                // makes both the mean and the variance of the error
                // independent of the signal. One uniform leaves the variance
                // modulated, audible as breathing on quiet passages.
                let noise = (self.uniform() + self.uniform()) * self.step;

                let history = &mut self.error[channel];
                let feedback = if self.shaped {
                    (SHAPING[0] * history[0] + SHAPING[1] * history[1])
                        .clamp(-FEEDBACK_LIMIT, FEEDBACK_LIMIT)
                        * self.step
                } else {
                    0.0
                };

                let wanted = *sample as f64 + feedback;
                let quantised = ((wanted + noise) / self.step).round() * self.step;

                // The error fed back is measured against what was wanted
                // before the noise, in steps, so the loop shapes the
                // quantisation error and not the dither.
                history[1] = history[0];
                history[0] = (wanted - quantised) / self.step;
                *sample = quantised as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::measure::{band_level, harmonics_above_noise, tone, tone_frequency};

    const RATE: u32 = 48_000;
    const FRAMES: usize = 1 << 15;
    const BITS: u32 = 8;
    /// 750 Hz at 48 kHz: 64 samples to the cycle. A whole number of samples
    /// per cycle makes the quantisation error exactly periodic, so undithered
    /// it lands entirely on harmonics.
    const CYCLES: usize = 512;

    fn quantised(dither: Option<&mut Dither>) -> Vec<f32> {
        // A quiet tone, six steps tall at eight bits: the region where
        // undithered quantisation is at its ugliest and where dither earns
        // its place.
        let mut signal = tone(FRAMES, CYCLES, 0.05);
        match dither {
            Some(dither) => dither.process(&mut signal, 1),
            None => {
                let step = 1.0 / (1u64 << (BITS - 1)) as f64;
                for sample in &mut signal {
                    *sample = (((*sample as f64) / step).round() * step) as f32;
                }
            }
        }
        signal
    }

    #[test]
    fn rounding_without_dither_produces_harmonics() {
        // The baseline the rest of the module exists to improve on.
        let standing =
            harmonics_above_noise(&quantised(None), RATE, tone_frequency(RATE, FRAMES, CYCLES));
        assert!(
            standing > 40.0,
            "undithered quantisation was expected to distort, and its harmonics \
             stand only {standing:.1} dB above the floor"
        );
    }

    #[test]
    fn dither_turns_the_harmonics_into_noise() {
        // Not quieter harmonics: no harmonics. Anything left has to be
        // indistinguishable from the noise beside it.
        let standing = harmonics_above_noise(
            &quantised(Some(&mut Dither::new(BITS, 1, false))),
            RATE,
            tone_frequency(RATE, FRAMES, CYCLES),
        );
        assert!(
            standing < 3.0,
            "after dithering the harmonics still stand {standing:.1} dB above the floor"
        );
    }

    #[test]
    fn shaping_moves_the_noise_out_of_the_way() {
        // Measured on silence, so what is left is the dither and nothing
        // else: with a tone present its own harmonics would sit in the same
        // bands and the shaping would be invisible.
        let mut flat = vec![0.0f32; FRAMES];
        Dither::new(BITS, 1, false).process(&mut flat, 1);
        let mut shaped = vec![0.0f32; FRAMES];
        Dither::new(BITS, 1, true).process(&mut shaped, 1);

        let flat_low = band_level(&flat, RATE, 100.0, 6_000.0);
        let shaped_low = band_level(&shaped, RATE, 100.0, 6_000.0);
        assert!(
            shaped_low < flat_low - 6.0,
            "shaping left {shaped_low:.1} dB below 6 kHz against {flat_low:.1} dB flat"
        );

        // And it has to have gone somewhere: a shaper that quietened the whole
        // spectrum would be measuring something else.
        let flat_high = band_level(&flat, RATE, 18_000.0, 23_000.0);
        let shaped_high = band_level(&shaped, RATE, 18_000.0, 23_000.0);
        assert!(
            shaped_high > flat_high + 6.0,
            "the noise below 6 kHz went down but nothing appeared above 18 kHz"
        );
    }

    #[test]
    fn every_value_lands_on_a_step() {
        let mut dither = Dither::new(16, 2, true);
        let mut signal: Vec<f32> = tone(4096, 85, 0.7).iter().flat_map(|s| [*s, *s]).collect();
        dither.process(&mut signal, 2);

        let step = 1.0 / 32_768.0;
        for sample in &signal {
            let steps = (*sample as f64) / step;
            assert!(
                (steps - steps.round()).abs() < 1e-3,
                "{sample} is {steps} steps, which is not a whole number"
            );
        }
    }
}
