use std::{
    collections::{HashMap, HashSet},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    album_artist: String,
    album: String,
    release_id: String,
    year: Option<i32>,
    genre: String,
    status: PlaybackStatus,
    position_ms: u64,
    duration_ms: u64,
}

enum Command {
    Update(Presence),
    CoverResolved(AlbumIdentity, Result<Option<String>, ()>),
    Clear,
    Images(bool),
    Shutdown,
}

pub const MUSCLI_DISCORD_APPLICATION_ID: &str = "1550289541544546394";

fn publish_presence(status: PlaybackStatus) -> bool {
    status == PlaybackStatus::Playing
}

pub struct DiscordPresence {
    tx: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl DiscordPresence {
    pub fn start(large_image: String, show_images: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let updates = tx.clone();
        let worker = thread::Builder::new()
            .name("muscli-discord".into())
            .spawn(move || {
                let (cover_requests, requested_covers) = mpsc::channel();
                let cover_worker = thread::Builder::new()
                    .name("muscli-discord-covers".into())
                    .spawn(move || {
                        let mut resolver = AlbumCoverResolver::new();
                        while let Ok(identity) = requested_covers.recv() {
                            let cover = resolver.resolve(&identity);
                            if updates
                                .send(Command::CoverResolved(identity, cover))
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .ok();
                let mut show_images = show_images;
                let mut covers = HashMap::<AlbumIdentity, Option<String>>::new();
                let mut pending_covers = HashSet::new();
                let mut current = None::<Presence>;
                let mut client = DiscordIpcClient::new(MUSCLI_DISCORD_APPLICATION_ID);
                let mut connected = false;
                while let Ok(command) = rx.recv() {
                    match command {
                        Command::Update(presence) => {
                            let identity = AlbumIdentity::from_presence(&presence);
                            let cover = covers.get(&identity).and_then(Option::as_deref);
                            if !connected {
                                connected = client.connect().is_ok();
                            }
                            if connected
                                && client
                                    .set_activity(activity(
                                        &presence,
                                        &large_image,
                                        show_images,
                                        cover,
                                    ))
                                    .is_err()
                            {
                                let _ = client.close();
                                connected = false;
                            }
                            if show_images
                                && !covers.contains_key(&identity)
                                && pending_covers.insert(identity.clone())
                            {
                                let _ = cover_requests.send(identity);
                            }
                            current = Some(presence);
                        }
                        Command::CoverResolved(identity, cover) => {
                            pending_covers.remove(&identity);
                            let Ok(cover) = cover else {
                                continue;
                            };
                            covers.insert(identity.clone(), cover);
                            if show_images
                                && let Some(presence) = current.as_ref().filter(|presence| {
                                    AlbumIdentity::from_presence(presence) == identity
                                })
                                && connected
                            {
                                let cover = covers.get(&identity).and_then(Option::as_deref);
                                if client
                                    .set_activity(activity(presence, &large_image, true, cover))
                                    .is_err()
                                {
                                    let _ = client.close();
                                    connected = false;
                                }
                            }
                        }
                        Command::Clear => {
                            current = None;
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
                drop(cover_requests);
                if let Some(worker) = cover_worker.filter(JoinHandle::is_finished) {
                    let _ = worker.join();
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
                album_artist: track.album_artist.clone(),
                album: track.album.clone(),
                release_id: track.release_id.clone(),
                year: track.year,
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

fn activity<'a>(
    presence: &'a Presence,
    large_image: &'a str,
    show_images: bool,
    album_cover: Option<&'a str>,
) -> Activity<'a> {
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
        let image = album_cover
            .unwrap_or_else(|| presence_image(&presence.artist, &presence.genre, large_image));
        activity = activity.assets(
            Assets::new()
                .large_image(image)
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AlbumIdentity {
    release_id: String,
    artist: String,
    title: String,
    year: Option<i32>,
}

impl AlbumIdentity {
    fn from_presence(presence: &Presence) -> Self {
        let album_artist = presence.album_artist.trim();
        Self {
            release_id: presence.release_id.trim().to_lowercase(),
            artist: if album_artist.is_empty() {
                presence.artist.trim().to_owned()
            } else {
                album_artist.to_owned()
            },
            title: presence.album.trim().to_owned(),
            year: presence.year,
        }
    }
}

struct AlbumCoverResolver {
    agent: ureq::Agent,
    last_musicbrainz_request: Option<Instant>,
}

impl AlbumCoverResolver {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .user_agent(concat!(
                "muscli/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/Kumpelo/muscli)"
            ))
            .timeout_global(Some(Duration::from_secs(4)))
            .build();
        Self {
            agent: config.into(),
            last_musicbrainz_request: None,
        }
    }

    fn resolve(&mut self, identity: &AlbumIdentity) -> Result<Option<String>, ()> {
        let release_group = if valid_mbid(&identity.release_id) {
            self.release_group_for_release(&identity.release_id)
        } else {
            self.search_release_group(identity)
        }?;
        Ok(release_group
            .filter(|id| valid_mbid(id))
            .map(|id| format!("https://coverartarchive.org/release-group/{id}/front-500")))
    }

    fn release_group_for_release(&mut self, release_id: &str) -> Result<Option<String>, ()> {
        let url = format!("https://musicbrainz.org/ws/2/release/{release_id}");
        for attempt in 0..2 {
            self.wait_for_musicbrainz();
            match self
                .agent
                .get(&url)
                .query("inc", "release-groups")
                .query("fmt", "json")
                .call()
            {
                Ok(mut response) => {
                    let value: serde_json::Value =
                        response.body_mut().read_json().map_err(|_| ())?;
                    return Ok(value["release-group"]["id"].as_str().map(str::to_owned));
                }
                Err(ureq::Error::StatusCode(404)) => return Ok(None),
                Err(_) if attempt == 0 => {}
                Err(_) => return Err(()),
            }
        }
        Err(())
    }

    fn search_release_group(&mut self, identity: &AlbumIdentity) -> Result<Option<String>, ()> {
        let Some(year) = identity.year else {
            return Ok(None);
        };
        if identity.artist.is_empty() || identity.title.is_empty() {
            return Ok(None);
        }
        let query = format!(
            "releasegroup:\"{}\" AND artist:\"{}\" AND primarytype:album",
            lucene_literal(&identity.title),
            lucene_literal(&identity.artist)
        );
        for attempt in 0..2 {
            self.wait_for_musicbrainz();
            match self
                .agent
                .get("https://musicbrainz.org/ws/2/release-group/")
                .query("query", &query)
                .query("fmt", "json")
                .query("limit", "10")
                .call()
            {
                Ok(mut response) => {
                    let value: serde_json::Value =
                        response.body_mut().read_json().map_err(|_| ())?;
                    return Ok(select_release_group(&value, identity, year));
                }
                Err(_) if attempt == 0 => {}
                Err(_) => return Err(()),
            }
        }
        Err(())
    }

    fn wait_for_musicbrainz(&mut self) {
        if let Some(last) = self.last_musicbrainz_request {
            let minimum_gap = Duration::from_millis(1_100);
            if let Some(wait) = minimum_gap.checked_sub(last.elapsed()) {
                thread::sleep(wait);
            }
        }
        self.last_musicbrainz_request = Some(Instant::now());
    }
}

fn valid_mbid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn lucene_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn comparable_album_title(value: &str) -> &str {
    value
        .trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or_else(|| value.trim())
}

fn select_release_group(
    response: &serde_json::Value,
    identity: &AlbumIdentity,
    year: i32,
) -> Option<String> {
    let matches = response["release-groups"]
        .as_array()?
        .iter()
        .filter(|candidate| {
            let score = candidate["score"].as_u64().unwrap_or_default();
            let title = candidate["title"].as_str().unwrap_or_default();
            let first_year = candidate["first-release-date"]
                .as_str()
                .and_then(|date| date.get(..4))
                .and_then(|value| value.parse::<i32>().ok());
            let artist = candidate["artist-credit"]
                .as_array()
                .and_then(|credits| credits.first())
                .and_then(|credit| credit["name"].as_str())
                .unwrap_or_default();
            score >= 95
                && first_year == Some(year)
                && artist.eq_ignore_ascii_case(&identity.artist)
                && comparable_album_title(title).eq_ignore_ascii_case(&identity.title)
        });
    let ids = matches
        .filter_map(|candidate| candidate["id"].as_str())
        .collect::<Vec<_>>();
    (ids.len() == 1).then(|| ids[0].to_owned())
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
            album_artist: "Artist".into(),
            album: "Album".into(),
            release_id: String::new(),
            year: None,
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
    fn sync_sends_play_resume_changes_and_clears_pause_without_private_paths() {
        let (tx, rx) = mpsc::channel();
        let discord = DiscordPresence { tx, worker: None };
        let mut track = sample_track();
        let mut state = PlaybackState {
            duration_ms: track.duration_ms,
            ..PlaybackState::default()
        };
        state.status = PlaybackStatus::Playing;
        discord.sync(Some(&track), &state);
        let Command::Update(presence) = rx.recv().unwrap() else {
            panic!("missing presence");
        };
        let payload = serde_json::to_string(&activity(&presence, "peter", true, None)).unwrap();
        assert!(!payload.contains("private"));
        state.status = PlaybackStatus::Paused;
        discord.sync(Some(&track), &state);
        assert!(matches!(rx.recv().unwrap(), Command::Clear));
        track.title = "Incoming MIX track".into();
        state.status = PlaybackStatus::Playing;
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
        let mut state = PlaybackState {
            status: PlaybackStatus::Playing,
            duration_ms: track.duration_ms,
            ..PlaybackState::default()
        };
        discord.sync(Some(&track), &state);
        let Command::Update(presence) = rx.recv().unwrap() else {
            panic!("missing presence");
        };
        let cover = "https://coverartarchive.org/release-group/e4d39d2e-9420-4007-881a-9b5bc23944e0/front-500";
        let payload =
            serde_json::to_value(activity(&presence, "peter", true, Some(cover))).unwrap();
        assert_eq!(payload["details"], "Deutschland");
        assert_eq!(payload["state"], "Rammstein");
        assert_eq!(payload["assets"]["large_text"], "Rammstein");
        assert_eq!(payload["assets"]["large_image"], cover);
        state.status = PlaybackStatus::Paused;
        discord.sync(Some(&track), &state);
        assert!(matches!(rx.recv().unwrap(), Command::Clear));
        state.status = PlaybackStatus::Playing;
        discord.sync(Some(&track), &state);
        let Command::Update(resumed) = rx.recv().unwrap() else {
            panic!("missing resumed presence");
        };
        assert_eq!(resumed.title, "Deutschland");
    }

    #[test]
    fn metadata_casing_is_preserved_in_each_discord_field() {
        let presence = Presence {
            title: "deutschland".into(),
            artist: "RAMMSTEIN".into(),
            album: "Rammstein".into(),
            ..sample_presence()
        };
        let payload = serde_json::to_value(activity(&presence, "peter", true, None)).unwrap();
        assert_eq!(payload["details"], "deutschland");
        assert_eq!(payload["state"], "RAMMSTEIN");
        assert_eq!(payload["assets"]["large_text"], "Rammstein");
    }

    #[test]
    fn presence_without_images_keeps_only_title_and_artist() {
        let value =
            serde_json::to_value(activity(&sample_presence(), "peter", false, None)).unwrap();
        assert_eq!(value["details"], "Song");
        assert_eq!(value["state"], "Artist");
        assert!(value.get("assets").is_none());
        assert_eq!(value["name"], "muscli");
    }

    #[test]
    fn resume_and_track_changes_refresh_metadata_and_clock() {
        let mut presence = sample_presence();
        let playing = serde_json::to_value(activity(&presence, "peter", true, None)).unwrap();
        assert_eq!(playing["assets"]["large_image"], "peter_dj");
        assert_eq!(playing["assets"]["large_text"], "Album");
        assert_eq!(
            playing["timestamps"]["end"].as_i64().unwrap()
                - playing["timestamps"]["start"].as_i64().unwrap(),
            180_000
        );
        presence.title = "Next song".into();
        presence.album = "Next album".into();
        presence.position_ms = 1_000;
        let resumed = serde_json::to_value(activity(&presence, "peter", false, None)).unwrap();
        assert_eq!(resumed["details"], "Next song");
        assert_eq!(resumed["state"], "Artist");
        assert!(resumed.get("timestamps").is_some());
    }

    #[test]
    fn missing_artist_is_omitted_without_using_album_as_a_fallback() {
        let mut presence = sample_presence();
        presence.album.clear();
        assert_eq!(
            serde_json::to_value(activity(&presence, "peter", false, None)).unwrap()["state"],
            "Artist"
        );
        presence.album = "Album".into();
        presence.artist.clear();
        let payload = serde_json::to_value(activity(&presence, "peter", true, None)).unwrap();
        assert!(payload.get("state").is_none());
        assert_eq!(payload["assets"]["large_text"], "Album");
    }

    #[test]
    fn paused_and_stopped_playback_clear_presence() {
        assert!(publish_presence(PlaybackStatus::Playing));
        assert!(!publish_presence(PlaybackStatus::Paused));
        assert!(!publish_presence(PlaybackStatus::Stopped));
    }

    #[test]
    fn resolved_release_group_cover_replaces_the_existing_image() {
        let presence = sample_presence();
        let cover = "https://coverartarchive.org/release-group/e4d39d2e-9420-4007-881a-9b5bc23944e0/front-500";
        let payload =
            serde_json::to_value(activity(&presence, "peter", true, Some(cover))).unwrap();
        assert_eq!(
            payload["assets"]["large_image"],
            "https://coverartarchive.org/release-group/e4d39d2e-9420-4007-881a-9b5bc23944e0/front-500"
        );
    }

    #[test]
    fn deutschland_selects_the_unique_2019_rammstein_album() {
        let identity = AlbumIdentity {
            release_id: String::new(),
            artist: "Rammstein".into(),
            title: "Rammstein".into(),
            year: Some(2019),
        };
        let response = serde_json::json!({
            "release-groups": [
                {
                    "id": "4ea191d7-f080-3f9f-a0ca-f699eb2cb823",
                    "score": 100,
                    "title": "Rammstein",
                    "first-release-date": "2002",
                    "artist-credit": [{"name": "Rammstein"}]
                },
                {
                    "id": "e4d39d2e-9420-4007-881a-9b5bc23944e0",
                    "score": 100,
                    "title": "[Rammstein]",
                    "first-release-date": "2019-05-17",
                    "artist-credit": [{"name": "Rammstein"}]
                }
            ]
        });
        assert_eq!(
            select_release_group(&response, &identity, 2019).as_deref(),
            Some("e4d39d2e-9420-4007-881a-9b5bc23944e0")
        );
    }

    #[test]
    fn ambiguous_album_search_does_not_guess_a_cover() {
        let identity = AlbumIdentity {
            release_id: String::new(),
            artist: "Artist".into(),
            title: "Album".into(),
            year: Some(2020),
        };
        let candidate = serde_json::json!({
            "id": "76df3287-6cda-33eb-8e9a-044b5e15ffdd",
            "score": 100,
            "title": "Album",
            "first-release-date": "2020-01-01",
            "artist-credit": [{"name": "Artist"}]
        });
        let response = serde_json::json!({"release-groups": [candidate.clone(), candidate]});
        assert_eq!(select_release_group(&response, &identity, 2020), None);
    }

    #[test]
    #[ignore = "requires live MusicBrainz and Cover Art Archive access"]
    fn live_deutschland_resolves_the_canonical_album_cover() {
        let presence = Presence {
            title: "Deutschland".into(),
            artist: "Rammstein".into(),
            album_artist: "Rammstein".into(),
            album: "Rammstein".into(),
            year: Some(2019),
            ..sample_presence()
        };
        assert_eq!(
            AlbumCoverResolver::new()
                .resolve(&AlbumIdentity::from_presence(&presence))
                .unwrap()
                .as_deref(),
            Some(
                "https://coverartarchive.org/release-group/\
                 e4d39d2e-9420-4007-881a-9b5bc23944e0/front-500"
            )
        );
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
