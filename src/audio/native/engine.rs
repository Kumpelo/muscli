//! The thread that decodes, processes and fills the ring.
//!
//! The split is the point. Everything that can block -- opening a file,
//! decoding, allocating -- happens here. The device callback does one thing:
//! copy out of a lock-free ring and, if the ring is short, output silence and
//! count it. A callback that waits for anything is a callback that misses its
//! deadline, and a missed deadline is a click.
//!
//! The engine is driven by [`Engine::step`] rather than by an internal loop,
//! so a test can advance it a block at a time and inspect what came out.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Result, anyhow};
use rtrb::{Producer, RingBuffer};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    audio::{
        decode::Decoder,
        dsp::{Chain, Settings},
        native::sink::{Output, SinkState},
    },
    model::PlayerEvent,
};

/// How much audio the ring holds.
///
/// Long enough that an ordinary hitch -- a page fault, the scanner taking a
/// lock, the scheduler looking elsewhere -- passes unnoticed, and short enough
/// that a seek does not have to throw much away.
const BUFFER_MS: u64 = 500;

/// What the application asks the engine to do.
#[derive(Debug)]
pub enum Command {
    Load { path: PathBuf, position_ms: u64 },
    Pause(bool),
    Toggle,
    SeekAbsolute(u64),
    SeekRelative(f64),
    Volume(f64),
    ReplayGain(Option<f64>),
    Equalizer(Vec<(u32, f32)>),
    BitPerfect(bool),
    Stop,
}

/// Ties a frame the device has played to the place in the track it came from.
///
/// One mark per block, kept until the device is past it. With a single track
/// this is bookkeeping; it earns its keep when two tracks share a ring and the
/// position has to jump at exactly the frame the second one begins.
#[derive(Debug, Clone, Copy)]
struct Mark {
    output_frame: u64,
    source_frame: u64,
    sample_rate: u32,
}

struct Stream {
    decoder: Decoder,
    chain: Chain,
    producer: Producer<f32>,
    state: Arc<SinkState>,
    channels: u16,
    sample_rate: u32,
    marks: VecDeque<Mark>,
    pushed: u64,
    /// Output frame the source ran out at, once it has.
    ends_at: Option<u64>,
    announced_end: bool,
}

pub struct Engine {
    output: Box<dyn Output>,
    settings: Settings,
    events: UnboundedSender<PlayerEvent>,
    /// Read by the interface from another thread, so it is an atomic rather
    /// than something that needs a lock on the audio path.
    position_ms: Arc<AtomicU64>,
    stream: Option<Stream>,
    paused: bool,
}

impl Engine {
    pub fn new(
        output: Box<dyn Output>,
        settings: Settings,
        events: UnboundedSender<PlayerEvent>,
        position_ms: Arc<AtomicU64>,
    ) -> Self {
        Self {
            output,
            settings,
            events,
            position_ms,
            stream: None,
            paused: false,
        }
    }

    /// The device this engine is playing to.
    pub fn output_name(&self) -> String {
        self.output.name()
    }

    /// Frames the device asked for and did not get, since the stream started.
    pub fn starved_frames(&self) -> u64 {
        self.stream
            .as_ref()
            .map_or(0, |stream| stream.state.starved.load(Ordering::Relaxed))
    }

    pub fn handle(&mut self, command: Command) {
        let outcome = match command {
            Command::Load { path, position_ms } => self.load(&path, position_ms),
            Command::Pause(paused) => self.set_paused(paused),
            Command::Toggle => self.set_paused(!self.paused),
            Command::SeekAbsolute(position_ms) => self.seek(position_ms),
            Command::SeekRelative(seconds) => {
                let current = self.position_ms.load(Ordering::Relaxed) as i64;
                let wanted = (current + (seconds * 1_000.0) as i64).max(0) as u64;
                self.seek(wanted)
            }
            Command::Volume(volume) => {
                self.settings.volume = volume.clamp(0.0, 1.0);
                if let Some(stream) = &mut self.stream {
                    stream.chain.set_volume(self.settings.volume);
                }
                Ok(())
            }
            Command::ReplayGain(gain_db) => {
                self.settings.replay_gain_db = gain_db;
                if let Some(stream) = &mut self.stream {
                    stream.chain.set_replay_gain(gain_db);
                }
                Ok(())
            }
            Command::Equalizer(bands) => {
                self.settings.equalizer = bands;
                if let Some(stream) = &mut self.stream {
                    stream.chain.set_equalizer(&self.settings.equalizer);
                }
                Ok(())
            }
            Command::BitPerfect(on) => {
                self.settings.bit_perfect = on;
                if let Some(stream) = &mut self.stream {
                    stream.chain.set_bit_perfect(on);
                }
                Ok(())
            }
            Command::Stop => {
                self.stop();
                Ok(())
            }
        };

        if let Err(error) = outcome {
            let _ = self.events.send(PlayerEvent::Error(error.to_string()));
        }
    }

