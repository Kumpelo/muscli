//! Sample-rate conversion, for when the device will not take the file's rate.
//!
//! The first answer to a rate mismatch is not to have one: the device is
//! opened at the file's rate wherever it can be, and this never runs. When it
//! cannot -- a card fixed at 48 kHz, a shared server that will not budge --
//! the alternative to converting here is letting something else do it, and
//! what usually does is a desktop sound server converting with a filter
//! chosen for latency rather than for the top octave.
//!
//! Conversion happens before the rest of the chain, so the equaliser, the
//! limiter and the dither all run at the rate the device will actually play.
//! In particular the limiter then sees the overshoot that conversion itself
//! can produce, which it would not if it ran first.

use anyhow::{Context, Result, anyhow};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler as _, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

/// Input frames handed to the converter at a time.
const CHUNK: usize = 1024;

/// Length of the interpolation filter.
///
/// 256 taps is where rubato's own guidance starts. `benches/audio.rs` puts
/// the cost at 6.7 ms per second of stereo -- under one per cent of a core --
/// and only tracks the device will not take at their own rate pay it at all.
/// Shorter filters buy back time nobody needs at the price of the top octave.
const TAPS: usize = 256;

pub struct Resampler {
    inner: Async<f32>,
    channels: usize,
    /// Input that did not fill a chunk, interleaved, waiting for the rest.
    pending: Vec<f32>,
    /// Output scratch, sized once so the hot path never allocates.
    scratch: Vec<f32>,
}

impl Resampler {
    /// A converter from `from` to `to`.
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self> {
        let channels = channels.max(1);
        let ratio = f64::from(to) / f64::from(from);
        let parameters = SincInterpolationParameters {
            sinc_len: TAPS,
            // None lets rubato pick the highest cutoff that keeps aliasing
            // under the window's sidelobes, which is what is wanted: the
            // alternative is guessing, and guessing low costs the top end.
            f_cutoff: None,
            oversampling_factor: 256,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };

        let inner = Async::new_sinc(ratio, 1.0, &parameters, CHUNK, channels, FixedAsync::Input)
            .map_err(|error| anyhow!("could not convert {from} Hz to {to} Hz: {error}"))?;
        let scratch = vec![0.0; inner.output_frames_max() * channels];

        Ok(Self {
            inner,
            channels,
            pending: Vec::with_capacity(CHUNK * channels * 2),
            scratch,
        })
    }

    /// Convert interleaved input, appending interleaved output.
    ///
    /// Input that does not fill a chunk is held until the rest arrives, so a
    /// caller may hand over blocks of any size.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<()> {
        self.pending.extend_from_slice(input);

        let wanted = self.inner.input_frames_next() * self.channels;
        let mut taken = 0;
        while self.pending.len() - taken >= wanted {
            let chunk = &self.pending[taken..taken + wanted];
            let source = InterleavedSlice::new(chunk, self.channels, wanted / self.channels)
                .map_err(|error| anyhow!("{error}"))?;

            let capacity = self.scratch.len() / self.channels;
            let mut destination =
                InterleavedSlice::new_mut(&mut self.scratch, self.channels, capacity)
                    .map_err(|error| anyhow!("{error}"))?;

            let (_, produced) = self
                .inner
                .process_into_buffer(&source, &mut destination, None)
                .context("the converter refused a block")?;
            output.extend_from_slice(&self.scratch[..produced * self.channels]);
            taken += wanted;
        }
        self.pending.drain(..taken);
        Ok(())
    }

    /// Forget what is half-converted, for a seek.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.inner.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::measure::{db, spectrum, tone, tone_frequency, windowed_amplitude};

    fn convert(from: u32, to: u32, input: &[f32]) -> Vec<f32> {
        let mut resampler = Resampler::new(from, to, 1).expect("build the converter");
        let mut output = Vec::new();
        resampler.process(input, &mut output).expect("convert");
        output
    }

    #[test]
    fn the_output_is_as_long_as_the_rate_change_implies() {
        let input = tone(1 << 14, 341, 0.5);
        let output = convert(44_100, 48_000, &input);
        let expected = input.len() as f64 * 48_000.0 / 44_100.0;
        // Allow for the tail still inside the filter: what matters is that
        // the length tracks the ratio rather than the buffer.
        assert!(
            (output.len() as f64 - expected).abs() < 2.0 * CHUNK as f64,
            "{} frames in became {} out, expected about {expected:.0}",
            input.len(),
            output.len()
        );
    }

    #[test]
    fn a_tone_keeps_its_pitch_and_nothing_appears_beside_it() {
        // The two ways conversion goes wrong: the tone lands at the wrong
        // frequency, or an image of it folds back down as an alias. Both are
        // checked, the second where the arithmetic says it would appear.
        let (from, to) = (44_100u32, 48_000u32);
        let frames = 1 << 15;
        let cycles = 14_113; // a little over 19 kHz at 44.1
        let output = convert(from, to, &tone(frames, cycles, 0.5));

        let hz = tone_frequency(from, frames, cycles);
        let analysed = 1 << 15;
        let tail = &output[output.len() - analysed..];

        let found = spectrum(tail, to).peak_bin() as f64 * f64::from(to) / analysed as f64;
        assert!(
            (found - hz).abs() < 2.0,
            "a {hz:.1} Hz tone came out at {found:.1} Hz"
        );

        // Upsampling leaves an image at the input rate minus the tone, which
        // at 48 kHz folds back to just under 23 kHz. That is the one place a
        // converter with too short a filter gives itself away.
        let alias = f64::from(to) - (f64::from(from) - hz);
        let wanted = windowed_amplitude(tail, to, hz - 200.0, hz + 200.0);
        let spurious = windowed_amplitude(tail, to, alias - 200.0, alias + 200.0);
        let level = db(spurious / wanted);
        assert!(
            level < -80.0,
            "the image at {alias:.0} Hz came back at {level:.1} dB"
        );
    }

    #[test]
    fn the_top_of_the_band_survives() {
        // Converting up must not quietly roll off what was there. 15 kHz is
        // above where most listeners hear and still has to come through, so
        // the same tone is measured the same way before and after.
        let (from, to) = (44_100u32, 48_000u32);
        let frames = 1 << 15;
        let cycles = 11_147; // just under 15 kHz at 44.1
        let input = tone(frames, cycles, 0.5);
        let output = convert(from, to, &input);

        let hz = tone_frequency(from, frames, cycles);
        let analysed = 1 << 15;
        let before = windowed_amplitude(&input, from, hz - 200.0, hz + 200.0);
        let after = windowed_amplitude(
            &output[output.len() - analysed..],
            to,
            hz - 200.0,
            hz + 200.0,
        );

        let lost = db(after / before);
        assert!(lost.abs() < 0.1, "15 kHz moved by {lost:.2} dB");
    }

    #[test]
    fn blocks_of_any_size_give_the_same_answer() {
        // The engine hands over whatever the decoder produced, which is not a
        // round number; holding the remainder has to be invisible.
        let input = tone(1 << 14, 341, 0.5);
        let whole = convert(44_100, 48_000, &input);

        let mut resampler = Resampler::new(44_100, 48_000, 1).expect("build the converter");
        let mut pieces = Vec::new();
        for block in input.chunks(437) {
            resampler.process(block, &mut pieces).expect("convert");
        }
        assert_eq!(whole, pieces);
    }
}
