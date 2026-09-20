//! A gain that slides instead of jumping.
//!
//! Both the volume control and ReplayGain are this, applied here rather than
//! handed to the device. That is the choice that makes everything else
//! possible: a hardware mixer is shared with the rest of the desktop,
//! quantises to whatever the driver feels like, and cannot be combined with a
//! track's ReplayGain without one of the two fighting the other. In `f32`
//! there is no quantisation to speak of -- a 24-bit signal attenuated by
//! 40 dB still has more resolution left than the recording ever had.

/// How long a volume change takes to arrive.
///
/// A gain that jumps between two blocks is a step in the waveform, and a step
/// is a click. Twenty milliseconds is long enough to be inaudible and short
/// enough that holding a volume key still feels immediate.
const RAMP_MS: f64 = 20.0;

/// A gain that slides to where it was sent.
#[derive(Debug, Clone, Copy)]
pub struct Gain {
    current: f64,
    target: f64,
    /// Change per frame while a ramp is running; zero once it has arrived.
    step: f64,
    remaining: u32,
    ramp_frames: u32,
}

impl Gain {
    pub fn new(sample_rate: u32, initial: f64) -> Self {
        let ramp_frames = ((sample_rate as f64 * RAMP_MS / 1_000.0) as u32).max(1);
        Self {
            current: initial,
            target: initial,
            step: 0.0,
            remaining: 0,
            ramp_frames,
        }
    }

    /// Slide to a linear gain.
    ///
    /// Asking for the target it is already heading to does nothing, so a
    /// caller that sets it on every block does not restart the ramp on every
    /// block and leave it never arriving.
    pub fn set(&mut self, target: f64) {
        if self.target == target {
            return;
        }
        self.target = target;
        self.retarget();
    }

    /// Slide to a gain in decibels, or to unity with `None`.
    pub fn set_db(&mut self, gain_db: Option<f64>) {
        self.set(gain_db.map_or(1.0, |db| 10.0f64.powf(db / 20.0)));
    }

    fn retarget(&mut self) {
        if self.target == self.current {
            self.remaining = 0;
            self.step = 0.0;
            return;
        }
        self.remaining = self.ramp_frames;
        self.step = (self.target - self.current) / self.ramp_frames as f64;
    }

    /// Arrive at the current target immediately.
    ///
    /// Used when playback starts or jumps: there is no previous audio for the
    /// ramp to be continuous with, so sliding would only fade the first
    /// twenty milliseconds in for no reason.
    pub fn settle(&mut self) {
        self.current = self.target;
        self.remaining = 0;
        self.step = 0.0;
    }

    /// The gain being applied right now.
    pub fn value(&self) -> f64 {
        self.current
    }

    /// Whether the gain would change anything at all.
    pub fn is_transparent(&self) -> bool {
        self.remaining == 0 && self.current == 1.0
    }

    /// Apply the gain to a block of interleaved samples.
    pub fn process(&mut self, interleaved: &mut [f32], channels: usize) {
        if self.is_transparent() {
            // Unity gain with nothing moving: leave the samples exactly as
            // they were decoded rather than multiplying by one and rounding.
            return;
        }

        for frame in interleaved.chunks_mut(channels.max(1)) {
            if self.remaining > 0 {
                self.current += self.step;
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.current = self.target;
                }
            }
            for sample in frame {
                *sample = (*sample as f64 * self.current) as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::measure::{db, rms, tone};

    const RATE: u32 = 48_000;

    #[test]
    fn full_volume_leaves_the_samples_alone() {
        let mut gain = Gain::new(RATE, 1.0);
        let original = tone(1024, 7, 0.5);
        let mut signal = original.clone();
        gain.process(&mut signal, 2);
        assert_eq!(signal, original);
    }

    #[test]
    fn a_half_amplitude_setting_is_six_decibels_down() {
        let mut gain = Gain::new(RATE, 1.0);
        gain.set(0.5);
        gain.settle();

        let original = tone(4096, 85, 0.5);
        let mut signal = original.clone();
        gain.process(&mut signal, 1);
        let moved = db(rms(&signal) / rms(&original));
        assert!((moved + 6.0206).abs() < 0.001, "moved {moved:.4} dB");
    }

    #[test]
    fn decibels_arrive_as_the_right_ratio() {
        let mut gain = Gain::new(RATE, 1.0);
        gain.set_db(Some(-6.0206));
        gain.settle();
        assert!((gain.value() - 0.5).abs() < 1e-6, "{}", gain.value());

        gain.set_db(None);
        gain.settle();
        assert_eq!(gain.value(), 1.0);
    }

    #[test]
    fn a_volume_change_takes_the_ramp_and_not_less() {
        let mut gain = Gain::new(RATE, 1.0);
        gain.set(0.0);

        // Half way through the ramp it must be neither finished nor unstarted.
        let mut signal = vec![1.0f32; (RATE as usize / 1_000) * 10];
        gain.process(&mut signal, 1);
        assert!(
            gain.value() > 0.4 && gain.value() < 0.6,
            "ten milliseconds into a twenty millisecond ramp the gain is {}",
            gain.value()
        );

        let mut rest = vec![1.0f32; (RATE as usize / 1_000) * 11];
        gain.process(&mut rest, 1);
        assert_eq!(gain.value(), 0.0);
    }

    #[test]
    fn a_volume_change_never_steps() {
        // What makes a volume change click is the size of the jump between
        // one sample and the next, so that is what gets measured.
        let mut gain = Gain::new(RATE, 1.0);
        gain.set(0.0);

        let mut signal = vec![1.0f32; RATE as usize / 10];
        gain.process(&mut signal, 1);

        let worst = signal
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 0.002,
            "the gain jumped by {worst} between two samples"
        );
    }
}
