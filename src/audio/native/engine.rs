//! The thread that decodes, processes and fills the ring.
//!
//! Everything that can block -- opening a file, decoding, allocating --
//! happens here, so the device callback only ever copies out of the ring.
//!
//! Driven by [`Engine::step`] rather than an internal loop, so a test can
//! advance it a block at a time.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use rtrb::{Producer, RingBuffer};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    audio::{
        decode::{Decoder, StreamSpec},
        dsp::{Chain, Settings, resample::Resampler},
        native::sink::{Output, OutputFormat, SinkState, Volume},
    },
    model::PlayerEvent,
    t,
};

/// How much audio the ring holds: long enough to ride out a page fault or the
/// scheduler looking elsewhere, short enough that a seek discards little.
const BUFFER_MS: u64 = 500;

/// Silence fed to a converter at the end of a track to walk its filter out.
const TAIL_FRAMES: usize = 2_048;

/// How often the engine looks at the ring while audio is playing.
const BUSY: Duration = Duration::from_millis(5);

/// The same while paused, when the ring is not draining.
const RESTING: Duration = Duration::from_millis(100);

/// How often a device that keeps running short is allowed to say so.
const STARVE_NOTICE_EVERY: Duration = Duration::from_secs(5);

/// The playing position, and a stamp saying which track it belongs to.
///
/// The application reads the position back in the same tick it asks for a
/// track, while the engine is still playing the previous one. The stamp marks
/// a reading from before the change so it can be dropped rather than stored
/// as the new track's resume point.
#[derive(Debug, Default)]
pub struct Position {
    ms: AtomicU64,
    epoch: AtomicU64,
}

impl Position {
    /// State a position from the application's side, retiring whatever the
    /// engine has not caught up with yet.
    pub fn declare(&self, ms: u64) {
        self.ms.store(ms, Ordering::Relaxed);
        self.epoch.fetch_add(1, Ordering::Release);
    }

