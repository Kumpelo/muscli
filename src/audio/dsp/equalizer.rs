//! The eight-band equaliser: fixed centres from `config::EQUALIZER_BANDS`,
//! one octave wide each. The same shape the mpv path asks lavfi for, so both
//! backends sound alike.

use super::biquad::Biquad;

/// Width of every band, in octaves. Narrow enough that eight bands do not
/// overlap much; heavily overlapping bands would interact, and two adjacent
/// ones at +6 dB would give far more than +6 dB between them.
const BANDWIDTH: f64 = 1.0;

/// Below this the band is dropped rather than built, so a slider parked at
/// zero costs nothing and, more importantly, cannot colour anything.
const NEGLIGIBLE_DB: f32 = 0.05;

pub struct Equalizer {
    channels: usize,
    /// One filter per band per channel, band-major. Each channel needs its own
    /// state: sharing it would mix the channels together, which at these
    /// gains collapses the stereo image.
    filters: Vec<Biquad>,
    bands: usize,
}

impl Equalizer {
    /// Build the active bands for a stream, as (centre frequency, gain in
    /// decibels). A band at zero is left out entirely.
    pub fn new(sample_rate: u32, channels: usize, bands: &[(u32, f32)]) -> Self {
        let wanted: Vec<&(u32, f32)> = bands
            .iter()
            .filter(|(_, gain)| gain.abs() >= NEGLIGIBLE_DB)
            .collect();

        let mut filters = Vec::with_capacity(wanted.len() * channels);
        for (frequency, gain) in &wanted {
            let design = Biquad::peaking(sample_rate, *frequency as f64, *gain as f64, BANDWIDTH);
            filters.extend(std::iter::repeat_n(design, channels));
        }

        Self {
            channels,
            filters,
            bands: wanted.len(),
        }
    }

    /// Whether this equaliser would do anything.
    pub fn is_active(&self) -> bool {
        self.bands > 0
    }

    /// Forget the filters' history, for a seek or a track change.
    pub fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
    }

    /// Filter a block of interleaved samples in place.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        if self.bands == 0 {
            return;
        }
        for band in 0..self.bands {
            let filters = &mut self.filters[band * self.channels..(band + 1) * self.channels];
            for frame in interleaved.chunks_exact_mut(self.channels) {
                for (sample, filter) in frame.iter_mut().zip(filters.iter_mut()) {
                    *sample = filter.process(*sample as f64) as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio::measure::{db, magnitude_at, rms, tone},
        config::EQUALIZER_BANDS,
    };

    const RATE: u32 = 48_000;
    const FRAMES: usize = 1 << 15;

    fn flat() -> Vec<(u32, f32)> {
        EQUALIZER_BANDS.iter().map(|hz| (*hz, 0.0)).collect()
    }

    #[test]
    fn a_flat_equaliser_returns_the_file_untouched() {
        let mut equalizer = Equalizer::new(RATE, 2, &flat());
        assert!(!equalizer.is_active());

        let original = tone(1024, 7, 0.5);
        let mut signal = original.clone();
        equalizer.process(&mut signal);
        assert_eq!(signal, original);
    }

    #[test]
    fn each_band_lifts_its_own_frequency() {
        for (index, centre) in EQUALIZER_BANDS.iter().enumerate() {
            let mut bands = flat();
            bands[index].1 = 6.0;

            let measured = magnitude_at(RATE, FRAMES, *centre as f64, |signal: &mut [f32]| {
                Equalizer::new(RATE, 1, &bands).process(signal)
            });
            assert!(
                (measured - 6.0).abs() < 0.2,
                "{centre} Hz asked for 6 dB and moved {measured:.2} dB"
            );
        }
    }

    #[test]
    fn a_band_leaves_its_neighbours_two_octaves_away_alone() {
        // Eight one-octave bands do not overlap much; if they did, a single
        // slider would drag the whole spectrum with it.
        let mut bands = flat();
        bands[3].1 = 12.0; // 1 kHz
        let far = magnitude_at(RATE, FRAMES, 4_000.0, |signal: &mut [f32]| {
            Equalizer::new(RATE, 1, &bands).process(signal)
        });
        assert!(far < 1.0, "1 kHz at +12 dB moved 4 kHz by {far:.2} dB");
    }

    #[test]
    fn the_channels_are_filtered_apart() {
        // One channel loud, one silent. If the filters shared state the silent
        // channel would pick up the other one's ringing.
        let mut bands = flat();
        bands[3].1 = 12.0;
        let mut equalizer = Equalizer::new(RATE, 2, &bands);

        let mono = tone(4096, 85, 0.5);
        let mut stereo: Vec<f32> = mono.iter().flat_map(|s| [*s, 0.0]).collect();
        equalizer.process(&mut stereo);

        let right: Vec<f32> = stereo.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(
            rms(&right),
            0.0,
            "the silent channel picked up {:?}",
            db(rms(&right))
        );
    }

    #[test]
    fn resetting_clears_the_ringing() {
        let mut bands = flat();
        bands[0].1 = 12.0;
        let mut equalizer = Equalizer::new(RATE, 1, &bands);

        let mut loud = tone(4096, 6, 0.9);
        equalizer.process(&mut loud);

        let mut silence = vec![0.0f32; 4096];
        equalizer.reset();
        equalizer.process(&mut silence);
        assert!(silence.iter().all(|s| *s == 0.0));
    }
}
