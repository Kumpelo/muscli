//! A measuring bench for the audio path.
//!
//! This exists before the processing it measures, on purpose. Claims about
//! audio quality are cheap to make and easy to get wrong by ear, so every
//! filter, every gain and every dither added later is checked here first: a
//! frequency response that matches the design, and a distortion floor that
//! stays where it belongs.
//!
//! Everything works on coherently sampled tones — a tone whose period divides
//! the analysis length exactly. That is what lets the transform run without a
//! window: the tone lands in a single bin and leaks nothing. A windowed
//! measurement would put its own skirt at roughly -90 dB, above the distortion
//! this bench is meant to catch.

use std::f64::consts::TAU;

/// A linear amplitude ratio in decibels.
///
/// Zero is clamped to the smallest positive `f64` rather than returning
/// negative infinity, so an exactly silent measurement still compares.
pub fn db(ratio: f64) -> f64 {
    20.0 * ratio.abs().max(f64::MIN_POSITIVE).log10()
}

/// The largest absolute sample.
pub fn peak(signal: &[f32]) -> f64 {
    signal
        .iter()
        .fold(0.0, |worst, s| f64::max(worst, s.abs() as f64))
}

/// Root mean square level.
pub fn rms(signal: &[f32]) -> f64 {
    if signal.is_empty() {
        return 0.0;
    }
    let sum: f64 = signal.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (sum / signal.len() as f64).sqrt()
}

/// The frequency of a tone of `cycles` periods spread over `frames` samples.
pub fn tone_frequency(sample_rate: u32, frames: usize, cycles: usize) -> f64 {
    cycles as f64 * sample_rate as f64 / frames as f64
}

/// The cycle count whose frequency lands nearest `hz`.
///
/// The result is clamped away from zero and from Nyquist, both of which are
/// degenerate: a tone at either is not a tone.
pub fn cycles_for(sample_rate: u32, frames: usize, hz: f64) -> usize {
    let exact = hz * frames as f64 / sample_rate as f64;
    (exact.round() as usize).clamp(1, frames / 2 - 1)
}

/// A sine of exactly `cycles` periods over `frames` samples.
///
/// Because the period divides the buffer, the tone can be analysed without a
/// window and repeats seamlessly if the buffer is played back to back.
pub fn tone(frames: usize, cycles: usize, amplitude: f64) -> Vec<f32> {
    (0..frames)
        .map(|n| {
            let phase = TAU * cycles as f64 * n as f64 / frames as f64;
            (amplitude * phase.sin()) as f32
        })
        .collect()
}

/// In-place radix-2 Cooley-Tukey transform.
///
/// Written out rather than pulled in: the bench needs one transform of a power
/// of two, and a dependency that exists only for tests is a dependency the
/// release binary still has to resolve.
pub fn fft(real: &mut [f64], imaginary: &mut [f64]) {
    let n = real.len();
    assert_eq!(n, imaginary.len(), "the two halves must be the same length");
    assert!(n.is_power_of_two(), "the length must be a power of two");
    if n < 2 {
        return;
    }

    // Bit-reversal permutation.
    let mut target = 0usize;
    for source in 1..n {
        let mut bit = n >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target |= bit;
        if source < target {
            real.swap(source, target);
            imaginary.swap(source, target);
        }
    }

    let mut span = 2;
    while span <= n {
        let angle = -TAU / span as f64;
        let (step_sin, step_cos) = angle.sin_cos();
        for start in (0..n).step_by(span) {
            let (mut twiddle_real, mut twiddle_imaginary) = (1.0f64, 0.0f64);
            for offset in 0..span / 2 {
                let lower = start + offset;
                let upper = lower + span / 2;
                let product_real =
                    real[upper] * twiddle_real - imaginary[upper] * twiddle_imaginary;
                let product_imaginary =
                    real[upper] * twiddle_imaginary + imaginary[upper] * twiddle_real;
                real[upper] = real[lower] - product_real;
                imaginary[upper] = imaginary[lower] - product_imaginary;
                real[lower] += product_real;
                imaginary[lower] += product_imaginary;
                let next_real = twiddle_real * step_cos - twiddle_imaginary * step_sin;
                twiddle_imaginary = twiddle_real * step_sin + twiddle_imaginary * step_cos;
                twiddle_real = next_real;
            }
        }
        span <<= 1;
    }
}

