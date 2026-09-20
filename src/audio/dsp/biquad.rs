//! Second-order sections, the building block of the equaliser.

use std::f64::consts::{LN_2, TAU};

/// A biquad in Direct Form II transposed: two state words, and better error
/// behaviour at low frequencies than direct form I.
///
/// The state is `f64` although the audio is `f32`, because the recursion feeds
/// its rounding error back. The `precision` test measures the difference: at
/// 60 Hz, -152.8 dB against -82.6 dB.
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

    /// A peaking band: `gain_db` at `frequency`, tapering over `bandwidth`
    /// octaves. Audio EQ Cookbook forms.
    ///
    /// The width is in octaves rather than Q so a fixed set of bands is evenly
    /// spaced; a constant Q would make the low bands far narrower.
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

    /// The gain this section applies at `hz`, in decibels, from the transfer
    /// function. The tests compare it against what [`process`](Self::process)
    /// actually does.
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
        // The width spans the half-gain points, so a one-octave band reaches
        // half its gain half an octave either side of centre, not a full one.
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

#[cfg(test)]
mod precision {
    use super::*;
    use crate::audio::measure::{thd_n, tone, tone_frequency};

    const RATE: u32 = 48_000;
    const FRAMES: usize = 1 << 16;

    /// The same section with `f32` state. Not used in the player; it is here
    /// so the test can show what the wider state is worth.
    fn narrow(design: Biquad, signal: &mut [f32]) {
        let (b0, b1, b2, a1, a2) = (
            design.b0 as f32,
            design.b1 as f32,
            design.b2 as f32,
            design.a1 as f32,
            design.a2 as f32,
        );
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for sample in signal {
            let output = b0 * *sample + s1;
            s1 = b1 * *sample - a1 * output + s2;
            s2 = b2 * *sample - a2 * output;
            *sample = output;
        }
    }

    fn distortion(design: Biquad, cycles: usize, wide: bool) -> f64 {
        let mut signal = tone(2 * FRAMES, 2 * cycles, 0.5);
        if wide {
            let mut filter = design;
            for sample in &mut signal {
                *sample = filter.process(*sample as f64) as f32;
            }
        } else {
            narrow(design, &mut signal);
        }
        // The second half only: the filter's start-up is not periodic, and
        // measuring across it measures the transient rather than the filter.
        thd_n(
            &signal[FRAMES..],
            RATE,
            tone_frequency(RATE, FRAMES, cycles),
        )
    }

    #[test]
    fn the_state_has_to_be_wider_than_the_audio() {
        // 60 Hz is the lowest band, where the poles sit closest to the unit
        // circle and the recursion has the least room for error.
        let design = Biquad::peaking(RATE, 60.0, 6.0, 1.0);
        let wide = distortion(design, 82, true);
        let narrow = distortion(design, 82, false);

        assert!(wide < -140.0, "the shipped filter left {wide:.1} dB");
        assert!(
            narrow > wide + 50.0,
            "f32 state measured {narrow:.1} dB against {wide:.1} dB, which \
             would make the wider state pointless"
        );
    }
}
