//! Playback: the queue, history accounting and outbound presence.
//!
//! Owns what plays next and what gets recorded about it: loading a track into
//! mpv with its ReplayGain filter, queue movement, listening history and resume
//! positions, and mirroring the current state to MPRIS, SMTC and Discord.

use super::*;
use crate::t;

/// Cache the identity of the armed file as well as its position. Inserting or
/// shuffling can replace a queue entry without changing its index.
fn sync_prefetch_target(
    armed: &mut Option<(usize, PathBuf)>,
    wanted: Option<(usize, PathBuf)>,
    mut set_prefetch: impl FnMut(Option<&Path>) -> Result<()>,
) -> Result<()> {
    if *armed == wanted {
        return Ok(());
    }
    set_prefetch(wanted.as_ref().map(|(_, path)| path.as_path()))?;
    *armed = wanted;
    Ok(())
}

pub(super) struct AudibleMix {
    pub(super) track_id: String,
    pub(super) path: PathBuf,
    pub(super) position_ms: u64,
    pub(super) progress: f32,
    tally: HistoryTally,
}

/// Listening accounting for the track currently loaded. Grouped because they
/// move together: a load resets all of them, and a flush has to clear the
/// pending delta and restart the clock in the same breath.
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

fn shuffle_upcoming<T>(queue: &mut [T], current: Option<usize>, rng: &mut impl rand::Rng) {
    let start = current
        .map(|index| index.saturating_add(1))
        .unwrap_or(0)
        .min(queue.len());

    queue[start..].shuffle(rng);
}

