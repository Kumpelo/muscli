//! Audio playback backends.
//!
//! The trait is deliberately not `async`: backends push events into a channel
//! the application owns, so they stay usable behind `dyn`.

use std::path::Path;

use anyhow::Result;

pub mod decode;
pub mod dsp;
pub mod hybrid;
pub mod measure;
pub mod mpv;
pub mod native;

pub use mpv::MpvPlayer;

/// What a backend supports. The interface asks before offering a control, so
/// a setting it cannot honour is shown as unavailable rather than ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Shown in the interface and in `muscli doctor`.
    pub name: &'static str,
    pub replay_gain: bool,
    pub equalizer: bool,
    pub volume: bool,
    /// Whether the next track can begin without a gap.
    pub gapless: bool,
    /// Whether samples can reach the device untouched.
    pub bit_perfect: bool,
}

pub trait AudioBackend: Send {
    fn capabilities(&self) -> Capabilities;

    /// Start playing `path`, seeking to `position_ms` first.
    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()>;

    /// Hand over what should play next, or `None` to withdraw it. The backend
    /// may open and decode it early; that is what makes the join seamless.
    fn set_prefetch(&mut self, path: Option<&Path>) -> Result<()>;

    /// Called once the application has caught up with a prefetched track the
    /// backend moved to on its own, so it can retire the finished one.
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
    /// Only called when [`Capabilities::bit_perfect`] is set.
    fn set_bit_perfect(&mut self, _on: bool) -> Result<()> {
        Ok(())
    }

    fn stop(&mut self) -> Result<()>;

    /// Playback position, as the backend last reported it.
    fn position_ms(&self) -> u64;
}
