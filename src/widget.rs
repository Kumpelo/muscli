//! Read-only desktop snapshots. No decoder, database handle or playback owner.
use serde::{Deserialize, Serialize};

use crate::model::{PlaybackContext, PlaybackState, PlaybackStatus, Track};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetTrack {
    /// A queue occurrence is unique within queue_revision, including duplicates.
    pub occurrence: usize,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub cover_uri: Option<String>,
    pub duration_ms: u64,
}

impl WidgetTrack {
    pub fn from_track(track: &Track, occurrence: usize) -> Self {
        Self {
            occurrence,
            id: track.id.clone(),
            title: bounded(&track.title),
            artist: bounded(track.display_artist()),
            duration_ms: track.duration_ms,
            cover_uri: track.cover_path.as_ref().and_then(|path| {
                #[cfg(unix)]
                {
                    url::Url::from_file_path(path)
                        .ok()
                        .map(String::from)
                        .filter(|uri| uri.len() <= 2048)
                }
                #[cfg(windows)]
                {
                    let _ = path;
                    None
                }
            }),
        }
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(128).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetSnapshot {
    pub schema_version: u32,
    pub available: bool,
    pub queue_revision: u64,
    pub status: String,
    pub volume: f64,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub context: PlaybackContext,
    pub logical_current: Option<WidgetTrack>,
    pub current: Option<WidgetTrack>,
    pub mix_progress: Option<f32>,
    pub upcoming: Vec<WidgetTrack>,
}

impl WidgetSnapshot {
    pub fn new(state: &PlaybackState, context: PlaybackContext) -> Self {
        Self {
            schema_version: 1,
            available: true,
            queue_revision: 0,
            status: match state.status {
                PlaybackStatus::Playing => "playing",
                PlaybackStatus::Paused => "paused",
                PlaybackStatus::Stopped => "stopped",
            }
            .into(),
            volume: state.volume,
            position_ms: state.position_ms,
            duration_ms: state.duration_ms,
            context,
            logical_current: None,
            current: None,
            mix_progress: None,
            upcoming: Vec::new(),
        }
    }
}