/// Which queue position plays after `current`.
///
/// Walks forward, skipping entries whose file is missing, and wraps only when
/// the queue repeats. Separate from `App` so the rule the prefetch depends on
/// can be tested without a database, an mpv process or a terminal.
fn previous_playable(
    len: usize,
    current: usize,
    position_ms: u64,
    playable: impl Fn(usize) -> bool,
) -> Option<usize> {
    // Like other music players, Previous restarts once the song has played
    // for a few seconds. At its beginning, it selects the last playable
    // entry before the current queue position.
    if position_ms > 5_000 {
        return None;
    }
    (0..current.min(len)).rev().find(|&index| playable(index))
}

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
    pub(super) fn set_shuffle(&mut self, enabled: bool) {
        if self.shuffle == enabled {
            return;
        }

        self.shuffle = enabled;

        if enabled && !self.playback_context.is_album() {
            let mut rng = rand::rng();
            shuffle_upcoming(&mut self.queue, self.queue_index, &mut rng);

            self.queue_dirty = true;
        }

        self.dirty = true;
    }
    pub(super) fn load_current(&mut self, position_ms: u64) -> Result<()> {
        self.load_current_with_pause(position_ms, false)
    }

    pub(super) fn load_current_with_pause(&mut self, position_ms: u64, paused: bool) -> Result<()> {
        self.flush_audible_mix()?;
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
        if paused {
            // Set the native backend's pause flag before the asynchronous
            // Load command can open a new output stream.
            self.player.pause(true)?;
        }
        self.sync_crossfade()?;
        self.player.load(&track.path, position_ms)?;
        // A replace wipes mpv's playlist, so whatever was queued behind is gone.
        self.prefetched = None;
        // Keep the native player's atomic pause set before its background
        // worker starts the new stream. Briefly unpausing first can leak sound
        // when Previous is pressed while the player is paused.
        self.player.pause(paused)?;
        self.playback.status = if paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
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

    fn replay_gain(&self, track: &Track) -> Result<Option<f64>> {
        let gain = if !self.config.replaygain_enabled {
            None
        } else if self.config.replaygain_mode == ReplayGainMode::Album {
            // The library's release grouping also includes conservatively
            // inferred compilations whose tracks have different album artists.
            if let Some(album) = self
                .albums
                .iter()
                .find(|album| album.track_ids.contains(&track.id))
            {
                self.db.album_gain_for_tracks(&track.id, &album.track_ids)?
            } else {
                self.db.track_gain(&track.id, true)?
            }
        } else {
            self.db.track_gain(&track.id, false)?
        };
        let gain = gain.map(|analysis| analysis.gain_db.min(-analysis.true_peak_db));
        Ok(gain)
    }

    fn apply_replay_gain(&mut self, track: &Track) -> Result<()> {
        let gain = self.replay_gain(track)?;
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

    /// Keep the backend's queued track in step with whatever plays next.
    ///
    /// Driven from the event loop rather than from each mutation, since the
    /// queue changes from a dozen places. Comparing against what is already
    /// armed means no work when nothing moved.
    pub(super) fn sync_crossfade(&mut self) -> Result<()> {
        use crate::audio::smart_mix::{self, LiveMixMode};
        let configured = match self.config.live_mix_mode() {
            LiveMixMode::Off => 0.0,
            LiveMixMode::Classic => self.config.mix_crossfade_seconds,
            LiveMixMode::Smart | LiveMixMode::AutoDj => self
                .current_track()
                .and_then(|current| {
                    let next = self.next_index().and_then(|index| self.queue.get(index))?;
                    let first = *self.mix_analyses.get(&current.id)?;
                    let second = *self.mix_analyses.get(next)?;
                    Some(smart_mix::transition_seconds(
                        first,
                        second,
                        self.config.mix_crossfade_seconds,
                    ))
                })
                .unwrap_or(0.0),
        };
        let seconds = self.playback_context.crossfade_seconds(
            configured,
            self.config.bit_perfect,
            self.config.gapless,
        );
        if self.applied_crossfade != Some(seconds) {
            self.player
                .set_crossfade(seconds, crate::audio::mix::MixCurve::Smooth)?;
            self.applied_crossfade = Some(seconds);
        }
        Ok(())
    }

    pub(super) fn sync_prefetch(&mut self) -> Result<()> {
        self.extend_auto_dj();
        self.sync_crossfade()?;
        let wanted = if (self.config.gapless || self.playback_context.is_album())
            && self.repeat != RepeatMode::Track
            && self.playback.status != PlaybackStatus::Stopped
        {
            self.next_index()
        } else {
            None
        };
        let wanted =
            wanted.and_then(|index| self.queue_track_path(index).map(|path| (index, path)));
        let gain = if self.prefetched != wanted {
            wanted
                .as_ref()
                .and_then(|(index, _)| self.queue.get(*index))
                .and_then(|id| self.track_index.get(id))
                .map(|index| self.replay_gain(&self.tracks[*index]))
                .transpose()?
                .flatten()
        } else {
            None
        };
        sync_prefetch_target(&mut self.prefetched, wanted, |path| {
            self.player.set_prefetch_with_gain(path, gain)
        })?;
        Ok(())
    }

    fn extend_auto_dj(&mut self) {
        use crate::audio::smart_mix::{LiveMixMode, transition_seconds};
        if self.config.live_mix_mode() != LiveMixMode::AutoDj
            || self.playback_context.is_album()
            || self.config.bit_perfect
            || self.repeat != RepeatMode::Off
            || self.next_index().is_some()
            || self.playback.status == PlaybackStatus::Stopped
        {
            return;
        }
        let Some(first) = self
            .current_track()
            .and_then(|track| self.mix_analyses.get(&track.id))
            .copied()
        else {
            return;
        };
        let attempt = (
            self.current_track().map(|track| track.id.clone()),
            self.mix_analyses.len(),
        );
        if self.auto_dj_attempt.as_ref() == Some(&attempt) {
            return;
        }
        self.auto_dj_attempt = Some(attempt);
        let candidate = crate::audio::smart_mix::recommend(
            first,
            &self.queue,
            self.tracks
                .iter()
                .filter(|track| track.available)
                .filter_map(|track| {
                    self.mix_analyses
                        .get(&track.id)
                        .copied()
                        .map(|info| (track, info))
                })
                .filter(|(track, info)| {
                    transition_seconds(first, *info, self.config.mix_crossfade_seconds) > 0.0
                        && track.path.exists()
                })
                .map(|(track, info)| (track.id.as_str(), info)),
            self.config.mix_crossfade_seconds,
        );
        if let Some(id) = candidate {
            self.queue.push(id);
            self.queue_dirty = true;
            self.dirty = true;
        }
    }

    /// Adopt the track mpv started on its own.
    ///
    /// The audio is already playing, so this only catches the application up:
    /// reloading here would undo the very gap this avoids.
    fn adopt_prefetched(&mut self, started: Option<&Path>) -> Result<()> {
        let Some((index, path)) = self.prefetched.take() else {
            return Ok(());
        };
        self.flush_history(true)?;
        // A transition event can already be waiting when a queue edit lands.
        // Never label audio from the old prefetch as the newly inserted track.
        if self.queue_track_path(index).as_ref() != Some(&path)
            || started.is_some_and(|started| started != path)
        {
            return self.next();
        }
        self.queue_index = Some(index);
        let Some(track) = self.current_track().cloned() else {
            return Ok(());
        };
        // The filter is applied a few tens of milliseconds into the track,
        // which is the accepted cost of not reloading.
        self.apply_replay_gain(&track)?;
        self.playback.status = PlaybackStatus::Playing;
        self.playback.position_ms = if started.is_some() {
            self.player.position_ms()
        } else {
            0
        };
        self.playback.duration_ms = track.duration_ms;
        if let Some(mix) = self.audible_mix.take().filter(|mix| mix.path == path) {
            self.playback.position_ms = self.playback.position_ms.max(mix.position_ms);
            self.tally = mix.tally;
        } else if self.config.history_enabled {
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

    /// A seek or skip controls the song already presented to the listener.
    /// Complete the overlap first, carrying its existing history rather than
    /// starting the incoming song a second time.
    fn promote_audible_mix(&mut self, position_ms: u64) -> Result<bool> {
        let Some(mix) = self.audible_mix.as_ref() else {
            return Ok(false);
        };
        let Some((index, path)) = self.prefetched.clone() else {
            return Ok(false);
        };
        if path != mix.path || self.queue.get(index) != Some(&mix.track_id) {
            return Ok(false);
        }
        self.flush_history(false)?;
        let mix = self.audible_mix.take().expect("validated audible mix");
        self.queue_index = Some(index);
        let track = self
            .current_track()
            .cloned()
            .expect("validated queued track");
        self.apply_replay_gain(&track)?;
        self.player.load(&track.path, position_ms)?;
        self.prefetched = None;
        self.tally = mix.tally;
        self.playback.position_ms = position_ms;
        self.playback.duration_ms = track.duration_ms;
        self.player
            .pause(self.playback.status == PlaybackStatus::Paused)?;
        self.dirty = true;
        self.persist_playback()?;
        Ok(true)
    }

    pub(super) fn next(&mut self) -> Result<()> {
        if let Some(mix) = &self.audible_mix {
            self.promote_audible_mix(mix.position_ms)?;
        }
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
        if let Some(mix) = &self.audible_mix {
            self.promote_audible_mix(mix.position_ms)?;
        }
        let current = self.queue_index.unwrap_or(0);
        let position_ms = self.player.position_ms();
        if let Some(index) = previous_playable(self.queue.len(), current, position_ms, |index| {
            self.queue_track_is_playable(index)
        }) {
            let was_paused = self.playback.status == PlaybackStatus::Paused;
            self.queue_index = Some(index);
            return self.load_current_with_pause(0, was_paused);
        }

        // Restart by seeking, not reopening the decoder and audio device.
        // This avoids a needless gap and preserves paused playback.
        if self.playback.status == PlaybackStatus::Stopped {
            return self.load_current(0);
        }
        self.player.seek_absolute_ms(0)?;
        self.playback.position_ms = 0;
        self.dirty = true;
        self.persist_playback()
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
        // Stepped in whole percent rather than by adding to what is already
        // there: adding 0.05 to an f64 nine times lands on 0.4499999999999999,
        // which reads back as 44 rather than 45.
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
        if playing && let Some(mix) = &mut self.audible_mix {
            let millis = elapsed.as_millis().min(u64::MAX as u128) as u64;
            mix.tally.listened_ms = mix.tally.listened_ms.saturating_add(millis);
            mix.tally.pending_ms = mix.tally.pending_ms.saturating_add(millis);
        }
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
                if let Some(mix) = &self.audible_mix {
                    let position = mix.position_ms.saturating_add_signed(offset_ms);
                    self.promote_audible_mix(position)?;
                } else {
                    self.player.seek_relative(offset_ms as f64 / 1000.0)?;
                }
            }
            PlayerAction::SeekAbsolute(position) => {
                if !self.promote_audible_mix(position)? {
                    self.player.seek_absolute_ms(position)?;
                }
            }
            PlayerAction::SetVolume(volume) => {
                self.playback.volume = volume.clamp(0.0, 1.0);
                self.player
                    .set_volume(self.config.volume_gain(self.playback.volume))?;
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
                self.player
                    .set_volume(self.config.volume_gain(self.playback.volume))?;
            }
            PlayerAction::SetShuffle(value) => self.set_shuffle(value),
            PlayerAction::SetRepeat(value) => self.repeat = value,
            PlayerAction::Quit => self.should_quit = true,
        }
        self.dirty = true;
        Ok(())
    }

    fn flush_audible_mix(&mut self) -> Result<()> {
        let Some(mix) = self.audible_mix.take() else {
            return Ok(());
        };
        if let Some(history_id) = mix.tally.entry {
            let duration = self
                .track_index
                .get(&mix.track_id)
                .map_or(0, |index| self.tracks[*index].duration_ms);
            let threshold = (duration / 2).min(240_000);
            self.db.update_history(HistoryUpdate {
                history_id,
                track_id: &mix.track_id,
                listened_delta_ms: mix.tally.pending_ms,
                position_ms: mix.position_ms,
                was_counted: mix.tally.counted,
                count_now: threshold > 0 && mix.tally.listened_ms >= threshold,
                completed: false,
            })?;
        }
        Ok(())
    }

    pub(super) fn handle_player_event(&mut self, event: PlayerEvent) -> Result<()> {
        let force_redraw = !matches!(&event, PlayerEvent::Position(_));
        match event {
            PlayerEvent::MixStarted {
                outgoing, incoming, ..
            } => {
                let valid = self
                    .current_track()
                    .is_some_and(|track| track.path == outgoing)
                    && self
                        .prefetched
                        .as_ref()
                        .is_some_and(|(_, path)| *path == incoming);
                if valid
                    && self.audible_mix.is_none()
                    && let Some(track) = self.tracks.iter().find(|track| track.path == incoming)
                {
                    let track_id = track.id.clone();
                    let mut tally = HistoryTally::new();
                    if self.config.history_enabled {
                        tally.restart(self.db.start_history(&track_id)?, track_id.clone());
                    }
                    self.audible_mix = Some(AudibleMix {
                        track_id,
                        path: incoming,
                        position_ms: 0,
                        progress: 0.0,
                        tally,
                    });
                }
            }
            PlayerEvent::MixProgress {
                incoming,
                position_ms,
                progress,
            } => {
                if let Some(mix) = &mut self.audible_mix
                    && mix.path == incoming
                {
                    mix.position_ms = position_ms;
                    mix.progress = progress;
                }
            }
            PlayerEvent::MixFinished { incoming } => {
                if let Some(mix) = &mut self.audible_mix
                    && mix.path == incoming
                {
                    mix.progress = 1.0;
                }
            }
            PlayerEvent::MixCancelled => self.flush_audible_mix()?,
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
            // mpv reports an amplitude; the control is a position on a
            // curve, and showing one as the other would make it jump every
            // time mpv echoed back what it was told.
            PlayerEvent::Volume(gain) => self.playback.volume = self.config.volume_position(gain),
            PlayerEvent::PrefetchedTrack(path) => self.adopt_prefetched(Some(&path))?,
            PlayerEvent::PlaylistPosition(position) => {
                // Anything past the first entry means mpv rolled into the track
                // queued behind this one.
                if position >= 1 {
                    self.adopt_prefetched(None)?;
                }
            }
            PlayerEvent::EndOfFile if self.repeat == RepeatMode::Track => {
                self.flush_history(true)?;
                self.load_current(0)?
            }
            // mpv is already starting the queued track; the playlist-pos change
            // does the bookkeeping. Advancing here would reload it and
            // reintroduce the gap this exists to remove.
            PlayerEvent::EndOfFile
                if self.prefetched.is_some() && self.player.capabilities().rolls_into_prefetch => {}
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
        self.presented_track().map(|track| {
            let mut hasher = DefaultHasher::new();
            track.id.hash(&mut hasher);
            hasher.finish()
        })
    }

    pub(super) fn publish_widget(&mut self) -> Result<()> {
        if self.last_widget_publish.elapsed() < Duration::from_secs(1) {
            return Ok(());
        }
        use crate::widget::{WidgetSnapshot, WidgetTrack};
        let state = self.presented_playback();
        let mut snapshot = WidgetSnapshot::new(&state, self.playback_context.clone());
        let mut hasher = DefaultHasher::new();
        self.queue.hash(&mut hasher);
        snapshot.queue_revision = hasher.finish();
        let logical_index = self.queue_index.unwrap_or(0);
        let presented_index = if self.audible_mix.is_some() {
            self.prefetched
                .as_ref()
                .map_or(logical_index, |(index, _)| *index)
        } else {
            logical_index
        };
        snapshot.logical_current = self
            .current_track()
            .map(|track| WidgetTrack::from_track(track, logical_index));
        snapshot.current = self
            .presented_track()
            .map(|track| WidgetTrack::from_track(track, presented_index));
        snapshot.mix_progress = self.audible_mix.as_ref().map(|mix| mix.progress);
        snapshot.upcoming = self
            .queue
            .iter()
            .enumerate()
            .skip(presented_index + 1)
            .filter_map(|(index, id)| {
                self.track_index
                    .get(id)
                    .map(|track_index| WidgetTrack::from_track(&self.tracks[*track_index], index))
            })
            .take(10)
            .collect();
        // A long or invalid cover URI cannot make playback fail.
        let _ = self._control_server.publish(&snapshot);
        self.last_widget_publish = Instant::now();
        Ok(())
    }

    pub(super) async fn sync_mpris(&mut self) {
        let presented = self.presented_playback();
        let index = self
            .audible_mix
            .as_ref()
            .and(self.prefetched.as_ref())
            .map_or(self.queue_index.unwrap_or(0), |(index, _)| *index);
        let state_signature = MediaSessionSignature {
            track_hash: self.current_track_hash(),
            status: self.playback.status,
            volume_bits: self.playback.volume.to_bits(),
            shuffle: self.shuffle && !self.playback_context.is_album(),
            repeat: self.repeat,
            can_previous: index > 0 || presented.position_ms > 5_000,
            can_next: index + 1 < self.queue.len(),
        };
        let position_signature = (presented.duration_ms, presented.position_ms / 1000);

        if let Some(mpris) = &self.mpris {
            if self.last_mpris_signature != Some(state_signature) {
                let _ = mpris
                    .sync(
                        self.presented_track(),
                        &presented,
                        self.shuffle && !self.playback_context.is_album(),
                        self.repeat,
                        state_signature.can_previous,
                        state_signature.can_next,
                    )
                    .await;
                self.last_mpris_signature = Some(state_signature);
            }
            if self.last_mpris_position_signature != Some(position_signature) {
                let _ = mpris.sync_position(&presented).await;
                self.last_mpris_position_signature = Some(position_signature);
            }
        }
    }

    pub(super) fn sync_discord(&mut self) {
        let presented = self.presented_playback();
        let signature = (
            self.current_track_hash(),
            presented.status,
            presented.duration_ms,
            presented.position_ms / 15_000,
        );
        if self.last_discord_signature == Some(signature) {
            return;
        }
        self.last_discord_signature = Some(signature);
        if let Some(discord) = &self.discord {
            discord.sync(self.presented_track(), &presented);
        }
    }

    pub(super) fn save_state(&mut self) -> Result<()> {
        self.flush_audible_mix()?;
        self.flush_history(false)?;
        self.persist_playback()
    }

    pub(super) fn playback_snapshot(&self) -> SavedPlayback {
        SavedPlayback {
            queue: Vec::new(),
            context: self.playback_context.clone(),
            current_index: self.queue_index,
            position_ms: self.playback.position_ms,
            volume: self.playback.volume,
            last_nonzero_volume: self.muted_volume,
            volume_is_position: true,
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
    use rand::{SeedableRng, rngs::StdRng};
    const ALL: fn(usize) -> bool = |_| true;

    #[test]
    fn play_next_replaces_a_prefetched_file_at_the_same_index() -> Result<()> {
        let mut queue = vec!["current.flac", "radiohead.flac"];
        let mut armed = Some((1, PathBuf::from(queue[1])));
        queue.insert(1, "marea.flac");
        let mut commands = Vec::new();
        sync_prefetch_target(&mut armed, Some((1, queue[1].into())), |path| {
            commands.push(path.map(Path::to_path_buf));
            Ok(())
        })?;
        assert_eq!(commands, vec![Some(PathBuf::from("marea.flac"))]);
        assert_eq!(armed, Some((1, PathBuf::from("marea.flac"))));
        Ok(())
    }

    #[test]
    fn reordering_upcoming_tracks_rearms_the_new_file() -> Result<()> {
        let mut queue = ["current.flac", "radiohead.flac", "marea.flac"];
        let mut armed = Some((1, PathBuf::from(queue[1])));
        queue.swap(1, 2);
        let mut commanded = None;
        sync_prefetch_target(&mut armed, Some((1, queue[1].into())), |path| {
            commanded = path.map(Path::to_path_buf);
            Ok(())
        })?;
        assert_eq!(commanded, Some(PathBuf::from("marea.flac")));
        Ok(())
    }

    #[test]
    fn unchanged_prefetch_does_not_restart_the_decoder() -> Result<()> {
        let mut armed = Some((1, PathBuf::from("marea.flac")));
        let wanted = armed.clone();
        sync_prefetch_target(&mut armed, wanted, |_| panic!("unchanged file rearmed"))
    }

    #[test]
    fn a_failed_prefetch_update_is_retried_and_can_be_withdrawn() -> Result<()> {
        let original = Some((1, PathBuf::from("radiohead.flac")));
        let mut armed = original.clone();
        let wanted = Some((1, PathBuf::from("marea.flac")));
        assert!(
            sync_prefetch_target(&mut armed, wanted.clone(), |_| {
                anyhow::bail!("backend unavailable")
            })
            .is_err()
        );
        assert_eq!(armed, original);
        sync_prefetch_target(&mut armed, wanted.clone(), |_| Ok(()))?;
        assert_eq!(armed, wanted);
        sync_prefetch_target(&mut armed, None, |path| {
            assert!(path.is_none());
            Ok(())
        })?;
        assert!(armed.is_none());
        Ok(())
    }

    #[test]
    fn previous_restarts_when_past_five_seconds() {
        assert_eq!(previous_playable(4, 3, 5_001, ALL), None);
        assert_eq!(previous_playable(4, 3, 5_000, ALL), Some(2));
    }

    #[test]
    fn previous_skips_unavailable_tracks() {
        assert_eq!(
            previous_playable(5, 4, 0, |index| index != 2 && index != 3),
            Some(1)
        );
        assert_eq!(previous_playable(3, 0, 0, ALL), None);
        assert_eq!(previous_playable(3, 2, 0, |_| false), None);
    }

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

    #[test]
    fn shuffle_keeps_the_current_and_played_tracks_in_place() {
        use rand::{SeedableRng, rngs::StdRng};

        let mut queue = vec!["a", "b", "current", "d", "e", "f", "g"];
        let original = queue.clone();
        let mut rng = StdRng::seed_from_u64(42);

        shuffle_upcoming(&mut queue, Some(2), &mut rng);

        assert_eq!(&queue[..=2], &original[..=2]);

        let mut expected_future = original[3..].to_vec();
        let mut actual_future = queue[3..].to_vec();
        expected_future.sort_unstable();
        actual_future.sort_unstable();

        assert_eq!(actual_future, expected_future);
        assert_ne!(&queue[3..], &original[3..]);
    }

    #[test]
    fn shuffle_without_a_current_track_uses_the_whole_queue() {
        let mut queue = vec!["a", "b", "c", "d", "e", "g"];
        let original = queue.clone();
        let mut rng = StdRng::seed_from_u64(42);

        shuffle_upcoming(&mut queue, None, &mut rng);

        let mut expected = original.clone();
        let mut actual = queue.clone();
        expected.sort_unstable();
        actual.sort_unstable();

        assert_eq!(original, expected);
        assert_ne!(queue, actual);
    }

    #[test]
    fn shuffle_with_the_current_track_last_changes_nothing() {
        let mut queue = vec!["a", "b", "current"];
        let original = queue.clone();
        let current = queue.len() - 1;
        let mut rng = StdRng::seed_from_u64(42);

        shuffle_upcoming(&mut queue, Some(current), &mut rng);

        assert_eq!(queue, original);
    }
}
