//! The native backend, as the application sees it.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Result, anyhow};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    audio::{
        AudioBackend, Capabilities,
        dsp::Settings,
        native::{
            device::CpalOutput,
            engine::{Command, Engine},
            sink::Output,
        },
    },
    model::PlayerEvent,
};

/// How long the engine waits for a command before topping the ring up anyway.
///
/// The ring holds half a second, so this could be far longer; it is short
/// because a command that arrives just after a wait began should not sit for
/// the rest of it.
const IDLE: Duration = Duration::from_millis(5);

pub struct NativePlayer {
    commands: Sender<Command>,
    position_ms: Arc<AtomicU64>,
    device: String,
    thread: Option<JoinHandle<()>>,
}

impl NativePlayer {
    /// Open a device and start the engine that feeds it.
    ///
    /// The device is opened on the engine's own thread, because a platform
    /// stream handle frequently may not be moved to another one. The result of
    /// opening it comes back here, so a missing or busy device is an error
    /// from this call rather than a player that silently never plays.
    pub fn start(
        device: Option<String>,
        settings: Settings,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Self> {
        Self::start_with(
            move |events| {
                CpalOutput::open(device.as_deref(), events)
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
        let position_ms = Arc::new(AtomicU64::new(0));
        let shared = Arc::clone(&position_ms);

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
                let mut engine = Engine::new(output, settings, events, shared);
                if opened.send(Ok(engine.output_name())).is_err() {
                    return;
                }
                drop(opened);

                loop {
                    match inbox.recv_timeout(IDLE) {
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
            position_ms,
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
            // Not yet: the engine plays one file at a time. Saying so is what
            // keeps the interface from promising a seamless join it cannot
            // deliver.
            gapless: false,
        }
    }

    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()> {
        self.send(Command::Load {
            path: path.to_path_buf(),
            position_ms,
        })
    }

    fn set_prefetch(&mut self, _path: Option<&Path>) -> Result<()> {
        Ok(())
    }

    fn adopt_prefetch(&mut self) -> Result<()> {
        Ok(())
    }

    fn pause(&mut self, paused: bool) -> Result<()> {
        self.send(Command::Pause(paused))
    }

    fn toggle(&mut self) -> Result<()> {
        self.send(Command::Toggle)
    }

    fn seek_relative(&mut self, seconds: f64) -> Result<()> {
        self.send(Command::SeekRelative(seconds))
    }

    fn seek_absolute_ms(&mut self, position_ms: u64) -> Result<()> {
        self.send(Command::SeekAbsolute(position_ms))
    }

    fn set_volume(&mut self, volume: f64) -> Result<()> {
        self.send(Command::Volume(volume))
    }

    fn set_replay_gain(&mut self, gain_db: Option<f64>) -> Result<()> {
        self.send(Command::ReplayGain(gain_db))
    }

    fn set_equalizer(&mut self, bands: &[(u32, f32)]) -> Result<()> {
        self.send(Command::Equalizer(bands.to_vec()))
    }

    fn stop(&mut self) -> Result<()> {
        self.send(Command::Stop)
    }

    fn position_ms(&self) -> u64 {
        self.position_ms.load(Ordering::Relaxed)
    }
}