/// The one-sided amplitude spectrum of a real signal.
///
/// Magnitudes are scaled so that a sine of amplitude `a` reads back as `a` in
/// its own bin, which makes every number here directly comparable to the
/// signal that produced it.
#[derive(Debug, Clone)]
pub struct Spectrum {
    sample_rate: u32,
    frames: usize,
    magnitude: Vec<f64>,
}

impl Spectrum {
    /// Amplitude per bin, from DC up to and including Nyquist.
    pub fn magnitudes(&self) -> &[f64] {
        &self.magnitude
    }

    /// The frequency a bin stands for.
    pub fn frequency(&self, bin: usize) -> f64 {
        bin as f64 * self.sample_rate as f64 / self.frames as f64
    }

    /// The bin a frequency falls in.
    pub fn bin_for(&self, hz: f64) -> usize {
        let bin = (hz * self.frames as f64 / self.sample_rate as f64).round() as usize;
        bin.min(self.magnitude.len() - 1)
    }

    /// The loudest bin, ignoring DC.
    pub fn peak_bin(&self) -> usize {
        self.magnitude
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(bin, _)| bin)
            .unwrap_or(0)
    }
}

/// Transform a signal whose length is a power of two.
pub fn spectrum(signal: &[f32], sample_rate: u32) -> Spectrum {
    let frames = signal.len();
    assert!(
        frames.is_power_of_two(),
        "the analysis length must be a power of two"
    );

    let mut real: Vec<f64> = signal.iter().map(|s| *s as f64).collect();
    let mut imaginary = vec![0.0; frames];
    fft(&mut real, &mut imaginary);

    // A real sine of amplitude a splits its energy between the positive and
    // negative frequency bins, so the positive one holds a*n/2; doubling and
    // dividing by n reads the amplitude straight off. DC and Nyquist have no
    // mirror image and take the undoubled scaling.
    let magnitude = (0..=frames / 2)
        .map(|bin| {
            let size = real[bin].hypot(imaginary[bin]);
            let scale = if bin == 0 || bin == frames / 2 {
                1.0
            } else {
                2.0
            };
            scale * size / frames as f64
        })
        .collect();

    Spectrum {
        sample_rate,
        frames,
        magnitude,
    }
}

/// Total harmonic distortion plus noise, in decibels below the fundamental.
///
/// Everything that is not the fundamental counts: harmonics, intermodulation,
/// quantisation noise, dither. That is the honest number — a THD figure that
/// counts only the first few harmonics can hide a noise floor entirely.
///
/// The signal must be coherently sampled; use [`tone`] to generate one. DC is
/// excluded because a filter with a gain at zero hertz is not distorting.
pub fn thd_n(signal: &[f32], sample_rate: u32, fundamental_hz: f64) -> f64 {
    let spectrum = spectrum(signal, sample_rate);
    let fundamental_bin = spectrum.bin_for(fundamental_hz);
    let magnitudes = spectrum.magnitudes();

    // The residual is accumulated directly rather than as total minus
    // fundamental. Subtracting two sums that differ by fifteen orders of
    // magnitude cancels away every digit of the answer: the first version of
    // this function reported a clean tone as -inf dB, which is not a
    // measurement, it is the subtraction failing.
    let mut residual = 0.0;
    let mut fundamental = 0.0;
    for (bin, magnitude) in magnitudes.iter().enumerate().skip(1) {
        let power = magnitude * magnitude;
        // One bin either side absorbs the rounding in bin_for; with coherent
        // sampling the neighbours are empty anyway.
        if bin.abs_diff(fundamental_bin) <= 1 {
            fundamental += power;
        } else {
            residual += power;
        }
    }

    if fundamental <= 0.0 {
        return 0.0;
    }
    10.0 * (residual / fundamental).log10()
}

