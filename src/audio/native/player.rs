//! The native backend, as the application sees it.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Result, anyhow};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    audio::{
        AudioBackend, Capabilities,
        dsp::Settings,
        native::{
            device::CpalOutput,
            engine::{Command, Engine, Position},
            sink::{Output, Volume},
        },
    },
    model::PlayerEvent,
};

pub struct NativePlayer {
    commands: Sender<Command>,
    position: Arc<Position>,
    /// Written straight from the caller's thread: the whole point of applying
    /// the volume at the output is that it does not queue behind the audio.
    volume: Arc<Volume>,
    /// Likewise, so that a device already started cannot play a note between
    /// the pause being asked for and the engine taking the command up.
    paused: Arc<AtomicBool>,
    /// What the listener asked for, kept so that leaving bit-perfect puts it
    /// back rather than leaving the music at full scale.
    wanted: f64,
    bit_perfect: bool,
    device: String,
    thread: Option<JoinHandle<()>>,
}

impl NativePlayer {
    /// Open a device and start the engine that feeds it.
    ///
    /// The device is opened on the engine's own thread, since a platform
    /// stream handle often may not move between threads, but the result comes
    /// back here so a busy device is an error rather than silence.
    pub fn start(
        device: Option<String>,
        settings: Settings,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Self> {
        let noise_shaping = settings.noise_shaping;
        Self::start_with(
            move |events| {
                CpalOutput::open(device.as_deref(), noise_shaping, events)
                    .map(|output| Box::new(output) as Box<dyn Output>)
            },
            settings,
            events,
        )
    }

    /// The same, against any output. The tests use this with a capture output.
    pub fn start_with<F>(
        open: F,
        settings: Settings,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Self>
    where
        F: FnOnce(UnboundedSender<PlayerEvent>) -> Result<Box<dyn Output>> + Send + 'static,
    {
        let (commands, inbox) = mpsc::channel();
        let (opened, opening) = mpsc::channel();
        let position = Arc::new(Position::default());
        let shared = Arc::clone(&position);
        let wanted = settings.volume.clamp(0.0, 1.0);
        let bit_perfect = settings.bit_perfect;
        let volume = Arc::new(Volume::default());
        volume.set(if bit_perfect { 1.0 } else { wanted });
        let shared_volume = Arc::clone(&volume);
        let paused = Arc::new(AtomicBool::new(false));
        let shared_paused = Arc::clone(&paused);

        let thread = thread::Builder::new()
            .name("muscli-audio".to_string())
            .spawn(move || {
                let output = match open(events.clone()) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = opened.send(Err(error.to_string()));
                        return;
                    }
                };
                let mut engine = Engine::new(
                    output,
                    settings,
                    events,
                    shared,
                    shared_volume,
                    shared_paused,
                );
                if opened.send(Ok(engine.output_name())).is_err() {
                    return;
                }
                drop(opened);

                loop {
                    // The engine says how long it can be left alone, so a
                    // player with nothing to play blocks instead of waking
                    // two hundred times a second.
                    let waited = match engine.idle_timeout() {
                        Some(timeout) => inbox.recv_timeout(timeout),
                        None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
                    };
                    match waited {
                        Ok(command) => {
                            engine.handle(command);
                            while let Ok(next) = inbox.try_recv() {
                                engine.handle(next);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    while engine.step() {}
                }
            })?;

        let device = match opening.recv() {
            Ok(Ok(name)) => name,
            Ok(Err(error)) => return Err(anyhow!(error)),
            Err(_) => return Err(anyhow!("the audio thread stopped before it started")),
        };

        Ok(Self {
            commands,
            position,
            volume,
            paused,
            wanted,
            bit_perfect,
            device,
            thread: Some(thread),
        })
    }

    /// The device being played to.
    pub fn device(&self) -> &str {
        &self.device
    }

    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| anyhow!("the audio thread has stopped"))
    }
}

impl Drop for NativePlayer {
    fn drop(&mut self) {
        // Dropping the sender is what ends the loop; joining afterwards makes
        // sure the device is closed before this returns, so a player that is
        // replaced does not briefly hold two.
        let (dead, _) = mpsc::channel();
        let _ = std::mem::replace(&mut self.commands, dead);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl AudioBackend for NativePlayer {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "native",
            replay_gain: true,
            equalizer: true,
            volume: true,
            gapless: true,
            bit_perfect: true,
        }
    }

    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()> {
        // Declared here rather than left to the engine. The application reads
        // the position back within the same tick, and until the engine takes
        // the command up it is still playing the track before this one.
        self.position.declare(position_ms);
        self.send(Command::Load {
            path: path.to_path_buf(),
            position_ms,
        })
    }

    fn set_prefetch(&mut self, path: Option<&Path>) -> Result<()> {
        self.send(Command::Prefetch(path.map(Path::to_path_buf)))
    }

    fn adopt_prefetch(&mut self) -> Result<()> {
        // The engine crossed into the next track by itself and said so; there
        // is no playlist entry left over to retire, as there is with mpv.
        Ok(())
    }

    fn pause(&mut self, paused: bool) -> Result<()> {
        // Set here as well as sent, so a device the engine has already started
        // is silent from the next callback rather than from whenever the
        // command is taken up.
        self.paused.store(paused, Ordering::Relaxed);
        self.send(Command::Pause(paused))
    }

    fn toggle(&mut self) -> Result<()> {
        let paused = !self.paused.load(Ordering::Relaxed);
        self.paused.store(paused, Ordering::Relaxed);
        self.send(Command::Pause(paused))
    }

    fn seek_relative(&mut self, seconds: f64) -> Result<()> {
        let wanted = (self.position.ms() as i64 + (seconds * 1_000.0) as i64).max(0) as u64;
        self.position.declare(wanted);
        self.send(Command::SeekRelative(seconds))
    }

    fn seek_absolute_ms(&mut self, position_ms: u64) -> Result<()> {
        self.position.declare(position_ms);
        self.send(Command::SeekAbsolute(position_ms))
    }

    fn set_volume(&mut self, volume: f64) -> Result<()> {
        self.wanted = volume.clamp(0.0, 1.0);
        if !self.bit_perfect {
            self.volume.set(self.wanted);
        }
        // Still told, so that a stream opened later starts at this volume.
        self.send(Command::Volume(self.wanted))
    }

    fn set_replay_gain(&mut self, gain_db: Option<f64>) -> Result<()> {
        self.send(Command::ReplayGain(gain_db))
    }

    fn set_equalizer(&mut self, bands: &[(u32, f32)]) -> Result<()> {
        self.send(Command::Equalizer(bands.to_vec()))
    }

    fn set_bit_perfect(&mut self, on: bool) -> Result<()> {
        self.bit_perfect = on;
        // Untouched means untouched: the volume is given up while it is on,
        // which is what the settings view says it costs.
        self.volume.set(if on { 1.0 } else { self.wanted });
        self.send(Command::BitPerfect(on))
    }

    fn stop(&mut self) -> Result<()> {
        self.position.declare(0);
        self.send(Command::Stop)
    }

    fn position_ms(&self) -> u64 {
        self.position.ms()
    }
}