    pub fn ms(&self) -> u64 {
        self.ms.load(Ordering::Relaxed)
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Report from the engine, ignored if the application has moved on.
    fn report(&self, epoch: u64, ms: u64) {
        if self.epoch() == epoch {
            self.ms.store(ms, Ordering::Relaxed);
        }
    }
}

/// What the application asks the engine to do.
#[derive(Debug)]
pub enum Command {
    Load { path: PathBuf, position_ms: u64 },
    Prefetch(Option<PathBuf>),
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
/// One per block, kept until the device is past it.
///
/// Held in milliseconds of the source rather than source frames, so a stream
/// being converted needs no arithmetic to undo.
#[derive(Debug, Clone, Copy)]
struct Mark {
    output_frame: u64,
    source_ms: u64,
}

struct Stream {
    decoder: Decoder,
    /// What was opened, so the stream can be rebuilt where it stands when a
    /// setting changes what the device negotiation would decide.
    path: PathBuf,
    /// The shape the stream was negotiated for. A container that changes rate
    /// or channels part way through has to be renegotiated, not carried on
    /// with, or it plays at the wrong speed.
    source_spec: StreamSpec,
    chain: Chain,
    /// Present only when the device would not take the file's own rate.
    resampler: Option<Resampler>,
    /// Set when a mono file is going to a device that only does stereo.
    duplicate: bool,
    producer: Producer<f32>,
    state: Arc<SinkState>,
    channels: u16,
    sample_rate: u32,
    /// Processed audio waiting for room in the ring. Converting changes a
    /// block's length, so how much comes out is not known until it is done.
    staging: Vec<f32>,
    marks: VecDeque<Mark>,
    pushed: u64,
    /// Samples the ring holds, so the staging area knows when to stop.
    capacity: usize,
    /// Output frames at which a following track begins, in order.
    handovers: VecDeque<u64>,
    /// A flush the output has been asked for and not yet carried out. While
    /// set, the engine decodes but does not push: what it wrote would be
    /// thrown away with the audio it is replacing.
    awaiting_flush: Option<u64>,
    /// Set when the decoder has no more to give. Not final: a track queued
    /// after this point can still join, since nothing has been announced.
    exhausted: bool,
    /// Whether a converter still has a tail to walk out. Deferred until the
    /// end is certain, because that tail is silence.
    needs_flush: bool,
    /// Output frame the audio runs out at, once everything has been staged.
    ends_at: Option<u64>,
    announced_end: bool,
}

impl Stream {
    /// Put processed audio in the queue for the ring, with a mark saying
    /// where in the track it came from.
    fn stage(&mut self, block: Vec<f32>, source_ms: u64) {
        let mut block = block;
        if self.duplicate {
            block = block.iter().flat_map(|sample| [*sample, *sample]).collect();
        }
        self.chain.process(&mut block);

        let ahead = self.staging.len() as u64 / u64::from(self.channels);
        self.marks.push_back(Mark {
            output_frame: self.pushed + ahead,
            source_ms,
        });
        self.staging.extend_from_slice(&block);
    }
}

pub struct Engine {
    output: Box<dyn Output>,
    settings: Settings,
    events: UnboundedSender<PlayerEvent>,
    /// Read by the interface from another thread, so it is an atomic rather
    /// than something that needs a lock on the audio path.
    position: Arc<Position>,
    /// The application's position stamp as of the last command handled here.
    epoch: u64,
    stream: Option<Stream>,
    /// The track queued behind the one playing, opened early so the join
    /// costs no file opening at the moment it has to be seamless.
    prefetch: Option<Decoder>,
    /// Applied on the way out rather than here, so it reaches audio that is
    /// already decoded and waiting.
    volume: Arc<Volume>,
    /// Read by the output on every callback; the engine keeps a copy to decide
    /// how long it may sleep.
    paused: Arc<AtomicBool>,
    /// Set when the stream has to be rebuilt where it stands, because the
    /// file turned out to be a different shape than it was opened as.
    needs_reload: bool,
    /// Frames already announced as missed, so a shortfall is reported once.
    reported_starved: u64,
    last_starve_notice: Option<Instant>,
}

impl Engine {
    pub fn new(
        output: Box<dyn Output>,
        settings: Settings,
        events: UnboundedSender<PlayerEvent>,
        position: Arc<Position>,
        volume: Arc<Volume>,
        paused: Arc<AtomicBool>,
    ) -> Self {
        let epoch = position.epoch();
        Self {
            output,
            settings,
            events,
            position,
            epoch,
            volume,
            paused,
            stream: None,
            prefetch: None,
            needs_reload: false,
            reported_starved: 0,
            last_starve_notice: None,
        }
    }

    /// The device this engine is playing to.
    pub fn output_name(&self) -> String {
        self.output.name()
    }

    /// How long the thread may wait before looking at the ring again. `None`
    /// means nothing is playing, so it can block until a command arrives.
    pub fn idle_timeout(&self) -> Option<Duration> {
        match (&self.stream, self.paused.load(Ordering::Relaxed)) {
            (None, _) => None,
            (Some(_), true) => Some(RESTING),
            (Some(_), false) => Some(BUSY),
        }
    }

    /// Frames the device asked for and did not get, since the stream started.
    pub fn starved_frames(&self) -> u64 {
        self.stream
            .as_ref()
            .map_or(0, |stream| stream.state.starved.load(Ordering::Relaxed))
    }

