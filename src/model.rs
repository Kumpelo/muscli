use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub source_id: String,
    pub relative_path: String,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    pub year: Option<i32>,
    pub disc_number: u32,
    pub track_number: u32,
    pub duration_ms: u64,
    pub cover_path: Option<PathBuf>,
    pub available: bool,
    pub favorite: bool,
}

impl Track {
    pub fn display_artist(&self) -> &str {
        if self.artist.is_empty() {
            "Unknown Artist"
        } else {
            &self.artist
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    pub cover_path: Option<PathBuf>,
    pub track_ids: Vec<String>,
    pub available_tracks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    pub name: String,
    pub track_ids: Vec<String>,
    pub album_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackStats {
    pub track_id: String,
    pub play_count: u64,
    pub total_listen_ms: u64,
    pub last_played_at: Option<i64>,
    pub resume_position_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    pub track_id: String,
    pub started_at: i64,
    pub listened_ms: u64,
    pub position_ms: u64,
    pub counted: bool,
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SmartMatch {
    All,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartRule {
    pub field: String,
    pub operator: String,
    pub value: serde_json::Value,
}

impl SmartPlaylist {
    /// What to show. A playlist muscli seeded follows the interface language;
    /// one the user made keeps the name they gave it.
    pub fn display_name(&self) -> &str {
        match &self.preset_key {
            Some(key) => crate::i18n::lookup(key),
            None => &self.name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartPlaylist {
    pub id: i64,
    /// Name as stored. For a seeded preset this is the original English text;
    /// what the interface shows comes from `preset_key`.
    pub name: String,
    /// Translation key, for the playlists muscli seeds itself. `None` means a
    /// playlist the user made, whose name is theirs and is never translated.
    #[serde(default)]
    pub preset_key: Option<String>,
    pub match_mode: SmartMatch,
    pub rules: Vec<SmartRule>,
    pub sort_field: String,
    pub descending: bool,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedQueue {
    pub id: i64,
    pub name: String,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayGainAnalysis {
    pub gain_db: f64,
    pub true_peak_db: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    Track,
    Queue,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Track,
            Self::Track => Self::Queue,
            Self::Queue => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPlayback {
    pub queue: Vec<String>,
    pub current_index: Option<usize>,
    pub position_ms: u64,
    pub volume: f64,
    pub last_nonzero_volume: Option<f64>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
}

impl Default for SavedPlayback {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            current_index: None,
            position_ms: 0,
            volume: 1.0,
            last_nonzero_volume: None,
            shuffle: false,
            repeat: RepeatMode::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Stopped,
    Paused,
    Playing,
}

#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub status: PlaybackStatus,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f64,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            status: PlaybackStatus::Stopped,
            position_ms: 0,
            duration_ms: 0,
            volume: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayerAction {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    SeekRelative(i64),
    SeekAbsolute(u64),
    SetVolume(f64),
    MuteToggle,
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    Quit,
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Position(u64),
    /// mpv moved within its playlist, which happens on its own when it rolls
    /// into a prefetched entry.
    PlaylistPosition(i64),
    Duration(u64),
    Paused(bool),
    Volume(f64),
    EndOfFile,
    /// Something the listener should know that is not a failure, such as a
    /// setting that could not be honoured for this particular file. Unlike
    /// [`PlayerEvent::Error`] it does not skip the track.
    Notice(String),
    Error(String),
}

pub fn parse_slash_number(value: Option<&str>) -> u32 {
    value
        .and_then(|v| v.split('/').next())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

pub fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_track_fraction() {
        assert_eq!(parse_slash_number(Some("03/12")), 3);
        assert_eq!(parse_slash_number(Some("nope")), 0);
        assert_eq!(parse_slash_number(None), 0);
    }

    #[test]
    fn repeat_cycles() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::Track);
        assert_eq!(RepeatMode::Track.next(), RepeatMode::Queue);
        assert_eq!(RepeatMode::Queue.next(), RepeatMode::Off);
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(normalize_text("  Miles   Davis\n"), "Miles Davis");
    }
}