    /// Do one unit of work: fill whatever room the ring has, and report where
    /// the device has got to.
    ///
    /// Returns whether anything happened, so a caller looping on this knows
    /// when there is nothing to do but wait.
    pub fn step(&mut self) -> bool {
        let mut worked = self.fill();
        worked |= self.report();
        worked
    }

    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()> {
        self.stop();

        let mut decoder = Decoder::open(path)?;
        let spec = decoder.spec();
        if !self.output.supports(spec.sample_rate, spec.channels) {
            // Phase four teaches the device to follow the file, and gives the
            // chain a resampler for when it cannot. Until then, saying so is
            // better than quietly playing it at the wrong speed.
            return Err(anyhow!(
                "the output cannot play {} Hz in {} channels",
                spec.sample_rate,
                spec.channels
            ));
        }
        if position_ms > 0 {
            decoder.seek_ms(position_ms)?;
        }

        let channels = spec.channels.max(1);
        let capacity =
            (u64::from(spec.sample_rate) * BUFFER_MS / 1_000) as usize * usize::from(channels);
        let (producer, consumer) = RingBuffer::new(capacity);
        let state = Arc::new(SinkState::default());

        let mut chain = Chain::new(spec.sample_rate, usize::from(channels), &self.settings);
        chain.settle();

        self.output
            .start(spec.sample_rate, channels, consumer, Arc::clone(&state))?;
        self.output.set_paused(self.paused)?;

        if let Some(duration) = decoder.duration_ms() {
            let _ = self.events.send(PlayerEvent::Duration(duration));
        }
        self.position_ms.store(position_ms, Ordering::Relaxed);

        self.stream = Some(Stream {
            decoder,
            chain,
            producer,
            state,
            channels,
            sample_rate: spec.sample_rate,
            marks: VecDeque::new(),
            pushed: 0,
            ends_at: None,
            announced_end: false,
        });
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.paused = paused;
        self.output.set_paused(paused)?;
        let _ = self.events.send(PlayerEvent::Paused(paused));
        Ok(())
    }

    /// Seek by starting the stream again from the new position.
    ///
    /// The ring holds half a second of audio that is now wrong, and a
    /// single-producer ring has no way to take it back, so the ring and the
    /// device stream are rebuilt. That costs a device open -- a few
    /// milliseconds -- which is cheaper than the machinery it would take to
    /// let the callback discard what it is holding without ever waiting.
    fn seek(&mut self, position_ms: u64) -> Result<()> {
        let Some(stream) = &self.stream else {
            return Ok(());
        };
        let path = stream.decoder.path().to_path_buf();
        self.load(&path, position_ms)
    }

    fn stop(&mut self) {
        self.output.stop();
        self.stream = None;
        self.position_ms.store(0, Ordering::Relaxed);
    }

    /// Decode and process until the ring is full or the track runs out.
    fn fill(&mut self) -> bool {
        let Some(stream) = &mut self.stream else {
            return false;
        };
        if stream.ends_at.is_some() {
            return false;
        }

        let mut worked = false;
        loop {
            // A block is only worth decoding if all of it fits: a partial push
            // would split a block across two marks for no gain.
            let room = stream.producer.slots();
            if room < usize::from(stream.channels) * 1024 {
                break;
            }

            let latency = stream.chain.latency_frames() as u64;
            let source_frame = stream.decoder.position_frames();
            let block = match stream.decoder.next_block() {
                Ok(Some(block)) => block,
                Ok(None) => {
                    stream.ends_at = Some(stream.pushed);
                    break;
                }
                Err(error) => {
                    let _ = self.events.send(PlayerEvent::Error(error.to_string()));
                    stream.ends_at = Some(stream.pushed);
                    break;
                }
            };
            if block.len() > room {
                // A block larger than the ring's free space: wait for the
                // device to drain rather than tear it in half. The decoder
                // keeps it, so nothing is lost.
                break;
            }

            let mut processed = block.to_vec();
            stream.chain.process(&mut processed);

            stream.marks.push_back(Mark {
                output_frame: stream.pushed,
                source_frame: source_frame.saturating_sub(latency),
                sample_rate: stream.sample_rate,
            });
            let (_, left) = stream.producer.push_partial_slice(&processed);
            debug_assert!(left.is_empty(), "the room was checked before pushing");
            stream.pushed += processed.len() as u64 / u64::from(stream.channels);
            worked = true;
        }
        worked
    }

    /// Update the reported position and announce the end of the track.
    fn report(&mut self) -> bool {
        let Some(stream) = &mut self.stream else {
            return false;
        };
        let played = stream.state.played.load(Ordering::Relaxed);

        // Marks the device is past are no longer needed, except the newest of
        // them, which is the one the position is measured from.
        while stream.marks.len() > 1 && stream.marks[1].output_frame <= played {
            stream.marks.pop_front();
        }
        if let Some(mark) = stream.marks.front()
            && mark.output_frame <= played
        {
            let frames = mark.source_frame + (played - mark.output_frame);
            self.position_ms.store(
                frames * 1_000 / u64::from(mark.sample_rate),
                Ordering::Relaxed,
            );
        }

        if let Some(ends_at) = stream.ends_at
            && !stream.announced_end
            && played >= ends_at
        {
            stream.announced_end = true;
            let _ = self.events.send(PlayerEvent::EndOfFile);
            return true;
        }
        false
    }
}