    pub fn handle(&mut self, command: Command) {
        // Commands arrive in the order they were issued, and each one may be
        // the application declaring a new position, so the engine adopts the
        // stamp as it takes the command up.
        self.epoch = self.position.epoch();
        let outcome = match command {
            Command::Load { path, position_ms } => {
                self.prefetch = None;
                self.load(&path, position_ms)
            }
            Command::Prefetch(path) => {
                self.prefetch = match path {
                    // Opened now rather than when the current track ends:
                    // opening a file is the one thing in the handover that
                    // could take long enough to be heard.
                    Some(path) => match Decoder::open(&path) {
                        Ok(decoder) => Some(decoder),
                        Err(error) => {
                            let _ = self.events.send(PlayerEvent::Error(error.to_string()));
                            None
                        }
                    },
                    None => None,
                };
                Ok(())
            }
            Command::Pause(paused) => self.set_paused(paused),
            Command::Toggle => {
                let paused = self.paused.load(Ordering::Relaxed);
                self.set_paused(!paused)
            }
            Command::SeekAbsolute(position_ms) => self.seek(position_ms),
            Command::SeekRelative(seconds) => {
                let current = self.position.ms() as i64;
                let wanted = (current + (seconds * 1_000.0) as i64).max(0) as u64;
                self.seek(wanted)
            }
            Command::Volume(volume) => {
                self.settings.volume = volume.clamp(0.0, 1.0);
                self.volume.set(self.settings.volume);
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
                // Rebuilt rather than switched: whether the samples can reach
                // the device untouched was decided when the stream was opened,
                // from the rate and channel count the device would take. A
                // flag on the chain would leave a resampler running underneath
                // a setting that says nothing is being converted.
                self.reload_in_place()
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

    /// Fill whatever room the ring has and report where the device has got
    /// to. Returns whether anything happened.
    pub fn step(&mut self) -> bool {
        if self.needs_reload {
            self.needs_reload = false;
            if let Err(error) = self.reload_in_place() {
                let _ = self.events.send(PlayerEvent::Error(error.to_string()));
            }
            return true;
        }

        let mut worked = self.fill();
        worked |= self.report();
        worked
    }

    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()> {
        self.stop();

        let mut decoder = Decoder::open(path)?;
        let spec = decoder.spec();
        let source_channels = spec.channels.max(1);

        // The device follows the file wherever it can; only when it will not
        // is anything converted. One answer rather than three questions,
        // because the rate, the channel count and the sample format constrain
        // each other.
        let format = self.output.negotiate(spec).ok_or_else(|| {
            anyhow!(
                "the output cannot play {} Hz in {source_channels} channels",
                spec.sample_rate
            )
        })?;
        let OutputFormat {
            sample_rate,
            channels,
            ..
        } = format;
        let duplicate = channels == 2 && source_channels == 1;
        // Converting the rate or the channel count is what bit-perfect exists
        // to rule out, so a device that will not match the file turns it off
        // for this track and says so.
        let untouched = !self.settings.bit_perfect
            || (sample_rate == spec.sample_rate && channels == source_channels);
        if !untouched {
            let _ = self.events.send(PlayerEvent::Notice(t!(
                "status.bit_perfect_rate",
                rate = spec.sample_rate,
                channels = source_channels
            )));
        }

        let resampler = (sample_rate != spec.sample_rate)
            .then(|| Resampler::new(spec.sample_rate, sample_rate, usize::from(source_channels)))
            .transpose()?;

        if position_ms > 0 {
            decoder.seek_ms(position_ms)?;
        }

        let capacity =
            (u64::from(sample_rate) * BUFFER_MS / 1_000) as usize * usize::from(channels);
        let (producer, consumer) = RingBuffer::new(capacity);
        let state = Arc::new(SinkState::new(
            Arc::clone(&self.volume),
            Arc::clone(&self.paused),
        ));

        let mut chain = Chain::new(sample_rate, usize::from(channels), &self.settings);
        chain.set_bit_perfect(self.settings.bit_perfect && untouched);
        chain.settle();

        if let Some(duration) = decoder.duration_ms() {
            let _ = self.events.send(PlayerEvent::Duration(duration));
        }
        self.position.report(self.epoch, position_ms);

        self.reported_starved = 0;
        self.stream = Some(Stream {
            decoder,
            path: path.to_path_buf(),
            source_spec: spec,
            chain,
            resampler,
            duplicate,
            producer,
            state: Arc::clone(&state),
            channels,
            sample_rate,
            staging: Vec::new(),
            capacity,
            marks: VecDeque::new(),
            handovers: VecDeque::new(),
            awaiting_flush: None,
            pushed: 0,
            exhausted: false,
            needs_flush: false,
            ends_at: None,
            announced_end: false,
        });

        // Decode into the ring before the device is told about it: a device
        // asks for samples the instant it is opened, and an empty ring then
        // means every track and every seek begins with a dropout.
        self.fill();

        if let Err(error) = self.output.start(format, consumer, state) {
            self.stream = None;
            return Err(error);
        }
        Ok(())
    }

    /// Open the current track again where it is playing, renegotiating the
    /// device.
    fn reload_in_place(&mut self) -> Result<()> {
        let Some((path, position_ms)) = self
            .stream
            .as_ref()
            .map(|stream| (stream.path.clone(), self.position.ms()))
        else {
            return Ok(());
        };
        self.load(&path, position_ms)
    }

    fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.paused.store(paused, Ordering::Relaxed);
        let _ = self.events.send(PlayerEvent::Paused(paused));
        Ok(())
    }

    /// Seek without rebuilding the output stream.
    ///
    /// The ring holds up to half a second belonging to the old position.
    /// Rebuilding it would mean asking the driver for the device again, so the
    /// output discards it instead and reports the frame playing resumes at.
    fn seek(&mut self, position_ms: u64) -> Result<()> {
        let Some(stream) = &mut self.stream else {
            return Ok(());
        };

        stream.decoder.seek_ms(position_ms)?;
        stream.chain.reset();
        if let Some(resampler) = &mut stream.resampler {
            resampler.reset();
        }

        // Everything measured from the old position goes, including the
        // counters the marks are relative to: they are rebased on the frame
        // the output reports once it has done its part.
        stream.staging.clear();
        stream.marks.clear();
        stream.handovers.clear();
        stream.pushed = 0;
        stream.exhausted = false;
        stream.needs_flush = false;
        stream.ends_at = None;
        stream.announced_end = false;

        let generation = stream.state.flush.load(Ordering::Relaxed) + 1;
        stream.awaiting_flush = Some(generation);
        stream.state.flush.store(generation, Ordering::Release);

        self.position
            .report(self.epoch, stream.decoder.position_ms());
        Ok(())
    }

    fn stop(&mut self) {
        self.output.stop();
        self.stream = None;
        self.position.report(self.epoch, 0);
    }

    /// Decode and process until the ring is full or the track runs out.
    fn fill(&mut self) -> bool {
        let Some(stream) = &mut self.stream else {
            return false;
        };
        let mut worked = false;

        if let Some(generation) = stream.awaiting_flush
            && stream.state.flushed.load(Ordering::Acquire) == generation
        {
            // The output has thrown away the old audio and said which frame
            // the new begins at. Everything counted from the seek is rebased
            // on it, and what was decoded meanwhile can now go out.
            let resumes_at = stream.state.flushed_at.load(Ordering::Relaxed);
            stream.pushed += resumes_at;
            for mark in &mut stream.marks {
                mark.output_frame += resumes_at;
            }
            for handover in &mut stream.handovers {
                *handover += resumes_at;
            }
            // Whatever silence the seek itself cost is the seek, not the
            // decoder falling behind.
            self.reported_starved = stream.state.starved.load(Ordering::Relaxed);
            stream.awaiting_flush = None;
            worked = true;
        }

        // Nothing may be pushed until then: anything written now would be
        // thrown away along with the audio it is replacing. Decoding carries
        // on regardless, so the music is ready the instant it may go out.
        let holding = stream.awaiting_flush.is_some();

        loop {
            if holding {
                if stream.staging.len() >= stream.capacity {
                    break;
                }
            } else if !stream.staging.is_empty() {
                let (_, left) = stream.producer.push_partial_slice(&stream.staging);
                let taken = stream.staging.len() - left.len();
                stream.staging.drain(..taken);
                stream.pushed += taken as u64 / u64::from(stream.channels);
                worked |= taken > 0;
                if !stream.staging.is_empty() {
                    // The ring is full. Everything left keeps its place.
                    break;
                }
            }

            if stream.exhausted {
                // A track queued late can still join. Nothing has been
                // announced, so appending it keeps the audio continuous
                // however long the application took to arm it.
                if !stream.announced_end
                    && let Some(next) = self
                        .prefetch
                        .take_if(|next| next.spec() == stream.decoder.spec())
                {
                    if let Some(duration) = next.duration_ms() {
                        let _ = self.events.send(PlayerEvent::Duration(duration));
                    }
                    stream.decoder = next;
                    stream.exhausted = false;
                    stream.needs_flush = false;
                    stream.ends_at = None;
                    // Plus the chain's delay: at that frame the device is
                    // still playing the limiter's hold of the last track.
                    stream.handovers.push_back(
                        stream.pushed
                            + stream.staging.len() as u64 / u64::from(stream.channels)
                            + stream.chain.latency_frames() as u64,
                    );
                    worked = true;
                    continue;
                }

                if stream.needs_flush {
                    stream.needs_flush = false;
                    let source_ms = stream.decoder.position_ms();
                    if let Some(resampler) = &mut stream.resampler {
                        let channels =
                            usize::from(stream.channels) / if stream.duplicate { 2 } else { 1 };
                        let tail = vec![0.0; TAIL_FRAMES * channels];
                        let mut flushed = Vec::new();
                        if resampler.process(&tail, &mut flushed).is_ok() {
                            stream.stage(flushed, source_ms);
                        }
                    }
                    continue;
                }

                // Where the audio ends is a frame of the output, and while a
                // flush is pending there is no such frame yet.
                if !holding && stream.ends_at.is_none() {
                    stream.ends_at = Some(stream.pushed);
                }
                break;
            }
            if !holding && stream.producer.slots() == 0 {
                break;
            }

            let latency_ms =
                stream.chain.latency_frames() as u64 * 1_000 / u64::from(stream.sample_rate.max(1));
            let source_ms = stream.decoder.position_ms().saturating_sub(latency_ms);

            let mut block = match stream.decoder.next_block() {
                Ok(Some(block)) => block.to_vec(),
                Ok(None) => {
                    stream.exhausted = true;
                    stream.needs_flush = stream.resampler.is_some();
                    continue;
                }
                Err(error) => {
                    let _ = self.events.send(PlayerEvent::Error(error.to_string()));
                    stream.exhausted = true;
                    continue;
                }
            };

            // A few containers change rate or channel count part way through.
            // The device was negotiated for what the file said at the start,
            // so carrying on would play the rest at the wrong speed.
            if stream.decoder.spec() != stream.source_spec {
                self.needs_reload = true;
                break;
            }

            if let Some(resampler) = &mut stream.resampler {
                let mut converted = Vec::new();
                if let Err(error) = resampler.process(&block, &mut converted) {
                    let _ = self.events.send(PlayerEvent::Error(error.to_string()));
                    stream.exhausted = true;
                    continue;
                }
                block = converted;
                if block.is_empty() {
                    // The converter is still filling; nothing to stage yet.
                    continue;
                }
            }
            stream.stage(block, source_ms);
            worked = true;
        }
        worked
    }

    /// Update the reported position and announce the end of the track.
    fn report(&mut self) -> bool {
        let Some(stream) = &mut self.stream else {
            return false;
        };
        if stream.awaiting_flush.is_some() {
            // The marks are counted from zero until the flush lands, while
            // the device is somewhere else. The seek already set the position.
            return false;
        }
        let played = stream.state.played.load(Ordering::Relaxed);

        // Marks the device is past are no longer needed, except the newest of
        // them, which is the one the position is measured from.
        while stream.marks.len() > 1 && stream.marks[1].output_frame <= played {
            stream.marks.pop_front();
        }
        if let Some(mark) = stream.marks.front()
            && mark.output_frame <= played
        {
            let elapsed = (played - mark.output_frame) * 1_000 / u64::from(stream.sample_rate);
            self.position.report(self.epoch, mark.source_ms + elapsed);
        }

        while stream
            .handovers
            .front()
            .is_some_and(|frame| *frame <= played)
        {
            stream.handovers.pop_front();
            // The same signal mpv gives when it rolls into the next playlist
            // entry, so the application's bookkeeping is the same either way.
            let _ = self.events.send(PlayerEvent::PlaylistPosition(1));
        }

        // Running dry at the very end is not a fault: the track finished and
        // the application has not loaded another yet. Anywhere else it means
        // the decoder could not keep up, and the listener heard it.
        let inside_the_track = stream.ends_at.is_none_or(|ends_at| played < ends_at);
        let starved = stream.state.starved.load(Ordering::Relaxed);
        if inside_the_track && starved > self.reported_starved {
            let missed = starved - self.reported_starved;
            let due = self
                .last_starve_notice
                .is_none_or(|at| at.elapsed() >= STARVE_NOTICE_EVERY);
            if due {
                let milliseconds = missed * 1_000 / u64::from(stream.sample_rate.max(1));
                let _ = self.events.send(PlayerEvent::Notice(t!(
                    "status.audio_underrun",
                    ms = milliseconds.max(1)
                )));
                self.last_starve_notice = Some(Instant::now());
            }
            self.reported_starved = starved;
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
