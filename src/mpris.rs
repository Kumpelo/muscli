#[cfg(unix)]
use tokio::sync::mpsc::UnboundedSender;

#[cfg(unix)]
use anyhow::Result;
#[cfg(unix)]
use mpris_server::{LoopStatus, Metadata, PlaybackStatus as MprisStatus, Player, Time, TrackId};
#[cfg(unix)]
use url::Url;

#[cfg(unix)]
use crate::model::{PlaybackState, PlaybackStatus, PlayerAction, RepeatMode, Track};

#[cfg(unix)]
pub struct MprisBridge {
    player: Player,
}

#[cfg(unix)]
impl MprisBridge {
    pub async fn new(actions: UnboundedSender<PlayerAction>) -> Result<Self> {
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
        Ok(())
    }

    pub async fn sync_position(&self, state: &PlaybackState) -> Result<()> {
        self.player.set_position(Time::from_millis(
            state.position_ms.min(i64::MAX as u64) as i64,
        ));
        Ok(())
    }
}

#[cfg(unix)]
fn connect(player: &Player, actions: &UnboundedSender<PlayerAction>) {
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

#[cfg(unix)]
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

#[cfg(windows)]
mod windows_smtc {
    use anyhow::Result;
    use tokio::sync::mpsc::UnboundedSender;
    use windows::{
        Foundation::{TimeSpan, TypedEventHandler},
        Media::Playback::MediaPlayer,
        Media::{
            MediaPlaybackAutoRepeatMode, MediaPlaybackStatus, MediaPlaybackType,
            PlaybackPositionChangeRequestedEventArgs, SystemMediaTransportControls,
            SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
            SystemMediaTransportControlsTimelineProperties,
        },
        core::HSTRING,
    };

    use crate::model::{PlaybackState, PlaybackStatus, PlayerAction, RepeatMode, Track};

    /// Windows media controls bridge. The first Windows release keeps the
    /// public surface identical to MPRIS so the TUI and playback core remain
    /// platform-neutral. SMTC initialization is best-effort: terminals which
    /// do not expose a window handle still play normally.
    pub struct MprisBridge {
        controls: SystemMediaTransportControls,
        _player: MediaPlayer,
        button_token: i64,
        position_token: i64,
    }

    impl MprisBridge {
        pub async fn new(actions: UnboundedSender<PlayerAction>) -> Result<Self> {
            let player = MediaPlayer::new()?;
            let controls = player.SystemMediaTransportControls()?;
            controls.SetIsEnabled(true)?;
            controls.SetIsPlayEnabled(true)?;
            controls.SetIsPauseEnabled(true)?;
            controls.SetIsStopEnabled(true)?;
            controls.SetIsNextEnabled(true)?;
            controls.SetIsPreviousEnabled(true)?;

            let button_actions = actions.clone();
            let button_token = controls.ButtonPressed(&TypedEventHandler::<
                SystemMediaTransportControls,
                SystemMediaTransportControlsButtonPressedEventArgs,
            >::new(move |_, args| {
                let Some(args) = args.as_ref() else {
                    return Ok(());
                };
                let action = match args.Button()? {
                    SystemMediaTransportControlsButton::Play => PlayerAction::Play,
                    SystemMediaTransportControlsButton::Pause => PlayerAction::Pause,
                    SystemMediaTransportControlsButton::Stop => PlayerAction::Stop,
                    SystemMediaTransportControlsButton::Next => PlayerAction::Next,
                    SystemMediaTransportControlsButton::Previous => PlayerAction::Previous,
                    _ => return Ok(()),
                };
                let _ = button_actions.send(action);
                Ok(())
            }))?;

            let position_token = controls.PlaybackPositionChangeRequested(&TypedEventHandler::<
                SystemMediaTransportControls,
                PlaybackPositionChangeRequestedEventArgs,
            >::new(
                move |_, args| {
                    if let Some(args) = args.as_ref() {
                        let ticks = args.RequestedPlaybackPosition()?.Duration.max(0);
                        let _ = actions.send(PlayerAction::SeekAbsolute((ticks / 10_000) as u64));
                    }
                    Ok(())
                },
            ))?;

            Ok(Self {
                controls,
                _player: player,
                button_token,
                position_token,
            })
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
            self.controls.SetPlaybackStatus(match state.status {
                PlaybackStatus::Playing => MediaPlaybackStatus::Playing,
                PlaybackStatus::Paused => MediaPlaybackStatus::Paused,
                PlaybackStatus::Stopped => MediaPlaybackStatus::Stopped,
            })?;
            self.controls.SetIsPreviousEnabled(can_previous)?;
            self.controls.SetIsNextEnabled(can_next)?;
            self.controls.SetShuffleEnabled(shuffle)?;
            self.controls.SetAutoRepeatMode(match repeat {
                RepeatMode::Off => MediaPlaybackAutoRepeatMode::None,
                RepeatMode::Track => MediaPlaybackAutoRepeatMode::Track,
                RepeatMode::Queue => MediaPlaybackAutoRepeatMode::List,
            })?;

            let updater = self.controls.DisplayUpdater()?;
            updater.SetType(MediaPlaybackType::Music)?;
            if let Some(track) = track {
                let music = updater.MusicProperties()?;
                music.SetTitle(&HSTRING::from(&track.title))?;
                music.SetArtist(&HSTRING::from(&track.artist))?;
                music.SetAlbumTitle(&HSTRING::from(&track.album))?;
            } else {
                updater.ClearAll()?;
            }
            updater.Update()?;

            Ok(())
        }

        pub async fn sync_position(&self, state: &PlaybackState) -> Result<()> {
            let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
            let duration = millis_to_timespan(state.duration_ms);
            timeline.SetStartTime(TimeSpan { Duration: 0 })?;
            timeline.SetMinSeekTime(TimeSpan { Duration: 0 })?;
            timeline.SetEndTime(duration)?;
            timeline.SetMaxSeekTime(duration)?;
            timeline.SetPosition(millis_to_timespan(state.position_ms))?;
            self.controls.UpdateTimelineProperties(&timeline)?;
            Ok(())
        }
    }

    impl Drop for MprisBridge {
        fn drop(&mut self) {
            let _ = self.controls.RemoveButtonPressed(self.button_token);
            let _ = self
                .controls
                .RemovePlaybackPositionChangeRequested(self.position_token);
            let _ = self.controls.SetPlaybackStatus(MediaPlaybackStatus::Closed);
            let _ = self.controls.SetIsEnabled(false);
        }
    }

    fn millis_to_timespan(value: u64) -> TimeSpan {
        TimeSpan {
            Duration: value.min(i64::MAX as u64 / 10_000) as i64 * 10_000,
        }
    }
}

#[cfg(windows)]
pub use windows_smtc::MprisBridge;
