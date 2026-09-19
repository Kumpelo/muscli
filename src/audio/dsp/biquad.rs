//! Second-order sections, the building block of the equaliser.

use std::f64::consts::{LN_2, TAU};

/// A biquad in Direct Form II transposed.
///
/// Transposed direct form II is the form to use in floating point: it keeps
/// only two state words, and its error behaviour at low frequencies -- where a
/// bass band's poles sit very close to the unit circle -- is markedly better
/// than direct form I. The state is `f64` even though the audio is `f32`,
/// because the recursion feeds its own rounding error back: at 32 bits a
/// 60 Hz band accumulates an audible noise floor, and the extra precision
/// costs nothing a modern core can measure.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    s1: f64,
    s2: f64,
}

impl Default for Biquad {
    fn default() -> Self {
        Self::identity()
    }
}

impl Biquad {
    /// A section that passes its input through untouched.
    pub const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// A peaking band: `gain_db` at `frequency`, tapering away either side over
    /// `bandwidth` octaves.
    ///
    /// These are the Audio EQ Cookbook forms. The width is given in octaves
    /// rather than as a Q so that a fixed set of bands sounds evenly spaced:
    /// an octave is an octave whether it sits at 60 Hz or at 12 kHz, while a
    /// constant Q would make the low bands far narrower than the high ones.
    pub fn peaking(sample_rate: u32, frequency: f64, gain_db: f64, bandwidth: f64) -> Self {
        // A band whose centre approaches Nyquist has nowhere to taper into,
        // and the width term divides by sin(w0), which goes to zero there.
        let nyquist = sample_rate as f64 / 2.0;
        let frequency = frequency.clamp(1.0, nyquist * 0.9);

        let amplitude = 10.0f64.powf(gain_db / 40.0);
        let w0 = TAU * frequency / sample_rate as f64;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin * ((LN_2 / 2.0) * bandwidth * w0 / sin).sinh();

        let a0 = 1.0 + alpha / amplitude;
        Self {
            b0: (1.0 + alpha * amplitude) / a0,
            b1: (-2.0 * cos) / a0,
            b2: (1.0 - alpha * amplitude) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha / amplitude) / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// Run one sample through, advancing the state.
    #[inline]
    pub fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.s1;
        self.s1 = self.b1 * input - self.a1 * output + self.s2;
        self.s2 = self.b2 * input - self.a2 * output;
        output
    }

    /// Forget the past. Called when playback jumps, so the tail of one part of
    /// a track cannot ring into another.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// The gain this section applies at `hz`, in decibels.
    ///
    /// This is the algebra rather than a measurement, which is the point: the
    /// tests compare it against what the bench measures coming out of
    /// [`process`](Self::process), so a mistake in either one shows up.
    pub fn gain_db_at(&self, sample_rate: u32, hz: f64) -> f64 {
        let w = TAU * hz / sample_rate as f64;
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();

        let numerator =
            (self.b0 + self.b1 * cos1 + self.b2 * cos2).hypot(-self.b1 * sin1 - self.b2 * sin2);
        let denominator =
            (1.0 + self.a1 * cos1 + self.a2 * cos2).hypot(-self.a1 * sin1 - self.a2 * sin2);
        super::super::measure::db(numerator / denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::measure::{frequency_response, magnitude_at};

    const RATE: u32 = 48_000;
    const FRAMES: usize = 1 << 15;

    fn run(mut filter: Biquad) -> impl FnOnce(&mut [f32]) {
        move |signal: &mut [f32]| {
            for sample in signal {
                *sample = filter.process(*sample as f64) as f32;
            }
        }
    }

    #[test]
    fn an_identity_section_changes_nothing_at_all() {
        let mut filter = Biquad::identity();
        let input: Vec<f32> = (0..1000).map(|n| (n as f32 * 0.001).sin()).collect();
        for sample in &input {
            assert_eq!(filter.process(*sample as f64) as f32, *sample);
        }
    }

    #[test]
    fn a_band_at_zero_decibels_is_a_pass_through() {
        // Not merely quiet: the coefficients cancel exactly, so a user with a
        // flat equaliser gets their file back sample for sample.
        let mut filter = Biquad::peaking(RATE, 1_000.0, 0.0, 1.0);
        let input: Vec<f32> = (0..1000).map(|n| (n as f32 * 0.01).sin() * 0.5).collect();
        for sample in &input {
            assert_eq!(filter.process(*sample as f64) as f32, *sample);
        }
    }

    #[test]
    fn a_band_lifts_its_own_centre_by_what_was_asked() {
        for gain in [-12.0, -6.0, -3.0, 3.0, 6.0, 12.0] {
            let measured = magnitude_at(
                RATE,
                FRAMES,
                1_000.0,
                run(Biquad::peaking(RATE, 1_000.0, gain, 1.0)),
            );
            assert!(
                (measured - gain).abs() < 0.05,
                "asked for {gain} dB at 1 kHz, measured {measured:.3} dB"
            );
        }
    }

    #[test]
    fn what_comes_out_matches_the_transfer_function() {
        let design = Biquad::peaking(RATE, 2_400.0, 8.0, 1.0);
        let points = [100.0, 600.0, 1_200.0, 2_400.0, 4_800.0, 12_000.0];
        let response = frequency_response(RATE, FRAMES, &points, || run(design));

        for (hz, measured) in points.iter().zip(&response) {
            let expected = design.gain_db_at(RATE, *hz);
            assert!(
                (measured - expected).abs() < 0.02,
                "{hz} Hz: algebra says {expected:.3} dB, the filter did {measured:.3} dB"
            );
        }
    }

    #[test]
    fn the_bandwidth_is_measured_where_the_cookbook_says() {
        // The width is the span between the half-gain points, so a one-octave
        // band reaches half its gain half an octave either side of centre --
        // not a full octave, which is the mistake that makes a fixed band set
        // sound narrower than it reads.
        let design = Biquad::peaking(RATE, 1_000.0, 12.0, 1.0);
        for edge in [
            1_000.0 / std::f64::consts::SQRT_2,
            1_000.0 * std::f64::consts::SQRT_2,
        ] {
            let measured = design.gain_db_at(RATE, edge);
            assert!(
                (measured - 6.0).abs() < 0.1,
                "half an octave from a 12 dB band reads {measured:.2} dB"
            );
        }
    }

    #[test]
    fn a_band_near_nyquist_stays_finite() {
        // 16 kHz is one of the fixed bands, and a 32 kHz file puts Nyquist
        // right on top of it. The coefficients must not become infinite.
        let design = Biquad::peaking(32_000, 16_000.0, 6.0, 1.0);
        let mut filter = design;
        for n in 0..1000 {
            let output = filter.process((n as f64 * 0.01).sin() * 0.5);
            assert!(output.is_finite(), "sample {n} came out as {output}");
        }
    }
}
