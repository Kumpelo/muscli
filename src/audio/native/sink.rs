//! Where processed audio goes.
//!
//! The device is behind a trait for one reason: a real one cannot be opened
//! in a test, and a playback path that is only ever exercised by listening to
//! it is a path where the first report of a bug is a listener. The capture
//! output here takes exactly the same samples the device callback would, on
//! demand, so the engine can be driven a block at a time and what comes out
//! compared against what went in.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use anyhow::Result;
use rtrb::Consumer;

use crate::audio::dsp::gain::Gain;

/// The listener's volume, shared between the interface and the output.
///
/// It lives here rather than in the processing chain because the chain runs
/// half a second ahead of what is being heard: a volume applied there is a
/// volume that arrives when the buffer does, and holding a key would feel
/// like pushing something heavy. Applied on the way out, it lands in the time
/// it takes to fill one device buffer.
#[derive(Debug)]
pub struct Volume(AtomicU64);

impl Default for Volume {
    fn default() -> Self {
        Self(AtomicU64::new(1.0f64.to_bits()))
    }
}

impl Volume {
    pub fn set(&self, volume: f64) {
        self.0
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// What the output reports back, shared with whoever is interested.
///
/// Both counters are in frames and only ever increase, which is what lets the
/// engine read them from another thread without a lock and still get an answer
/// that means something.
#[derive(Debug)]
pub struct SinkState {
    /// What the listener asked for, shared across every stream this player
    /// opens so that changing it needs no round trip through the engine.
    pub volume: Arc<Volume>,
    /// Frames handed to the device since the stream was started.
    pub played: AtomicU64,
    /// Frames the device asked for and did not get.
    ///
    /// Non-zero means the decoder could not keep up and the listener heard a
    /// gap. It is counted rather than logged because the callback cannot log.
    pub starved: AtomicU64,
    /// Bumped by the engine when what is still in the ring belongs to
    /// somewhere else in the track and must not be played.
    ///
    /// A single-producer ring gives the side that fills it no way to take
    /// anything back, and the side that can is the one that must never wait.
    /// So the engine asks, the output does it on its way through, and says
    /// where it happened.
    pub flush: AtomicU64,
    /// The request the output has carried out.
    pub flushed: AtomicU64,
    /// The frame the first sample after the flush will be played at.
    pub flushed_at: AtomicU64,
}

impl SinkState {
    pub fn new(volume: Arc<Volume>) -> Self {
        Self {
            volume,
            played: AtomicU64::new(0),
            starved: AtomicU64::new(0),
            flush: AtomicU64::new(0),
            flushed: AtomicU64::new(0),
            flushed_at: AtomicU64::new(0),
        }
    }
}

/// An output device.
///
/// Deliberately not `Send`: a platform stream handle often may not cross
/// threads, so the device is opened on the thread that will feed it. The
/// engine is built there too, which is where it belongs anyway.
pub trait Output {
    /// A name for the interface and for `muscli doctor`.
    fn name(&self) -> String;

    /// Whether this stream can be played without converting it.
    fn supports(&self, sample_rate: u32, channels: u16) -> bool;

    /// Rates this device will take, for a given channel count.
    fn rates(&self, channels: u16) -> Vec<u32>;

    /// Channel counts this device will take.
    fn channel_counts(&self) -> Vec<u16>;

    /// Start the output on a stream, pulling from `frames`.
    ///
    /// Starting again replaces whatever was playing.
    fn start(
        &mut self,
        sample_rate: u32,
        channels: u16,
        frames: Consumer<f32>,
        state: Arc<SinkState>,
    ) -> Result<()>;

    fn set_paused(&mut self, paused: bool) -> Result<()>;

    fn stop(&mut self);
}

/// Fill a device buffer from the ring, exactly as a callback must.
///
/// This is the only place that decides what happens on a starved ring, so
/// both the real device and the test see the same behaviour: what is there is
/// played, the rest is silence, and the shortfall is counted. Anything else --
/// waiting, allocating, locking -- would be a glitch rather than a gap.
pub fn fill(
    buffer: &mut [f32],
    frames: &mut Consumer<f32>,
    state: &SinkState,
    channels: u16,
    volume: &mut Gain,
) {
    let wanted = buffer.len();
    let channel_count = u64::from(channels.max(1));

    // A flush is a few index updates, so it is safe here; refilling is not,
    // which is why this buffer comes out silent and the engine takes over
    // from the frame reported below.
    let requested = state.flush.load(Ordering::Acquire);
    let flushed_now = requested != state.flushed.load(Ordering::Relaxed);
    if flushed_now {
        let stale = frames.slots();
        if stale > 0
            && let Ok(chunk) = frames.read_chunk(stale)
        {
            chunk.commit_all();
        }
        let played = state.played.load(Ordering::Relaxed);
        state
            .flushed_at
            .store(played + wanted as u64 / channel_count, Ordering::Relaxed);
        state.flushed.store(requested, Ordering::Release);
    }

    let (_, missing) = frames.pop_partial_slice(buffer);
    let short = missing.len();
    missing.fill(0.0);

    // Last of all, so it reaches what is already decoded and waiting.
    volume.set(state.volume.get());
    volume.process(buffer, channels.max(1).into());

    state
        .played
        .fetch_add(wanted as u64 / channel_count, Ordering::Relaxed);
    // Silence in the buffer that carried out a flush is the seek landing,
    // not the decoder falling behind, and counting it would accuse the wrong
    // thing.
    if short > 0 && !flushed_now {
        state
            .starved
            .fetch_add(short as u64 / channel_count, Ordering::Relaxed);
    }
}

/// An output that hands its samples to the test instead of to a device.
pub struct CaptureOutput {
    shared: Arc<Mutex<Capture>>,
    rates: Vec<u32>,
    channels: Vec<u16>,
}

#[derive(Default)]
struct Capture {
    /// How many times a stream has been opened on this output, so a test can
    /// tell a seek that reuses the device from one that reopens it.
    starts: usize,
    volume: Option<Gain>,
    frames: Option<Consumer<f32>>,
    state: Option<Arc<SinkState>>,
    channels: u16,
    paused: bool,
}

/// The test's end of a [`CaptureOutput`].
#[derive(Clone)]
pub struct CaptureHandle {
    shared: Arc<Mutex<Capture>>,
}

impl CaptureOutput {
    /// An output accepting `rates`, and the handle to pull from it.
    ///
    /// Listing the rates rather than accepting everything is what lets a test
    /// ask what happens when a device cannot play a file.
    pub fn new(rates: &[u32]) -> (Self, CaptureHandle) {
        Self::with_channels(rates, &[1, 2])
    }

    /// The same, for a device that only offers certain channel counts.
    pub fn with_channels(rates: &[u32], channels: &[u16]) -> (Self, CaptureHandle) {
        let shared = Arc::new(Mutex::new(Capture::default()));
        (
            Self {
                shared: Arc::clone(&shared),
                rates: rates.to_vec(),
                channels: channels.to_vec(),
            },
            CaptureHandle { shared },
        )
    }
}

impl Output for CaptureOutput {
    fn name(&self) -> String {
        "capture".to_string()
    }

    fn supports(&self, sample_rate: u32, channels: u16) -> bool {
        self.rates.contains(&sample_rate) && self.channels.contains(&channels)
    }

    fn rates(&self, _channels: u16) -> Vec<u32> {
        self.rates.clone()
    }

    fn channel_counts(&self) -> Vec<u16> {
        self.channels.clone()
    }

    fn start(
        &mut self,
        sample_rate: u32,
        channels: u16,
        frames: Consumer<f32>,
        state: Arc<SinkState>,
    ) -> Result<()> {
        let mut capture = self
            .shared
            .lock()
            .expect("the capture lock is never poisoned");
        capture.volume = Some(Gain::new(sample_rate, state.volume.get()));
        capture.frames = Some(frames);
        capture.state = Some(state);
        capture.channels = channels;
        capture.paused = false;
        capture.starts += 1;
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.shared
            .lock()
            .expect("the capture lock is never poisoned")
            .paused = paused;
        Ok(())
    }

    fn stop(&mut self) {
        let mut capture = self
            .shared
            .lock()
            .expect("the capture lock is never poisoned");
        capture.frames = None;
        capture.state = None;
    }
}

impl CaptureHandle {
    /// Take `frames` frames, as a device callback of that size would.
    pub fn pull(&self, frames: usize) -> Vec<f32> {
        let mut capture = self
            .shared
            .lock()
            .expect("the capture lock is never poisoned");
        let channels = capture.channels.max(1);
        let mut buffer = vec![0.0; frames * channels as usize];

        let Capture {
            frames: Some(ring),
            state: Some(state),
            volume: Some(volume),
            paused: false,
            ..
        } = &mut *capture
        else {
            return buffer;
        };
        fill(&mut buffer, ring, state, channels, volume);
        buffer
    }

    /// Keep pulling until `frames` non-silent frames have come out or the
    /// patience runs out, returning everything pulled.
    pub fn drain(&self, frames: usize, block: usize) -> Vec<f32> {
        let mut collected = Vec::new();
        while collected.len() < frames * self.channels() as usize {
            collected.extend(self.pull(block));
        }
        collected
    }

    /// How many times the output has been opened.
    pub fn starts(&self) -> usize {
        self.shared
            .lock()
            .expect("the capture lock is never poisoned")
            .starts
    }

    fn channels(&self) -> u16 {
        self.shared
            .lock()
            .expect("the capture lock is never poisoned")
            .channels
            .max(1)
    }
}

/// Pick the rate to play a file at on a device offering `available`.
///
/// The file's own rate first, always: converting a stream that did not need
/// converting is the one avoidable loss in the whole path. Failing that, a
/// whole multiple of it, which a converter handles with the least work and
/// the least error. Failing that, the highest rate above the file's, because
/// converting downwards throws away the top of the band for nothing. Only if
/// there is nothing higher does a lower rate get used.
pub fn choose_rate(wanted: u32, available: &[u32]) -> Option<u32> {
    if available.contains(&wanted) {
        return Some(wanted);
    }
    if let Some(multiple) = available
        .iter()
        .filter(|rate| **rate > wanted && (*rate % wanted) == 0)
        .min()
    {
        return Some(*multiple);
    }
    available
        .iter()
        .filter(|rate| **rate > wanted)
        .min()
        .or_else(|| available.iter().max())
        .copied()
}

/// Pick the channel count to play a file in.
///
/// A mono file on a stereo-only device is played to both channels; anything
/// else is refused rather than folded, because a fold is a mix, and mixing
/// somebody's recording without being asked is not this player's business.
pub fn choose_channels(wanted: u16, available: &[u16]) -> Option<u16> {
    if available.contains(&wanted) {
        return Some(wanted);
    }
    if wanted == 1 && available.contains(&2) {
        return Some(2);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_the_device_has_is_used_as_it_is() {
        assert_eq!(choose_rate(44_100, &[44_100, 48_000]), Some(44_100));
        assert_eq!(choose_rate(192_000, &[48_000, 192_000]), Some(192_000));
    }

    #[test]
    fn a_whole_multiple_beats_a_nearer_rate() {
        // 88.2 is two times 44.1, so the converter has only to interpolate
        // between samples that already line up. 96 is closer in ratio and
        // much harder to get right.
        assert_eq!(choose_rate(44_100, &[48_000, 88_200, 96_000]), Some(88_200));
    }

    #[test]
    fn converting_downwards_is_the_last_resort() {
        assert_eq!(
            choose_rate(96_000, &[44_100, 48_000, 192_000]),
            Some(192_000)
        );
        assert_eq!(choose_rate(96_000, &[44_100, 48_000]), Some(48_000));
    }

    #[test]
    fn a_mono_file_may_be_played_to_both_channels() {
        assert_eq!(choose_channels(1, &[2]), Some(2));
        assert_eq!(choose_channels(2, &[2]), Some(2));
    }

    #[test]
    fn nothing_is_folded_without_being_asked() {
        assert_eq!(choose_channels(6, &[2]), None);
        assert_eq!(choose_channels(2, &[1]), None);
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;
    use rtrb::RingBuffer;

    const RATE: u32 = 48_000;

    fn ring(samples: &[f32]) -> (Consumer<f32>, Arc<SinkState>) {
        let (mut producer, consumer) = RingBuffer::new(samples.len().max(1));
        let _ = producer.push_partial_slice(samples);
        std::mem::forget(producer);
        (
            consumer,
            Arc::new(SinkState::new(Arc::new(Volume::default()))),
        )
    }

    #[test]
    fn the_volume_reaches_audio_that_is_already_waiting() {
        // The buffer holds half a second. A volume applied where the decoding
        // happens would not be heard until all of that had played, which is
        // what made holding the key feel like pushing something heavy.
        let (mut frames, state) = ring(&vec![1.0f32; 4_096]);
        state.volume.set(0.5);
        let mut gain = Gain::new(RATE, state.volume.get());

        let mut buffer = vec![0.0f32; 2_048];
        fill(&mut buffer, &mut frames, &state, 2, &mut gain);

        // Past the ramp, the samples that were already in the ring come out
        // at the new volume.
        let settled = buffer[buffer.len() - 1];
        assert!(
            (settled - 0.5).abs() < 1e-6,
            "audio already buffered came out at {settled} instead of 0.5"
        );
    }

    #[test]
    fn a_volume_change_does_not_click() {
        let (mut frames, state) = ring(&vec![1.0f32; 8_192]);
        let mut gain = Gain::new(RATE, 1.0);

        let mut buffer = vec![0.0f32; 1_024];
        fill(&mut buffer, &mut frames, &state, 2, &mut gain);
        state.volume.set(0.0);
        let mut next = vec![0.0f32; 4_096];
        fill(&mut next, &mut frames, &state, 2, &mut gain);

        buffer.extend_from_slice(&next);
        let worst = buffer
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 0.002,
            "the volume jumped by {worst} between samples"
        );
    }

    #[test]
    fn full_volume_leaves_the_samples_exactly_as_they_were() {
        let original: Vec<f32> = (0..1_024).map(|n| (n as f32 * 0.01).sin() * 0.5).collect();
        let (mut frames, state) = ring(&original);
        let mut gain = Gain::new(RATE, 1.0);

        let mut buffer = vec![0.0f32; 1_024];
        fill(&mut buffer, &mut frames, &state, 2, &mut gain);
        assert_eq!(buffer, original);
    }
}
