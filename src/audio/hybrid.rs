//! The native path where it works, mpv where it does not.
//!
//! The native decoder reads FLAC, MP3, AAC, ALAC, Vorbis, WAV and AIFF. It
//! does not read Opus, WavPack or Monkey's Audio, and pretending otherwise
//! would mean a library that mostly plays. mpv reads all of them, so the two
//! sit behind one interface and each track goes to whichever can play it.
//!
//! Which one that is comes from opening the file, not from its extension: an
//! Opus stream inside an `.ogg` container looks exactly like a Vorbis one
//! from the outside, and only the decoder knows the difference. The extra
//! open costs about a millisecond per track change.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    audio::{
        AudioBackend, Capabilities, MpvPlayer, decode::Decoder, dsp::Settings,
        native::player::NativePlayer,
    },
    model::PlayerEvent,
};

pub struct HybridPlayer {
    native: NativePlayer,
    /// Started the first time a file needs it. A listener whose library the
    /// native path can read never has an mpv process at all.
    mpv: Option<MpvPlayer>,
    socket: PathBuf,
    events: UnboundedSender<PlayerEvent>,
    on_mpv: bool,
    /// Kept so that whichever backend takes over a track starts where the
    /// other left off rather than at its own defaults.
    volume: f64,
    replay_gain: Option<f64>,
    equalizer: Vec<(u32, f32)>,
    bit_perfect: bool,
}

impl HybridPlayer {
    pub fn start(
        device: Option<String>,
        settings: Settings,
        socket: &Path,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Self> {
        let volume = settings.volume;
        let equalizer = settings.equalizer.clone();
        let bit_perfect = settings.bit_perfect;
        let native = NativePlayer::start(device, settings, events.clone())?;

        Ok(Self {
            native,
            mpv: None,
            socket: socket.to_path_buf(),
            events,
            on_mpv: false,
            volume,
            replay_gain: None,
            equalizer,
            bit_perfect,
        })
    }

    /// The device the native path is playing to.
    pub fn device(&self) -> &str {
        self.native.device()
    }

    /// Whether the native decoder can read this file.
    fn native_can_play(path: &Path) -> bool {
        Decoder::open(path).is_ok()
    }

    fn mpv(&mut self) -> Result<&mut MpvPlayer> {
        if self.mpv.is_none() {
            self.mpv = Some(MpvPlayer::start(&self.socket, self.events.clone())?);
        }
        Ok(self.mpv.as_mut().expect("just started"))
    }

    /// Move to the backend that can play `path`, carrying the settings over.
    fn choose(&mut self, path: &Path) -> Result<()> {
        let wants_mpv = !Self::native_can_play(path);
        if wants_mpv == self.on_mpv {
            return Ok(());
        }

        // Whichever is being left keeps hold of a device or a socket until it
        // is told to stop, and two backends holding one card is how a second
        // track fails to start.
        if self.on_mpv {
            if let Some(mpv) = &mut self.mpv {
                mpv.stop()?;
            }
        } else {
            self.native.stop()?;
        }
        self.on_mpv = wants_mpv;
        if wants_mpv && self.bit_perfect {
            let _ = self.events.send(PlayerEvent::Notice(
                "bit-perfect is off for this track: only mpv can read it".to_string(),
            ));
        }

        let volume = self.volume;
        let replay_gain = self.replay_gain;
        let equalizer = self.equalizer.clone();
        let taking_over = self.active();
        taking_over.set_volume(volume)?;
        taking_over.set_replay_gain(replay_gain)?;
        taking_over.set_equalizer(&equalizer)?;
        Ok(())
    }

    fn active(&mut self) -> &mut dyn AudioBackend {
        if self.on_mpv {
            match &mut self.mpv {
                Some(mpv) => mpv,
                // Only reachable if starting mpv failed, in which case the
                // native backend is the one that is running.
                None => &mut self.native,
            }
        } else {
            &mut self.native
        }
    }
}

impl AudioBackend for HybridPlayer {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "native + mpv",
            replay_gain: true,
            equalizer: true,
            volume: true,
            // Only on the native side; a file that has to go to mpv cannot
            // be played untouched, and the interface says so when it happens.
            gapless: true,
            bit_perfect: true,
        }
    }

    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()> {
        if !Self::native_can_play(path) {
            // Starting mpv can fail on a machine that does not have it, and
            // the message for that has to arrive before the load does.
            self.mpv()?;
        }
        self.choose(path)?;
        self.active().load(path, position_ms)
    }

    fn set_prefetch(&mut self, path: Option<&Path>) -> Result<()> {
        // A track that would play on the other backend cannot be joined to
        // this one, so nothing is armed and the ordinary end of file moves
        // the queue on. Losing the seamless join between, say, a FLAC and an
        // Opus is not a loss: they were never one recording.
        let armed = path.filter(|path| Self::native_can_play(path) != self.on_mpv);
        self.active().set_prefetch(armed)
    }

    fn adopt_prefetch(&mut self) -> Result<()> {
        self.active().adopt_prefetch()
    }

    fn pause(&mut self, paused: bool) -> Result<()> {
        self.active().pause(paused)
    }

    fn toggle(&mut self) -> Result<()> {
        self.active().toggle()
    }

    fn seek_relative(&mut self, seconds: f64) -> Result<()> {
        self.active().seek_relative(seconds)
    }

    fn seek_absolute_ms(&mut self, position_ms: u64) -> Result<()> {
        self.active().seek_absolute_ms(position_ms)
    }

    fn set_volume(&mut self, volume: f64) -> Result<()> {
        self.volume = volume;
        self.active().set_volume(volume)
    }

    fn set_replay_gain(&mut self, gain_db: Option<f64>) -> Result<()> {
        self.replay_gain = gain_db;
        self.active().set_replay_gain(gain_db)
    }

    fn set_equalizer(&mut self, bands: &[(u32, f32)]) -> Result<()> {
        self.equalizer = bands.to_vec();
        self.active().set_equalizer(bands)
    }

    fn set_bit_perfect(&mut self, on: bool) -> Result<()> {
        self.bit_perfect = on;
        self.native.set_bit_perfect(on)
    }

    fn stop(&mut self) -> Result<()> {
        self.active().stop()
    }

    fn position_ms(&self) -> u64 {
        if self.on_mpv {
            match &self.mpv {
                Some(mpv) => mpv.position_ms(),
                None => self.native.position_ms(),
            }
        } else {
            self.native.position_ms()
        }
    }
}
