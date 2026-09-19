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

/// What the output reports back, shared with whoever is interested.
///
/// Both counters are in frames and only ever increase, which is what lets the
/// engine read them from another thread without a lock and still get an answer
/// that means something.
#[derive(Debug, Default)]
pub struct SinkState {
    /// Frames handed to the device since the stream was started.
    pub played: AtomicU64,
    /// Frames the device asked for and did not get.
    ///
    /// Non-zero means the decoder could not keep up and the listener heard a
    /// gap. It is counted rather than logged because the callback cannot log.
    pub starved: AtomicU64,
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
pub fn fill(buffer: &mut [f32], frames: &mut Consumer<f32>, state: &SinkState, channels: u16) {
    let wanted = buffer.len();
    let (_, missing) = frames.pop_partial_slice(buffer);
    let short = missing.len();
    missing.fill(0.0);

    let channels = u64::from(channels.max(1));
    state
        .played
        .fetch_add(wanted as u64 / channels, Ordering::Relaxed);
    if short > 0 {
        state
            .starved
            .fetch_add(short as u64 / channels, Ordering::Relaxed);
    }
}

/// An output that hands its samples to the test instead of to a device.
pub struct CaptureOutput {
    shared: Arc<Mutex<Capture>>,
    rates: Vec<u32>,
}

#[derive(Default)]
struct Capture {
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
        let shared = Arc::new(Mutex::new(Capture::default()));
        (
            Self {
                shared: Arc::clone(&shared),
                rates: rates.to_vec(),
            },
            CaptureHandle { shared },
        )
    }
}

impl Output for CaptureOutput {
    fn name(&self) -> String {
        "capture".to_string()
    }

    fn supports(&self, sample_rate: u32, _channels: u16) -> bool {
        self.rates.contains(&sample_rate)
    }

    fn start(
        &mut self,
        _sample_rate: u32,
        channels: u16,
        frames: Consumer<f32>,
        state: Arc<SinkState>,
    ) -> Result<()> {
        let mut capture = self
            .shared
            .lock()
            .expect("the capture lock is never poisoned");
        capture.frames = Some(frames);
        capture.state = Some(state);
        capture.channels = channels;
        capture.paused = false;
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
            paused: false,
            ..
        } = &mut *capture
        else {
            return buffer;
        };
        fill(&mut buffer, ring, state, channels);
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

    fn channels(&self) -> u16 {
        self.shared
            .lock()
            .expect("the capture lock is never poisoned")
            .channels
            .max(1)
    }
}