/// How far the harmonics of `fundamental_hz` stand above the noise beside them.
///
/// Zero decibels means the harmonic bins hold no more than their neighbours,
/// which is to say there are no harmonics -- only noise. That is the question
/// dither is an answer to, and comparing each harmonic against its own
/// surroundings rather than against the fundamental keeps the answer
/// independent of how loud the noise happens to be.
///
/// Infinity is a real result: a tone whose period divides the buffer exactly,
/// quantised without dither, puts every last bit of its error on harmonics
/// and leaves the bins between them empty.
pub fn harmonics_above_noise(signal: &[f32], sample_rate: u32, fundamental_hz: f64) -> f64 {
    /// Bins either side of a harmonic, skipping the few it might smear into.
    const REFERENCE: std::ops::Range<usize> = 8..40;

    let spectrum = spectrum(signal, sample_rate);
    let fundamental_bin = spectrum.bin_for(fundamental_hz);
    let magnitudes = spectrum.magnitudes();
    let power = |bin: usize| magnitudes[bin] * magnitudes[bin];

    let mut harmonic_power = 0.0;
    let mut harmonic_bins = 0usize;
    let mut noise_power = 0.0;
    let mut noise_bins = 0usize;

    for order in 2..=10 {
        let centre = fundamental_bin * order;
        if centre + REFERENCE.end >= magnitudes.len() {
            break;
        }
        for offset in -1i64..=1 {
            harmonic_power += power((centre as i64 + offset) as usize);
            harmonic_bins += 1;
        }
        for distance in REFERENCE {
            noise_power += power(centre - distance) + power(centre + distance);
            noise_bins += 2;
        }
    }

    if harmonic_bins == 0 || noise_bins == 0 {
        return 0.0;
    }
    if noise_power <= 0.0 {
        return if harmonic_power > 0.0 {
            f64::INFINITY
        } else {
            0.0
        };
    }
    let harmonic = harmonic_power / harmonic_bins as f64;
    let noise = noise_power / noise_bins as f64;
    10.0 * (harmonic / noise).log10()
}

/// The power between two frequencies, as an amplitude in decibels.
///
/// Used to ask where a noise floor sits rather than how big it is overall,
/// which is the only way to tell a noise shaper apart from a noise generator.
pub fn band_level(signal: &[f32], sample_rate: u32, low: f64, high: f64) -> f64 {
    let spectrum = spectrum(signal, sample_rate);
    let first = spectrum.bin_for(low).max(1);
    let last = spectrum.bin_for(high);
    let power: f64 = spectrum.magnitudes()[first..=last]
        .iter()
        .map(|magnitude| magnitude * magnitude)
        .sum();
    db(power.sqrt())
}

/// Amplitude in a frequency band, measured through a window.
///
/// [`spectrum`] needs a coherently sampled signal; anything that has been
/// through a sample-rate converter is not one, and analysing it unwindowed
/// spreads the tone across every bin. A four-term Blackman-Harris window
/// confines it again, at the cost of a floor around -92 dB where its
/// sidelobes sit.
///
/// The number carries the window's gain, so it is not a level. What it is for
/// is comparing two measurements made the same way -- the same tone before
/// and after a converter, or a tone against the alias beside it.
pub fn windowed_amplitude(signal: &[f32], sample_rate: u32, low: f64, high: f64) -> f64 {
    const BLACKMAN_HARRIS: [f64; 4] = [0.35875, -0.48829, 0.14128, -0.01168];

    let frames = signal.len();
    let windowed: Vec<f32> = signal
        .iter()
        .enumerate()
        .map(|(n, sample)| {
            let phase = TAU * n as f64 / frames as f64;
            let weight: f64 = BLACKMAN_HARRIS
                .iter()
                .enumerate()
                .map(|(term, coefficient)| coefficient * (term as f64 * phase).cos())
                .sum();
            (*sample as f64 * weight) as f32
        })
        .collect();

    let spectrum = spectrum(&windowed, sample_rate);
    let first = spectrum.bin_for(low).max(1);
    let last = spectrum.bin_for(high);
    let power: f64 = spectrum.magnitudes()[first..=last]
        .iter()
        .map(|magnitude| magnitude * magnitude)
        .sum();
    power.sqrt()
}

/// Measure what `process` does to a single frequency, in decibels.
///
/// Twice `frames` samples go in and only the second half is analysed, so a
/// filter's start-up transient is over before the measurement begins. The
/// tone is coherent in both halves, which is what makes the second half
/// analysable on its own.
pub fn magnitude_at<F>(sample_rate: u32, frames: usize, hz: f64, process: F) -> f64
where
    F: FnOnce(&mut [f32]),
{
    const AMPLITUDE: f64 = 0.5;

    let cycles = cycles_for(sample_rate, frames, hz);
    let mut signal = tone(2 * frames, 2 * cycles, AMPLITUDE);
    process(&mut signal);

    let measured = spectrum(&signal[frames..], sample_rate).magnitudes()[cycles];
    db(measured / AMPLITUDE)
}

