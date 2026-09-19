//! Audio playback backends.
//!
//! Playback sits behind a trait so a second backend can be built alongside the
//! working one instead of replacing it in a single step. mpv has twenty years
//! of handling truncated files, lying headers and devices that vanish on
//! suspend; a native pipeline earns its place by being measured against that,
//! not by being declared better.
//!
//! The trait carries no `async`. Events are pushed into a channel the
//! application owns, which keeps a backend usable behind `dyn` and puts the
//! event loop in one place rather than one per backend.

use std::path::Path;

use anyhow::Result;

pub mod decode;
pub mod dsp;
pub mod hybrid;
pub mod measure;
pub mod mpv;
pub mod native;

pub use mpv::MpvPlayer;

/// What a backend can actually do.
///
/// The interface asks rather than assumes. A backend that cannot apply a gain
/// says so, and the settings view greys the row out with a reason, instead of
/// offering a control that silently does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Shown in the interface and in `muscli doctor`.
    pub name: &'static str,
    /// Whether a ReplayGain adjustment can be applied at all.
    pub replay_gain: bool,
    pub equalizer: bool,
    /// Whether the volume can be changed. A bit-perfect path may refuse.
    pub volume: bool,
    /// Whether the next track can begin without a gap.
    pub gapless: bool,
    /// Whether the samples can be handed to the device untouched.
    pub bit_perfect: bool,
}

pub trait AudioBackend: Send {
    fn capabilities(&self) -> Capabilities;

    /// Start playing `path`, seeking to `position_ms` first.
    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()>;

    /// Hand over what should play next, or `None` to withdraw it.
    ///
    /// The backend is free to open and begin decoding it early; that early
    /// open is what makes the transition seamless.
    fn set_prefetch(&mut self, path: Option<&Path>) -> Result<()>;

    /// Acknowledge that the backend moved to the prefetched track by itself.
    ///
    /// Called after the application has caught up, so the backend can retire
    /// whatever bookkeeping the finished track needed.
    fn adopt_prefetch(&mut self) -> Result<()>;

    fn pause(&mut self, paused: bool) -> Result<()>;
    fn toggle(&mut self) -> Result<()>;
    fn seek_relative(&mut self, seconds: f64) -> Result<()>;
    fn seek_absolute_ms(&mut self, position_ms: u64) -> Result<()>;

    /// Set the volume, `0.0` to `1.0`.
    fn set_volume(&mut self, volume: f64) -> Result<()>;

    /// Apply a ReplayGain adjustment in decibels, or remove it with `None`.
    fn set_replay_gain(&mut self, gain_db: Option<f64>) -> Result<()>;

    /// Apply equaliser gains in decibels, as (centre frequency, gain) pairs.
    fn set_equalizer(&mut self, bands: &[(u32, f32)]) -> Result<()>;

    /// Hand the decoder's samples to the device untouched, or stop doing so.
    ///
    /// Only called on a backend whose capabilities say it can; the default
    /// exists so that one which cannot does not have to write a refusal it
    /// will never be asked for.
    fn set_bit_perfect(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()>;

    /// Playback position, as the backend last reported it.
    fn position_ms(&self) -> u64;
}
