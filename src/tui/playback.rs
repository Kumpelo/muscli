//! Playback: the queue, history accounting and outbound presence.
//!
//! Owns what plays next and what gets recorded about it: loading a track into
//! mpv with its ReplayGain filter, queue movement, listening history and resume
//! positions, and mirroring the current state to MPRIS, SMTC and Discord.

use super::*;

/// Listening accounting for the track currently loaded.
///
/// These seven values only make sense together: loading a track resets all of
/// them, and a flush has to clear the pending delta and restart the flush clock
/// in the same breath. As separate fields on App nothing said so.
pub(super) struct HistoryTally {
    /// The in-progress history row, when history recording is enabled.
    pub(super) entry: Option<i64>,
    pub(super) track_id: Option<String>,
    /// Listened since this track was loaded; decides whether the play counts.
    pub(super) listened_ms: u64,
    /// Listened since the last flush; added to the stored total.
    pub(super) pending_ms: u64,
    /// Whether this play has already been counted, so a later flush does not
    /// count it twice.
    pub(super) counted: bool,
    pub(super) last_tick: Instant,
    pub(super) last_flush: Instant,
}

impl HistoryTally {
    pub(super) fn new() -> Self {
        Self {
            entry: None,
            track_id: None,
            listened_ms: 0,
            pending_ms: 0,
            counted: false,
            last_tick: Instant::now(),
            last_flush: Instant::now(),
        }
    }

    /// Begin accounting for a freshly loaded track.
    fn restart(&mut self, entry: i64, track_id: String) {
        *self = Self {
            entry: Some(entry),
            track_id: Some(track_id),
            ..Self::new()
        };
    }
}

impl App {
    pub(super) fn load_current(&mut self, position_ms: u64) -> Result<()> {
        self.flush_history(false)?;
        let Some(track) = self.current_track().cloned() else {
            return Ok(());
        };
        if !track.available || !track.path.exists() {
            self.status = format!("No disponible: {}", track.title);
            self.playback.status = PlaybackStatus::Stopped;
            self.mpv.stop()?;
            self.dirty = true;
            return Ok(());
        }
        let gain = if self.config.replaygain_enabled {
            self.db.track_gain(
                &track.id,
                self.config.replaygain_mode == ReplayGainMode::Album,
            )?
        } else {
            None
        };
        let gain = gain.map(|analysis| analysis.gain_db.min(-analysis.true_peak_db));
        self.mpv.set_replay_gain(gain)?;
        self.mpv.load(&track.path, position_ms)?;
        self.mpv.pause(false)?;
        self.playback.status = PlaybackStatus::Playing;
        self.playback.position_ms = position_ms;
        self.playback.duration_ms = track.duration_ms;
        if self.config.history_enabled {
            let entry = self.db.start_history(&track.id)?;
            self.tally.restart(entry, track.id.clone());
        }
        self.status.clear();
        self.dirty = true;
        self.persist_playback()?;
        Ok(())
    }