/// Measure a processor's magnitude response across a set of frequencies.
///
/// `make` is called once per frequency: filters carry state, and a response
/// measured with the state left over from the previous point is a measurement
/// of the wrong thing.
pub fn frequency_response<P, F>(
    sample_rate: u32,
    frames: usize,
    frequencies: &[f64],
    mut make: F,
) -> Vec<f64>
where
    F: FnMut() -> P,
    P: FnOnce(&mut [f32]),
{
    frequencies
        .iter()
        .map(|hz| magnitude_at(sample_rate, frames, *hz, make()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const FRAMES: usize = 1 << 15;

    #[test]
    fn a_tone_lands_in_a_single_bin() {
        let cycles = cycles_for(RATE, FRAMES, 997.0);
        let signal = tone(FRAMES, cycles, 0.5);
        let spectrum = spectrum(&signal, RATE);

        assert_eq!(spectrum.peak_bin(), cycles);
        assert!((spectrum.magnitudes()[cycles] - 0.5).abs() < 1e-9);
        // The neighbours hold nothing: this is what coherent sampling buys,
        // and what the distortion floor below depends on.
        assert!(db(spectrum.magnitudes()[cycles - 1] / 0.5) < -150.0);
        assert!(db(spectrum.magnitudes()[cycles + 1] / 0.5) < -150.0);
    }

    #[test]
    fn a_pure_tone_has_no_measurable_distortion() {
        let cycles = cycles_for(RATE, FRAMES, 997.0);
        let signal = tone(FRAMES, cycles, 0.5);
        // The only impurity left is the 24-bit mantissa of f32 the tone was
        // rounded into; this measures -153.7 dB, which is where theory puts
        // it. Anything the bench reports above that floor later is the
        // processing, not the bench.
        let floor = thd_n(&signal, RATE, tone_frequency(RATE, FRAMES, cycles));
        assert!(floor < -150.0, "the bench's own floor is {floor:.1} dB");
    }

    #[test]
    fn a_planted_harmonic_is_measured_where_it_was_planted() {
        let cycles = cycles_for(RATE, FRAMES, 997.0);
        let fundamental = tone(FRAMES, cycles, 0.5);
        let harmonic = tone(FRAMES, 2 * cycles, 0.5 * 0.01); // -40 dB
        let signal: Vec<f32> = fundamental
            .iter()
            .zip(&harmonic)
            .map(|(a, b)| a + b)
            .collect();

        let measured = thd_n(&signal, RATE, tone_frequency(RATE, FRAMES, cycles));
        assert!(
            (measured + 40.0).abs() < 0.1,
            "expected about -40 dB, measured {measured:.2} dB"
        );
    }

    #[test]
    fn doing_nothing_measures_as_flat() {
        let points = [20.0, 100.0, 1_000.0, 5_000.0, 18_000.0];
        let response = frequency_response(RATE, FRAMES, &points, || |_: &mut [f32]| {});
        for (hz, gain) in points.iter().zip(&response) {
            assert!(gain.abs() < 0.001, "{hz} Hz moved by {gain:.4} dB");
        }
    }

    #[test]
    fn a_filter_with_a_known_response_measures_as_designed() {
        // A one-pole low-pass, whose magnitude response has a closed form. If
        // the bench agrees with the algebra here, it can be trusted on the
        // biquads later, which do not have a response anyone can check by eye.
        const CUTOFF: f64 = 1_000.0;
        let coefficient = (-TAU * CUTOFF / RATE as f64).exp();

        let points = [100.0, 500.0, 1_000.0, 4_000.0, 10_000.0];
        let response = frequency_response(RATE, FRAMES, &points, || {
            let mut state = 0.0f64;
            move |signal: &mut [f32]| {
                for sample in signal {
                    state = (1.0 - coefficient) * (*sample as f64) + coefficient * state;
                    *sample = state as f32;
                }
            }
        });

        for (hz, measured) in points.iter().zip(&response) {
            // |H(w)| = (1-a) / |1 - a*e^{-jw}|
            let omega = TAU * hz / RATE as f64;
            let (sin, cos) = omega.sin_cos();
            let expected =
                db((1.0 - coefficient) / ((1.0 - coefficient * cos).hypot(coefficient * sin)));
            assert!(
                (measured - expected).abs() < 0.01,
                "{hz} Hz: expected {expected:.3} dB, measured {measured:.3} dB"
            );
        }
    }
}
