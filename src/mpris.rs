use std::sync::mpsc::Sender;

use anyhow::Result;
use mpris_server::{LoopStatus, Metadata, PlaybackStatus as MprisStatus, Player, Time, TrackId};
use url::Url;

use crate::model::{PlaybackState, PlaybackStatus, PlayerAction, RepeatMode, Track};

pub struct MprisBridge {
    player: Player,
}

impl MprisBridge {
    pub async fn new(actions: Sender<PlayerAction>) -> Result<Self> {
        let player = Player::builder("muscli")
            .identity("muscli")
            .desktop_entry("muscli")
            .supported_uri_schemes(["file"])
            .supported_mime_types(["audio/flac"])
            .can_quit(true)
            .can_play(true)
            .can_pause(true)
            .can_seek(true)
            .can_go_next(true)
            .can_go_previous(true)
            .build()
            .await?;

        connect(&player, &actions);
        tokio::task::spawn_local(player.run());
        Ok(Self { player })
    }

    pub async fn sync(
        &self,
        track: Option<&Track>,
        state: &PlaybackState,
        shuffle: bool,
        repeat: RepeatMode,
        can_previous: bool,
        can_next: bool,
    ) -> Result<()> {
        let status = match state.status {
            PlaybackStatus::Playing => MprisStatus::Playing,
            PlaybackStatus::Paused => MprisStatus::Paused,
            PlaybackStatus::Stopped => MprisStatus::Stopped,
        };
        self.player.set_playback_status(status).await?;
        self.player.set_volume(state.volume).await?;
        self.player.set_shuffle(shuffle).await?;
        self.player
            .set_loop_status(match repeat {
                RepeatMode::Off => LoopStatus::None,
                RepeatMode::Track => LoopStatus::Track,
                RepeatMode::Queue => LoopStatus::Playlist,
            })
            .await?;
        self.player.set_can_go_previous(can_previous).await?;
        self.player.set_can_go_next(can_next).await?;
        self.player
            .set_metadata(track.map(metadata).unwrap_or_default())
            .await?;
        self.player.set_position(Time::from_millis(
            state.position_ms.min(i64::MAX as u64) as i64
        ));
        Ok(())
    }
}

fn connect(player: &Player, actions: &Sender<PlayerAction>) {
    macro_rules! action {
        ($method:ident, $value:expr) => {{
            let tx = actions.clone();
            player.$method(move |_| {
                let _ = tx.send($value);
            });
        }};
    }
    action!(connect_next, PlayerAction::Next);
    action!(connect_previous, PlayerAction::Previous);
    action!(connect_pause, PlayerAction::Pause);
    action!(connect_play, PlayerAction::Play);
    action!(connect_play_pause, PlayerAction::Toggle);
    action!(connect_stop, PlayerAction::Stop);
    action!(connect_quit, PlayerAction::Quit);

    let tx = actions.clone();
    player.connect_seek(move |_, offset| {
        let _ = tx.send(PlayerAction::SeekRelative(offset.as_millis()));
    });
    let tx = actions.clone();
    player.connect_set_position(move |_, _, position| {
        let _ = tx.send(PlayerAction::SeekAbsolute(
            position.as_millis().max(0) as u64
        ));
    });
    let tx = actions.clone();
    player.connect_set_volume(move |_, volume| {
        let _ = tx.send(PlayerAction::SetVolume(volume));
    });
    let tx = actions.clone();
    player.connect_set_shuffle(move |_, value| {
        let _ = tx.send(PlayerAction::SetShuffle(value));
    });
    let tx = actions.clone();
    player.connect_set_loop_status(move |_, value| {
        let mode = match value {
            LoopStatus::Track => RepeatMode::Track,
            LoopStatus::Playlist => RepeatMode::Queue,
            LoopStatus::None => RepeatMode::Off,
        };
        let _ = tx.send(PlayerAction::SetRepeat(mode));
    });
}

fn metadata(track: &Track) -> Metadata {
    let track_id = TrackId::try_from(format!("/org/muscli/track/{}", track.id)).unwrap_or_default();
    let mut builder = Metadata::builder()
        .trackid(track_id)
        .title(track.title.clone())
        .artist([track.artist.clone()])
        .album_artist([track.album_artist.clone()])
        .album(track.album.clone())
        .length(Time::from_millis(
            track.duration_ms.min(i64::MAX as u64) as i64
        ))
        .track_number(track.track_number as i32)
        .disc_number(track.disc_number as i32);
    if let Ok(url) = Url::from_file_path(&track.path) {
        builder = builder.url(url.to_string());
    }
    if let Some(cover) = &track.cover_path
        && let Ok(url) = Url::from_file_path(cover)
    {
        builder = builder.art_url(url.to_string());
    }
    if !track.genre.is_empty() {
        builder = builder.genre([track.genre.clone()]);
    }
    builder.build()
}
