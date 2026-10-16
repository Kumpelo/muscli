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
    Images(bool),
    Shutdown,
}

pub const MUSCLI_DISCORD_APPLICATION_ID: &str = "1550289541544546394";

fn publish_presence(status: PlaybackStatus) -> bool {
    matches!(status, PlaybackStatus::Playing | PlaybackStatus::Paused)
}

pub struct DiscordPresence {
    tx: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl DiscordPresence {
    pub fn start(large_image: String, show_images: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("muscli-discord".into())
            .spawn(move || {
                let mut show_images = show_images;
                let mut client = DiscordIpcClient::new(MUSCLI_DISCORD_APPLICATION_ID);
                let mut connected = false;
                while let Ok(command) = rx.recv() {
                    match command {
                        Command::Update(presence) => {
                            if !connected {
                                connected = client.connect().is_ok();
                            }
                            if connected
                                && client
                                    .set_activity(activity(&presence, &large_image, show_images))
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
                        Command::Images(enabled) => show_images = enabled,
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

    pub fn set_images(&self, enabled: bool) {
        let _ = self.tx.send(Command::Images(enabled));
    }

    pub fn sync(&self, track: Option<&Track>, state: &PlaybackState) {
        let command = match track {
            Some(track) if publish_presence(state.status) => Command::Update(Presence {
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
        // IPC handshakes can block inside the SDK. Audio and settings must
        // never wait for Discord; the worker clears activity when it exits.
        if let Some(worker) = self.worker.take().filter(JoinHandle::is_finished) {
            let _ = worker.join();
        }
    }
}

fn activity<'a>(presence: &'a Presence, large_image: &'a str, show_images: bool) -> Activity<'a> {
    let details = limited(&presence.title, 120);
    let state = limited(&presence.artist, 120);
    let mut activity = Activity::new()
        .name("muscli")
        .activity_type(ActivityType::Listening)
        .status_display_type(StatusDisplayType::Details);
    if !details.trim().is_empty() {
        activity = activity.details(details);
    }
    if !state.trim().is_empty() {
        activity = activity.state(state);
    }
    if show_images {
        activity = activity.assets(
            Assets::new()
                .large_image(presence_image(
                    &presence.artist,
                    &presence.genre,
                    large_image,
                ))
                .large_text(limited(&presence.album, 120)),
        );
    }
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
    } else if !fallback.is_empty()
        && fallback.len() <= 128
        && fallback
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        fallback
    } else {
        "peter"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_presence() -> Presence {
        Presence {
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            genre: "Electronic".into(),
            status: PlaybackStatus::Playing,
            position_ms: 30_000,
            duration_ms: 180_000,
        }
    }

    fn sample_track() -> Track {
        Track {
            id: "private-id".into(),
            source_id: "private-source".into(),
            relative_path: "private/song.flac".into(),
            path: "/private/song.flac".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: "Artist".into(),
            release_id: String::new(),
            compilation: false,
            genre: "Rock".into(),
            year: None,
            disc_number: 1,
            track_number: 1,
            duration_ms: 180_000,
            cover_path: Some("/private/cover.jpg".into()),
            available: true,
            favorite: false,
        }
    }

    #[test]
    fn sync_sends_play_pause_resume_changes_and_clear_without_private_paths() {
        let (tx, rx) = mpsc::channel();
        let discord = DiscordPresence { tx, worker: None };
        let mut track = sample_track();
        let mut state = PlaybackState {
            duration_ms: track.duration_ms,
            ..PlaybackState::default()
        };
        for status in [
            PlaybackStatus::Playing,
            PlaybackStatus::Paused,
            PlaybackStatus::Playing,
        ] {
            state.status = status;
            discord.sync(Some(&track), &state);
            let Command::Update(presence) = rx.recv().unwrap() else {
                panic!("missing presence");
            };
            assert_eq!(presence.status, status);
            let payload = serde_json::to_string(&activity(&presence, "peter", true)).unwrap();
            assert!(!payload.contains("private"));
        }
        track.title = "Incoming MIX track".into();
        state.position_ms = 1_250;
        state.duration_ms = 240_000;
        discord.sync(Some(&track), &state);
        let Command::Update(incoming) = rx.recv().unwrap() else {
            panic!("missing incoming track");
        };
        assert_eq!(incoming.title, "Incoming MIX track");
        assert_eq!(
            (incoming.position_ms, incoming.duration_ms),
            (1_250, 240_000)
        );
        state.status = PlaybackStatus::Stopped;
        discord.sync(Some(&track), &state);
        assert!(matches!(rx.recv().unwrap(), Command::Clear));
        state.status = PlaybackStatus::Playing;
        discord.sync(None, &state);
        assert!(matches!(rx.recv().unwrap(), Command::Clear));
    }

    #[test]
    fn unavailable_ipc_never_blocks_disabling_presence() {
        let (release, blocked) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = blocked.recv();
        });
        let (tx, _rx) = mpsc::channel();
        let discord = DiscordPresence {
            tx,
            worker: Some(worker),
        };
        let (done, finished) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(discord);
            done.send(()).unwrap();
        });
        let result = finished.recv_timeout(std::time::Duration::from_secs(1));
        release.send(()).unwrap();
        dropper.join().unwrap();
        assert!(result.is_ok(), "Discord IPC blocked the caller");
    }

    #[test]
    fn image_fallback_never_publishes_a_local_path() {
        for path in [
            "/home/private/cover.jpg",
            "file:///private/cover.png",
            r"C:\private\cover.jpg",
        ] {
            assert_eq!(presence_image("Artist", "Rock", path), "peter");
        }
        assert_eq!(presence_image("Artist", "Rock", "my_asset"), "my_asset");
    }

    #[test]
    fn deutschland_preserves_tags_without_combining_artist_and_album() {
        let (tx, rx) = mpsc::channel();
        let discord = DiscordPresence { tx, worker: None };
        let mut track = sample_track();
        track.title = "Deutschland".into();
        track.artist = "Rammstein".into();
        track.album = "Rammstein".into();
        for status in [
            PlaybackStatus::Playing,
            PlaybackStatus::Paused,
            PlaybackStatus::Playing,
        ] {
            let state = PlaybackState {
                status,
                duration_ms: track.duration_ms,
                ..PlaybackState::default()
            };
            discord.sync(Some(&track), &state);
            let Command::Update(presence) = rx.recv().unwrap() else {
                panic!("missing presence");
            };
            let payload = serde_json::to_value(activity(&presence, "peter", true)).unwrap();
            assert_eq!(payload["details"], "Deutschland");
            assert_eq!(payload["state"], "Rammstein");
            assert_eq!(payload["assets"]["large_text"], "Rammstein");
            assert_eq!(
                payload.get("timestamps").is_some(),
                status == PlaybackStatus::Playing
            );
        }
    }

    #[test]
    fn metadata_casing_is_preserved_in_each_discord_field() {
        let presence = Presence {
            title: "deutschland".into(),
            artist: "RAMMSTEIN".into(),
            album: "Rammstein".into(),
            ..sample_presence()
        };
        let payload = serde_json::to_value(activity(&presence, "peter", true)).unwrap();
        assert_eq!(payload["details"], "deutschland");
        assert_eq!(payload["state"], "RAMMSTEIN");
        assert_eq!(payload["assets"]["large_text"], "Rammstein");
    }

    #[test]
    fn presence_without_images_keeps_only_title_and_artist() {
        let value = serde_json::to_value(activity(&sample_presence(), "peter", false)).unwrap();
        assert_eq!(value["details"], "Song");
        assert_eq!(value["state"], "Artist");
        assert!(value.get("assets").is_none());
        assert_eq!(value["name"], "muscli");
    }

    #[test]
    fn pause_resume_and_track_changes_refresh_metadata_and_clock() {
        let mut presence = sample_presence();
        let playing = serde_json::to_value(activity(&presence, "peter", true)).unwrap();
        assert_eq!(playing["assets"]["large_image"], "peter_dj");
        assert_eq!(playing["assets"]["large_text"], "Album");
        assert_eq!(
            playing["timestamps"]["end"].as_i64().unwrap()
                - playing["timestamps"]["start"].as_i64().unwrap(),
            180_000
        );
        presence.status = PlaybackStatus::Paused;
        let paused = serde_json::to_value(activity(&presence, "peter", true)).unwrap();
        assert_eq!(paused["details"], "Song");
        assert_eq!(paused["state"], "Artist");
        assert!(paused.get("timestamps").is_none());
        presence.status = PlaybackStatus::Playing;
        presence.title = "Next song".into();
        presence.album = "Next album".into();
        presence.position_ms = 1_000;
        let resumed = serde_json::to_value(activity(&presence, "peter", false)).unwrap();
        assert_eq!(resumed["details"], "Next song");
        assert_eq!(resumed["state"], "Artist");
        assert!(resumed.get("timestamps").is_some());
    }

    #[test]
    fn missing_artist_is_omitted_without_using_album_as_a_fallback() {
        let mut presence = sample_presence();
        presence.album.clear();
        assert_eq!(
            serde_json::to_value(activity(&presence, "peter", false)).unwrap()["state"],
            "Artist"
        );
        presence.album = "Album".into();
        presence.artist.clear();
        let payload = serde_json::to_value(activity(&presence, "peter", true)).unwrap();
        assert!(payload.get("state").is_none());
        assert_eq!(payload["assets"]["large_text"], "Album");
    }

    #[test]
    fn stopped_playback_clears_presence() {
        assert!(publish_presence(PlaybackStatus::Playing));
        assert!(publish_presence(PlaybackStatus::Paused));
        assert!(!publish_presence(PlaybackStatus::Stopped));
    }

    #[test]
    fn pause_keeps_metadata_visible_without_a_running_clock() {
        assert!(publish_presence(PlaybackStatus::Paused));
    }

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
