//! A look-ahead peak limiter, catching what the equaliser or an upward
//! ReplayGain pushed past full scale.
//!
//! Looking ahead lets the gain start coming down before the loud sample
//! arrives, so the reduction is gradual; an instantaneous one is itself a
//! click.

use std::collections::VecDeque;

use crate::audio::measure::db;

/// How far ahead the limiter looks, and therefore the latency it adds.
const LOOKAHEAD_MS: f64 = 2.0;

/// How long the gain takes to come back, in milliseconds.
const RELEASE_MS: f64 = 100.0;

pub struct Limiter {
    ceiling: f64,
    channels: usize,
    lookahead: usize,
    /// The signal, delayed by exactly the look-ahead, as a ring.
    delay: Vec<f32>,
    delay_head: usize,
    /// A monotonic queue giving the minimum required gain over the window
    /// still ahead of the output. Preallocated: this runs in the callback.
    window: VecDeque<(u64, f64)>,
    frame: u64,
    held: f64,
    release: f64,
    /// A ring of recent held values with its running sum, which averages the
    /// gain curve so it has no corners.
    history: Vec<f64>,
    history_head: usize,
    history_sum: f64,
    worst: f64,
}

impl Limiter {
    /// A limiter for a stream, holding peaks to `ceiling_db` (at or below 0).
    pub fn new(sample_rate: u32, channels: usize, ceiling_db: f64) -> Self {
        let channels = channels.max(1);
        let lookahead = ((sample_rate as f64 * LOOKAHEAD_MS / 1_000.0) as usize).max(1);
        let release = 1.0 - (-1_000.0 / (RELEASE_MS * sample_rate as f64)).exp();

        Self {
            ceiling: 10.0f64.powf(ceiling_db.min(0.0) / 20.0),
            channels,
            lookahead,
            delay: vec![0.0; lookahead * channels],
            delay_head: 0,
            window: VecDeque::with_capacity(lookahead + 2),
            frame: 0,
            held: 1.0,
            release,
            history: vec![1.0; lookahead],
            history_head: 0,
            history_sum: lookahead as f64,
            worst: 1.0,
        }
    }

    /// The latency the limiter adds, in frames. Everything that reports a
    /// playback position has to account for it.
    pub fn latency_frames(&self) -> usize {
        self.lookahead
    }

    /// The largest reduction applied since this was last asked, in decibels
    /// (never positive). For the interface, so a listener can see the limiter
    /// working rather than wonder.
    pub fn take_reduction_db(&mut self) -> f64 {
        let worst = std::mem::replace(&mut self.worst, 1.0);
        db(worst)
    }

    /// Clear the delay and the gain history.
    pub fn reset(&mut self) {
        self.delay.fill(0.0);
        self.delay_head = 0;
        self.window.clear();
        self.frame = 0;
        self.held = 1.0;
        self.history.fill(1.0);
        self.history_head = 0;
        self.history_sum = self.history.len() as f64;
        self.worst = 1.0;
    }

    /// Limit a block of interleaved samples in place. Output is delayed by
    /// [`latency_frames`](Self::latency_frames), which is silence at first.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        for frame in interleaved.chunks_exact_mut(self.channels) {
            let peak = frame
                .iter()
                .fold(0.0f64, |worst, s| worst.max(s.abs() as f64));
            let required = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };

            // Keep the queue increasing, so its front is the minimum over the
            // frames between the one about to come out and the one going in.
            while self
                .window
                .back()
                .is_some_and(|(_, value)| *value >= required)
            {
                self.window.pop_back();
            }
            self.window.push_back((self.frame, required));
            let oldest = self.frame.saturating_sub(self.lookahead as u64);
            while self.window.front().is_some_and(|(at, _)| *at < oldest) {
                self.window.pop_front();
            }
            let ahead = self.window.front().map_or(1.0, |(_, value)| *value);

            // Down instantly, back up slowly: without the release the gain
            // would snap back the moment a peak passed, and the pumping that
            // causes is far more audible than the peak was.
            self.held = ahead.min(self.held + (1.0 - self.held) * self.release);

            self.history_sum -= self.history[self.history_head];
            self.history[self.history_head] = self.held;
            self.history_sum += self.held;
            self.history_head = (self.history_head + 1) % self.history.len();
            let gain = self.history_sum / self.history.len() as f64;
            self.worst = self.worst.min(gain);

            let slot = self.delay_head * self.channels;
            for (channel, sample) in frame.iter_mut().enumerate() {
                let delayed = self.delay[slot + channel];
                self.delay[slot + channel] = *sample;
                *sample = if gain < 1.0 {
                    (delayed as f64 * gain) as f32
                } else {
                    delayed
                };
            }
            self.delay_head = (self.delay_head + 1) % self.lookahead;
            self.frame += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::measure::{peak, thd_n, tone, tone_frequency};

    const RATE: u32 = 48_000;
    const FRAMES: usize = 1 << 15;

    #[test]
    fn audio_below_the_ceiling_comes_through_untouched() {
        // The common case is every track that was mastered sanely, and the
        // limiter has to be invisible on all of them.
        let mut limiter = Limiter::new(RATE, 1, 0.0);
        let original = tone(FRAMES, 683, 0.95);
        let mut signal = original.clone();
        limiter.process(&mut signal);

        let latency = limiter.latency_frames();
        assert_eq!(&signal[latency..], &original[..original.len() - latency]);
    }

    #[test]
    fn nothing_gets_past_the_ceiling() {
        let mut limiter = Limiter::new(RATE, 2, 0.0);
        // A tone well over full scale, as an equaliser boost would produce.
        let mono = tone(FRAMES, 683, 1.8);
        let mut stereo: Vec<f32> = mono.iter().flat_map(|s| [*s, *s]).collect();
        limiter.process(&mut stereo);

        let highest = peak(&stereo);
        assert!(highest <= 1.0 + 1e-6, "a sample came out at {highest}");
    }

    #[test]
    fn a_lower_ceiling_is_respected_too() {
        let mut limiter = Limiter::new(RATE, 1, -3.0);
        let mut signal = tone(FRAMES, 683, 1.0);
        limiter.process(&mut signal);

        let ceiling = 10.0f64.powf(-3.0 / 20.0);
        assert!(peak(&signal) <= ceiling + 1e-6);
    }

    #[test]
    fn holding_a_steady_tone_down_does_not_distort_it() {
        // A badly smoothed limiter modulates the gain at the signal's own
        // rate, turning the reduction into distortion. This measures -64.5 dB
        // holding a tone 3 dB over full scale; letting it clip instead
        // measures -17.7 dB. The residual is peak detection rather than
        // smoothing, and removing it would take an oversampled true-peak
        // detector.
        let mut limiter = Limiter::new(RATE, 1, 0.0);
        let cycles = 683;
        let mut signal = tone(4 * FRAMES, 4 * cycles, 1.4);
        limiter.process(&mut signal);

        let tail = &signal[3 * FRAMES..];
        let measured = thd_n(tail, RATE, tone_frequency(RATE, FRAMES, cycles));
        assert!(
            measured < -60.0,
            "limiting a steady tone added distortion at {measured:.1} dB"
        );
    }

    #[test]
    fn the_reduction_is_reported() {
        let mut limiter = Limiter::new(RATE, 1, 0.0);
        let mut signal = tone(FRAMES, 683, 2.0);
        limiter.process(&mut signal);

        let reduction = limiter.take_reduction_db();
        assert!(
            (reduction + 6.0206).abs() < 0.2,
            "halving the signal was reported as {reduction:.2} dB"
        );
        assert_eq!(limiter.take_reduction_db(), 0.0);
    }
}