    pub(super) fn next(&mut self) -> Result<()> {
        if self.queue.is_empty() {
            return Ok(());
        }
        let current = self.queue_index.unwrap_or(0);
        for offset in 1..=self.queue.len() {
            let raw = current + offset;
            if raw >= self.queue.len() && self.repeat != RepeatMode::Queue {
                break;
            }
            let index = raw % self.queue.len();
            if self.queue_track_is_playable(index) {
                self.queue_index = Some(index);
                return self.load_current(0);
            }
        }
        self.playback.status = PlaybackStatus::Stopped;
        self.status = "No quedan pistas disponibles en la cola".into();
        self.mpv.stop()?;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn previous(&mut self) -> Result<()> {
        if self.playback.position_ms > 5_000 {
            return self.load_current(0);
        }
        let current = self.queue_index.unwrap_or(0);
        for offset in 1..=self.queue.len() {
            let Some(index) = current.checked_sub(offset) else {
                break;
            };
            if self.queue_track_is_playable(index) {
                self.queue_index = Some(index);
                return self.load_current(0);
            }
        }
        self.load_current(0)
    }

    pub(super) fn queue_track_is_playable(&self, index: usize) -> bool {
        self.queue
            .get(index)
            .and_then(|id| self.track_index.get(id))
            .map(|&track_index| &self.tracks[track_index])
            .is_some_and(|track| track.available && track.path.exists())
    }

    pub(super) fn move_queue_item(&mut self, amount: isize) {
        if self.queue.is_empty() {
            return;
        }
        let target = shifted_index(self.selected, amount, self.queue.len());
        self.queue.swap(self.selected, target);
        self.queue_dirty = true;
        if self.queue_index == Some(self.selected) {
            self.queue_index = Some(target);
        } else if self.queue_index == Some(target) {
            self.queue_index = Some(self.selected);
        }
        self.selected = target;
        self.dirty = true;
    }

    pub(super) fn remove_queue_item(&mut self) {
        if self.selected >= self.queue.len() {
            return;
        }
        self.queue.remove(self.selected);
        self.queue_dirty = true;
        if let Some(current) = self.queue_index {
            self.queue_index = if self.queue.is_empty() {
                None
            } else if self.selected < current {
                Some(current - 1)
            } else {
                Some(current.min(self.queue.len() - 1))
            };
        }
        self.selected = self.selected.min(self.queue.len().saturating_sub(1));
        self.dirty = true;
    }

    pub(super) fn handle_remote_action(&mut self, action: RemoteCommand) -> Result<()> {
        let step = f64::from(self.config.volume_step.clamp(1, 20)) / 100.0;
        match action {
            RemoteCommand::VolumeUp => self.handle_action(PlayerAction::SetVolume(
                (self.playback.volume + step).min(1.0),
            ))?,
            RemoteCommand::VolumeDown => self.handle_action(PlayerAction::SetVolume(
                (self.playback.volume - step).max(0.0),
            ))?,
            RemoteCommand::VolumeSet(percent) => {
                self.handle_action(PlayerAction::SetVolume(f64::from(percent.min(100)) / 100.0))?
            }
            RemoteCommand::MuteToggle => self.handle_action(PlayerAction::MuteToggle)?,
            RemoteCommand::Rescan => {
                if self.scan_running {
                    self.scan_pending = true;
                } else {
                    self.start_scan();
                }
            }
            RemoteCommand::Prune => {
                let tracks = self.db.prune_missing_tracks()?;
                let covers = self.db.clear_dangling_cover_paths()?;
                prune_unreferenced_covers(
                    &self.paths.cover_cache_dir(),
                    &self.db.referenced_cover_paths()?,
                )?;
                let removed = prune_cover_cache(
                    &self.paths.cover_cache_dir(),
                    self.config.cover_cache_mb * 1024 * 1024,
                )?;
                let references = self.db.clear_cover_paths(&removed)?;
                self.reload_library()?;
                self.status = format!(
                    "Limpieza: {tracks} pistas, {} referencias de portada",
                    covers + references
                );
            }
        }
        Ok(())
    }

    pub(super) fn tick_history(&mut self) -> Result<()> {
        let elapsed = self.tally.last_tick.elapsed();
        self.tally.last_tick = Instant::now();
        let playing = self.playback.status == PlaybackStatus::Playing;
        if playing && self.tally.entry.is_some() {
            let millis = elapsed.as_millis().min(u64::MAX as u128) as u64;
            self.tally.listened_ms = self.tally.listened_ms.saturating_add(millis);
            self.tally.pending_ms = self.tally.pending_ms.saturating_add(millis);
        }

        let history_due = playing
            && self.tally.entry.is_some()
            && self.tally.last_flush.elapsed() >= Duration::from_secs(5);
        let playback_due = playing
            && self.current_track().is_some()
            && self.last_playback_save.elapsed() >= Duration::from_secs(5);

        if history_due && playback_due {
            self.flush_history_with_playback(false)?;
        } else if history_due {
            self.flush_history(false)?;
        } else if playback_due {
            self.persist_playback()?;
        }
        Ok(())
    }

    pub(super) fn flush_history(&mut self, completed: bool) -> Result<()> {
        self.flush_history_inner(completed, false)
    }

    pub(super) fn flush_history_with_playback(&mut self, completed: bool) -> Result<()> {
        self.flush_history_inner(completed, true)
    }

    pub(super) fn flush_history_inner(
        &mut self,
        completed: bool,
        save_playback: bool,
    ) -> Result<()> {
        let (Some(history_id), Some(track_id)) = (self.tally.entry, self.tally.track_id.clone())
        else {
            return Ok(());
        };
        let duration = self
            .track_index
            .get(&track_id)
            .map(|index| self.tracks[*index].duration_ms)
            .unwrap_or(self.playback.duration_ms);
        let threshold = (duration / 2).min(240_000);
        let count_now = self.tally.listened_ms >= threshold && threshold > 0;
        let completed = completed
            || (duration > 0 && self.playback.position_ms >= duration.saturating_mul(95) / 100);
        if save_playback {
            let state = self.playback_snapshot();
            let queue = self.queue_dirty.then_some(self.queue.as_slice());
            self.db.update_history_and_playback(
                HistoryUpdate {
                    history_id,
                    track_id: &track_id,
                    listened_delta_ms: self.tally.pending_ms,
                    position_ms: self.playback.position_ms,
                    was_counted: self.tally.counted,
                    count_now,
                    completed,
                },
                &state,
                queue,
            )?;
            self.queue_dirty = false;
            self.last_playback_save = Instant::now();
        } else {
            self.db.update_history(HistoryUpdate {
                history_id,
                track_id: &track_id,
                listened_delta_ms: self.tally.pending_ms,
                position_ms: self.playback.position_ms,
                was_counted: self.tally.counted,
                count_now,
                completed,
            })?;
        }
        self.tally.pending_ms = 0;
        self.tally.counted |= count_now;
        self.tally.last_flush = Instant::now();
        Ok(())
    }

    pub(super) fn handle_action(&mut self, action: PlayerAction) -> Result<()> {
        match action {
            PlayerAction::Play => {
                if self.current_track().is_none() {
                    self.activate_selection()?
                } else if !self
                    .queue_index
                    .is_some_and(|index| self.queue_track_is_playable(index))
                {
                    self.next()?;
                } else {
                    self.mpv.pause(false)?;
                    self.playback.status = PlaybackStatus::Playing;
                }
            }
            PlayerAction::Pause => {
                self.mpv.pause(true)?;
                self.playback.status = PlaybackStatus::Paused;
                self.flush_history(false)?;
            }
            PlayerAction::Toggle => {
                if self.current_track().is_none() {
                    self.activate_selection()?;
                } else if !self
                    .queue_index
                    .is_some_and(|index| self.queue_track_is_playable(index))
                {
                    self.next()?;
                } else {
                    self.mpv.toggle()?;
                }
            }
            PlayerAction::Stop => {
                self.mpv.stop()?;
                self.playback.status = PlaybackStatus::Stopped;
            }
            PlayerAction::Next => self.next()?,
            PlayerAction::Previous => self.previous()?,
            PlayerAction::SeekRelative(offset_ms) => {
                self.mpv.seek_relative(offset_ms as f64 / 1000.0)?
            }
            PlayerAction::SeekAbsolute(position) => self.mpv.seek_absolute_ms(position)?,
            PlayerAction::SetVolume(volume) => {
                self.playback.volume = volume.clamp(0.0, 1.0);
                self.mpv.set_volume(self.playback.volume)?;
                if self.playback.volume > 0.0 {
                    self.muted_volume = None;
                }
            }
            PlayerAction::MuteToggle => {
                if self.playback.volume > 0.0 {
                    self.muted_volume = Some(self.playback.volume);
                    self.playback.volume = 0.0;
                } else {
                    self.playback.volume = self.muted_volume.take().unwrap_or(1.0);
                }
                self.mpv.set_volume(self.playback.volume)?;
            }
            PlayerAction::SetShuffle(value) => self.shuffle = value,
            PlayerAction::SetRepeat(value) => self.repeat = value,
            PlayerAction::Quit => self.should_quit = true,
        }
        self.dirty = true;
        Ok(())
    }

    pub(super) fn handle_player_event(&mut self, event: PlayerEvent) -> Result<()> {
        let force_redraw = !matches!(&event, PlayerEvent::Position(_));
        match event {
            PlayerEvent::Position(value) => self.playback.position_ms = value,
            PlayerEvent::Duration(value) => self.playback.duration_ms = value,
            PlayerEvent::Paused(true) => {
                self.playback.status = PlaybackStatus::Paused;
                self.flush_history(false)?;
                self.persist_playback()?;
            }
            PlayerEvent::Paused(false) if self.current_track().is_some() => {
                self.playback.status = PlaybackStatus::Playing
            }
            PlayerEvent::Paused(false) => {}
            PlayerEvent::Volume(value) => self.playback.volume = value,
            PlayerEvent::EndOfFile if self.repeat == RepeatMode::Track => {
                self.flush_history(true)?;
                self.load_current(0)?
            }
            PlayerEvent::EndOfFile => {
                self.flush_history(true)?;
                self.next()?
            }
            PlayerEvent::Error(error) => {
                self.status = error;
                self.next()?;
            }
        }
        self.dirty |= force_redraw;
        Ok(())
    }

    pub(super) fn current_track_hash(&self) -> Option<u64> {
        self.current_track().map(|track| {
            let mut hasher = DefaultHasher::new();
            track.id.hash(&mut hasher);
            hasher.finish()
        })
    }

    pub(super) async fn sync_mpris(&mut self) {
        let index = self.queue_index.unwrap_or(0);
        let state_signature = MediaSessionSignature {
            track_hash: self.current_track_hash(),
            status: self.playback.status,
            volume_bits: self.playback.volume.to_bits(),
            shuffle: self.shuffle,
            repeat: self.repeat,
            can_previous: index > 0,
            can_next: index + 1 < self.queue.len(),
        };
        let position_signature = (self.playback.duration_ms, self.playback.position_ms / 1000);

        if let Some(mpris) = &self.mpris {
            if self.last_mpris_signature != Some(state_signature) {
                let _ = mpris
                    .sync(
                        self.current_track(),
                        &self.playback,
                        self.shuffle,
                        self.repeat,
                        state_signature.can_previous,
                        state_signature.can_next,
                    )
                    .await;
                self.last_mpris_signature = Some(state_signature);
            }
            if self.last_mpris_position_signature != Some(position_signature) {
                let _ = mpris.sync_position(&self.playback).await;
                self.last_mpris_position_signature = Some(position_signature);
            }
        }
    }

    pub(super) fn sync_discord(&mut self) {
        let signature = (
            self.current_track_hash(),
            self.playback.status,
            self.playback.duration_ms,
            self.playback.position_ms / 15_000,
        );
        if self.last_discord_signature == Some(signature) {
            return;
        }
        self.last_discord_signature = Some(signature);
        if let Some(discord) = &self.discord {
            discord.sync(self.current_track(), &self.playback);
        }
    }

    pub(super) fn save_state(&mut self) -> Result<()> {
        self.flush_history(false)?;
        self.persist_playback()
    }

    pub(super) fn playback_snapshot(&self) -> SavedPlayback {
        SavedPlayback {
            queue: Vec::new(),
            current_index: self.queue_index,
            position_ms: self.playback.position_ms,
            volume: self.playback.volume,
            last_nonzero_volume: self.muted_volume,
            shuffle: self.shuffle,
            repeat: self.repeat,
        }
    }

    pub(super) fn persist_playback(&mut self) -> Result<()> {
        let state = self.playback_snapshot();
        let queue = self.queue_dirty.then_some(self.queue.as_slice());
        self.db.save_playback(&state, queue)?;
        self.queue_dirty = false;
        self.last_playback_save = Instant::now();
        Ok(())
    }
}
