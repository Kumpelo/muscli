use std::{
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use discord_rich_presence::{
    DiscordIpc, DiscordIpcClient,
    activity::{Activity, ActivityType, Assets, StatusDisplayType, Timestamps},
};

use crate::model::{PlaybackState, PlaybackStatus, Track};

#[derive(Debug, Clone)]
struct Presence {
    title: String,
    artist: String,
    album: String,
    genre: String,
    status: PlaybackStatus,
    position_ms: u64,
    duration_ms: u64,
}

enum Command {
    Update(Presence),
    Clear,
    Shutdown,
}

pub struct DiscordPresence {
    tx: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl DiscordPresence {
    pub fn start(application_id: String, large_image: String) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("muscli-discord".into())
            .spawn(move || {
                let mut client = DiscordIpcClient::new(application_id);
                let mut connected = false;
                while let Ok(command) = rx.recv() {
                    match command {
                        Command::Update(presence) => {
                            if !connected {
                                connected = client.connect().is_ok();
                            }
                            if connected
                                && client
                                    .set_activity(activity(&presence, &large_image))
                                    .is_err()
                            {
                                let _ = client.close();
                                connected = false;
                            }
                        }
                        Command::Clear => {
                            if connected && client.clear_activity().is_err() {
                                let _ = client.close();
                                connected = false;
                            }
                        }
                        Command::Shutdown => {
                            if connected {
                                let _ = client.clear_activity();
                                let _ = client.close();
                            }
                            break;
                        }
                    }
                }
            })
            .ok();
        Self { tx, worker }
    }

    pub fn sync(&self, track: Option<&Track>, state: &PlaybackState) {
        let command = match track {
            Some(track) if state.status != PlaybackStatus::Stopped => Command::Update(Presence {
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: track.album.clone(),
                genre: track.genre.clone(),
                status: state.status,
                position_ms: state.position_ms,
                duration_ms: state.duration_ms,
            }),
            _ => Command::Clear,
        };
        let _ = self.tx.send(command);
    }
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn activity<'a>(presence: &'a Presence, large_image: &'a str) -> Activity<'a> {
    let details = limited(&presence.title, 120);
    let artist = limited(&presence.artist, 120);
    let large_image = presence_image(&presence.artist, &presence.genre, large_image);
    let state = if presence.status == PlaybackStatus::Paused {
        limited(&crate::t!("label.paused_presence", artist = artist), 120)
    } else {
        artist
    };
    let mut activity = Activity::new()
        .name("muscli")
        .activity_type(ActivityType::Listening)
        .status_display_type(StatusDisplayType::Details)
        .details(details)
        .state(state)
        .assets(
            Assets::new()
                .large_image(large_image)
                .large_text(limited(&presence.album, 120)),
        );
    if presence.status == PlaybackStatus::Playing && presence.duration_ms > 0 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let start = now.saturating_sub(presence.position_ms.min(i64::MAX as u64) as i64);
        let end = start.saturating_add(presence.duration_ms.min(i64::MAX as u64) as i64);
        activity = activity.timestamps(Timestamps::new().start(start).end(end));
    }
    activity
}

fn limited(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn presence_image<'a>(artist: &str, genre: &str, fallback: &'a str) -> &'a str {
    if artist
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .windows(2)
        .any(|words| words == ["daft", "punk"])
    {
        return "daft_punk";
    }
    let genre = genre.to_lowercase();
    if genre.contains("metal") {
        "peter_metal"
    } else if [
        "electronic",
        "electrónica",
        "electronica",
        "dance",
        "house",
        "techno",
        "trance",
        "edm",
        "dubstep",
        "drum and bass",
        "dnb",
    ]
    .iter()
    .any(|kind| genre.contains(kind))
    {
        "peter_dj"
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_fields_are_safely_limited() {
        assert_eq!(limited("abcdef", 4), "abcd");
        assert_eq!(limited("música", 3), "mús");
    }

    #[test]
    fn discord_image_follows_the_track_genre() {
        assert_eq!(
            presence_image("Metallica", "Heavy Metal", "peter"),
            "peter_metal"
        );
        assert_eq!(
            presence_image("Skrillex", "Electronic", "peter"),
            "peter_dj"
        );
        assert_eq!(
            presence_image("Artist", "Dance / House", "peter"),
            "peter_dj"
        );
        assert_eq!(presence_image("Artist", "Rock", "peter"), "peter");
    }

    #[test]
    fn daft_punk_asset_has_priority_over_genre() {
        assert_eq!(
            presence_image("Daft Punk", "Electronic", "peter"),
            "daft_punk"
        );
        assert_eq!(
            presence_image("Daft Punk feat. Pharrell Williams", "Dance", "peter"),
            "daft_punk"
        );
    }
}
