//! Playback: the queue, history accounting and outbound presence.
//!
//! Owns what plays next and what gets recorded about it: loading a track into
//! mpv with its ReplayGain filter, queue movement, listening history and resume
//! positions, and mirroring the current state to MPRIS, SMTC and Discord.

use super::*;
use crate::t;

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

/// Which queue position plays after `current`.
///
/// Walks forward, skipping entries whose file is missing, and wraps only when
/// the queue repeats. Separate from `App` so the rule the prefetch depends on
/// can be tested without a database, an mpv process or a terminal.
fn next_playable(
    len: usize,
    current: usize,
    repeat: RepeatMode,
    playable: impl Fn(usize) -> bool,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    for offset in 1..=len {
        let raw = current + offset;
        if raw >= len && repeat != RepeatMode::Queue {
            break;
        }
        let index = raw % len;
        if playable(index) {
            return Some(index);
        }
    }
    None
}

impl App {
    pub(super) fn load_current(&mut self, position_ms: u64) -> Result<()> {
        self.flush_history(false)?;
        let Some(track) = self.current_track().cloned() else {
            return Ok(());
        };
        if !track.available || !track.path.exists() {
            self.status = t!("status.unavailable", title = track.title);
            self.playback.status = PlaybackStatus::Stopped;
            self.player.stop()?;
            self.dirty = true;
            return Ok(());
        }
        self.apply_replay_gain(&track)?;
        self.player.load(&track.path, position_ms)?;
        // A replace wipes mpv's playlist, so whatever was queued behind is gone.
        self.prefetched = None;
        self.player.pause(false)?;
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

    fn apply_replay_gain(&mut self, track: &Track) -> Result<()> {
        let gain = if self.config.replaygain_enabled {
            self.db.track_gain(
                &track.id,
                self.config.replaygain_mode == ReplayGainMode::Album,
            )?
        } else {
            None
        };
        let gain = gain.map(|analysis| analysis.gain_db.min(-analysis.true_peak_db));
        self.player.set_replay_gain(gain)
    }

    /// The queue index `next` would move to, without moving there.
    ///
    /// Shuffle reorders the queue itself, so what plays next is a pure function
    /// of the queue, the current index and the repeat mode - which is what lets
    /// it be prefetched.
    fn next_index(&self) -> Option<usize> {
        next_playable(
            self.queue.len(),
            self.queue_index.unwrap_or(0),
            self.repeat,
            |index| self.queue_track_is_playable(index),
        )
    }

    fn queue_track_path(&self, index: usize) -> Option<PathBuf> {
        let id = self.queue.get(index)?;
        let track_index = *self.track_index.get(id)?;
        Some(self.tracks.get(track_index)?.path.clone())
    }

    /// Load lyrics for the current track, if they have been imported.
    ///
    /// Cached per track rather than read every frame. Successful loads stay
    /// cached; misses are retried periodically so an external `lyrics import`
    /// becomes visible without changing track or restarting muscli.
    pub(super) fn sync_lyrics(&mut self) {
        let current = self.current_track().map(|track| track.id.clone());
        if current.is_none() {
            if self.lyrics_track.is_some() || self.lyrics.is_some() {
                self.lyrics_track = None;
                self.lyrics = None;
                self.dirty = true;
            }
            return;
        }

        let same_track = current == self.lyrics_track;
        if same_track && self.lyrics.is_some() {
            return;
        }
        if same_track && self.last_lyrics_check.elapsed() < Duration::from_secs(1) {
            return;
        }

        self.last_lyrics_check = Instant::now();
        self.lyrics = self
            .current_track()
            .cloned()
            .and_then(|track| crate::lyrics::load(&self.paths, &track));
        self.lyrics_track = current;
        self.dirty = true;
    }

    /// Keep mpv's queued entry in step with whatever would play next.
    ///
    /// Driven from the event loop rather than from each mutation, because the
    /// queue changes from a dozen places - reorder, remove, clear, enqueue,
    /// load a saved queue, toggle shuffle or repeat - and missing one would
    /// either lose the gapless transition or play the wrong track. Comparing
    /// against what is already armed means no IPC when nothing moved.
    pub(super) fn sync_prefetch(&mut self) -> Result<()> {
        let wanted = if self.config.gapless
            && self.repeat != RepeatMode::Track
            && self.playback.status != PlaybackStatus::Stopped
        {
            self.next_index()
        } else {
            None
        };
        if wanted == self.prefetched {
            return Ok(());
        }
        match wanted.and_then(|index| self.queue_track_path(index)) {
            Some(path) => {
                self.player.set_prefetch(Some(&path))?;
                self.prefetched = wanted;
            }
            None => {
                self.player.set_prefetch(None)?;
                self.prefetched = None;
            }
        }
        Ok(())
    }

    /// Adopt the track mpv started on its own.
    ///
    /// The audio is already playing, so this only catches the application up:
    /// reloading here would undo the very gap this avoids.
    fn adopt_prefetched(&mut self) -> Result<()> {
        let Some(index) = self.prefetched.take() else {
            return Ok(());
        };
        self.flush_history(true)?;
        self.queue_index = Some(index);
        let Some(track) = self.current_track().cloned() else {
            return Ok(());
        };
        // The filter is applied a few tens of milliseconds into the track,
        // which is the accepted cost of not reloading.
        self.apply_replay_gain(&track)?;
        self.playback.status = PlaybackStatus::Playing;
        self.playback.position_ms = 0;
        self.playback.duration_ms = track.duration_ms;
        if self.config.history_enabled {
            let entry = self.db.start_history(&track.id)?;
            self.tally.restart(entry, track.id.clone());
        }
        // Make the playing file entry 0 again, so the next one can be queued
        // behind it and the playlist never grows.
        self.player.adopt_prefetch()?;
        self.status.clear();
        self.dirty = true;
        self.persist_playback()?;
        Ok(())
    }

    pub(super) fn next(&mut self) -> Result<()> {
        if let Some(index) = self.next_index() {
            self.queue_index = Some(index);
            return self.load_current(0);
        }
        self.playback.status = PlaybackStatus::Stopped;
        self.status = t!("status.queue_exhausted").into();
        self.player.stop()?;
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
        // Stepped in whole percent rather than by adding a fraction to what
        // is already there. Adding 0.05 to an f64 nine times lands on
        // 0.4499999999999999, which reads back as 44 rather than 45, and from
        // then on every step looks like a different size than the one asked
        // for.
        let step = i32::from(self.config.volume_step.clamp(1, 20));
        let percent = (self.playback.volume * 100.0).round() as i32;
        let stepped =
            |by: i32| PlayerAction::SetVolume(f64::from((percent + by).clamp(0, 100)) / 100.0);
        match action {
            RemoteCommand::VolumeUp => self.handle_action(stepped(step))?,
            RemoteCommand::VolumeDown => self.handle_action(stepped(-step))?,
            RemoteCommand::VolumeSet(percent) => {
                self.handle_action(PlayerAction::SetVolume(f64::from(percent.min(100)) / 100.0))?
            }
            RemoteCommand::MuteToggle => self.handle_action(PlayerAction::MuteToggle)?,
            RemoteCommand::Rescan => {
                if self.scan_running {
                    self.scan_pending.record_full_rescan();
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
                self.status = t!(
                    "status.cleanup",
                    tracks = tracks,
                    covers = covers + references
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
                    self.player.pause(false)?;
                    self.playback.status = PlaybackStatus::Playing;
                }
            }
            PlayerAction::Pause => {
                self.player.pause(true)?;
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
                    self.player.toggle()?;
                }
            }
            PlayerAction::Stop => {
                self.player.stop()?;
                self.playback.status = PlaybackStatus::Stopped;
            }
            PlayerAction::Next => self.next()?,
            PlayerAction::Previous => self.previous()?,
            PlayerAction::SeekRelative(offset_ms) => {
                self.player.seek_relative(offset_ms as f64 / 1000.0)?
            }
            PlayerAction::SeekAbsolute(position) => self.player.seek_absolute_ms(position)?,
            PlayerAction::SetVolume(volume) => {
                self.playback.volume = volume.clamp(0.0, 1.0);
                self.player.set_volume(self.playback.volume)?;
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
                self.player.set_volume(self.playback.volume)?;
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
            PlayerEvent::PlaylistPosition(position) => {
                // Anything past the first entry means mpv rolled into the track
                // queued behind this one.
                if position >= 1 {
                    self.adopt_prefetched()?;
                }
            }
            PlayerEvent::EndOfFile if self.repeat == RepeatMode::Track => {
                self.flush_history(true)?;
                self.load_current(0)?
            }
            // mpv is already starting the queued track; the playlist-pos change
            // does the bookkeeping. Advancing here would reload it and
            // reintroduce the gap this exists to remove.
            PlayerEvent::EndOfFile if self.prefetched.is_some() => {}
            PlayerEvent::EndOfFile => {
                self.flush_history(true)?;
                self.next()?
            }
            PlayerEvent::Notice(message) => self.status = message,
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

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: fn(usize) -> bool = |_| true;

    #[test]
    fn an_empty_queue_has_no_next_track() {
        assert_eq!(next_playable(0, 0, RepeatMode::Off, ALL), None);
        assert_eq!(next_playable(0, 0, RepeatMode::Queue, ALL), None);
    }

    #[test]
    fn the_next_track_is_the_one_after_the_current() {
        assert_eq!(next_playable(5, 0, RepeatMode::Off, ALL), Some(1));
        assert_eq!(next_playable(5, 3, RepeatMode::Off, ALL), Some(4));
    }

    #[test]
    fn the_end_of_the_queue_only_wraps_when_it_repeats() {
        assert_eq!(
            next_playable(3, 2, RepeatMode::Off, ALL),
            None,
            "without repeat the queue simply ends"
        );
        assert_eq!(next_playable(3, 2, RepeatMode::Queue, ALL), Some(0));
        // Repeating one track is handled by reloading it, not by moving on, so
        // this rule is the same as Off here.
        assert_eq!(next_playable(3, 2, RepeatMode::Track, ALL), None);
    }

    #[test]
    fn missing_files_are_skipped_over() {
        // Index 1 and 2 are gone, for instance because a drive was unplugged.
        let playable = |index: usize| index != 1 && index != 2;
        assert_eq!(next_playable(5, 0, RepeatMode::Off, playable), Some(3));
    }

    #[test]
    fn a_queue_with_nothing_playable_has_no_next_track() {
        assert_eq!(next_playable(4, 0, RepeatMode::Queue, |_| false), None);
    }

    #[test]
    fn a_repeating_queue_comes_back_to_the_only_playable_track_last() {
        // Every other entry is gone, so repeat-queue loops the survivor - and
        // the prefetch queues it again, which is what makes that loop seamless.
        // It is reached only after the others have been ruled out.
        let playable = |index: usize| index == 2;
        assert_eq!(next_playable(4, 2, RepeatMode::Queue, playable), Some(2));
        assert_eq!(
            next_playable(4, 2, RepeatMode::Off, playable),
            None,
            "without repeat there is nothing after it"
        );
    }
}
