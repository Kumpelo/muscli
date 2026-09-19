//! Input handling: keyboard, mouse and the modal editors.
//!
//! `handle_key` covers normal navigation; when a modal is open `handle_input`
//! takes over entirely and interprets the same keys per `InputMode`. Both are
//! reached through `handle_terminal_event`, which is what the event loop calls.

use super::keys::{self, Action};
use super::*;
use crate::t;

impl App {
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // A modal owns the keyboard completely while it is open.
        if self.input.is_some() {
            return self.handle_input(key);
        }
        match keys::resolve_with(&self.bindings, &key, self.view, self.focus) {
            Some(action) => self.apply(action),
            None => Ok(()),
        }
    }

    /// Carry out a resolved action.
    ///
    /// Bindings decide *where* a key applies; this decides whether the action
    /// is possible right now. An action with nothing to act on is a no-op, not
    /// an error: pressing `f` with no track selected should do nothing.
    fn apply(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back => self.close_detail(),
            Action::OpenView(view) => {
                self.view = view;
                self.selected = 0;
            }
            Action::ToggleCompact => {
                self.compact = !self.compact;
                resize_terminal_for_mode(self.compact)?;
                self.status = t!(if self.compact {
                    "status.compact_on"
                } else {
                    "status.compact_off"
                })
                .into();
            }
            Action::OpenSearch => {
                self.view = View::Search;
                self.selected = 0;
                self.query.clear();
                self.refresh_search();
                self.input = Some(InputMode::Search);
                self.input_buffer.clear();
            }
            Action::NewPlaylist => {
                self.input = Some(InputMode::NewPlaylist);
                self.input_buffer.clear();
            }
            Action::AddSelectedToPlaylist => {
                if let Some(id) = self.selected_track_id() {
                    if self.playlists.is_empty() {
                        let playlist = self.db.create_playlist(t!("label.default_playlist"))?;
                        self.db.add_to_playlist(playlist, &id)?;
                        self.refresh_playlists()?;
                        self.status = t!("status.added_to_default_playlist").into();
                    } else {
                        self.input = Some(InputMode::ChoosePlaylist {
                            track_id: id,
                            selected: 0,
                        });
                    }
                }
            }
            Action::NextGenreTab => {
                self.genre_tab = (self.genre_tab + 1) % 3;
                self.selected = 0;
            }
            Action::ToggleFocus => {
                self.focus = if self.focus == Focus::Sidebar {
                    Focus::Content
                } else {
                    Focus::Sidebar
                };
            }
            Action::FocusSidebar => self.focus = Focus::Sidebar,
            Action::FocusContent => self.focus = Focus::Content,
            Action::MoveSelection(amount) => self.move_selection(amount),
            Action::AlbumStep(amount) => self.move_album_selection(amount),
            Action::AlbumRow(rows) => self.move_album_selection(rows * self.album_columns as isize),
            Action::SelectFirst => {
                self.selected = 0;
                self.dirty = true;
            }
            Action::SelectLast => {
                self.selected = self.item_count().saturating_sub(1);
                self.dirty = true;
            }
            Action::Activate => self.activate_or_open()?,
            Action::OpenContextMenu => {
                if self.selected_track_id().is_some() {
                    self.input = Some(InputMode::Context { selected: 0 });
                }
            }
            Action::QueueMove(amount) => self.move_queue_item(amount),
            Action::QueueRemove => self.remove_queue_item(),
            Action::QueueClear => {
                if !self.queue.is_empty() {
                    self.input = Some(InputMode::ConfirmClearQueue);
                }
            }
            Action::QueueSave => {
                if !self.queue.is_empty() {
                    self.input_buffer.clear();
                    self.input = Some(InputMode::SaveQueue);
                }
            }
            Action::QueueLoad => {
                if !self.saved_queues.is_empty() {
                    self.input = Some(InputMode::LoadQueue { selected: 0 });
                }
            }
            Action::EditSmartPlaylist => {
                if let Some(playlist) = self.smart_playlists.get(self.selected).cloned() {
                    self.input = Some(InputMode::SmartEditor {
                        playlist,
                        selected: 0,
                    });
                }
            }
            Action::Player(player_action) => self.handle_action(player_action)?,
            Action::ToggleShuffle => {
                self.shuffle = !self.shuffle;
                self.status = t!(if self.shuffle {
                    "status.shuffle_on"
                } else {
                    "status.shuffle_off"
                })
                .into();
                self.dirty = true;
            }
            Action::CycleRepeat => {
                self.repeat = self.repeat.next();
                self.status = t!(match self.repeat {
                    RepeatMode::Off => "status.repeat_off",
                    RepeatMode::Track => "status.repeat_track",
                    RepeatMode::Queue => "status.repeat_queue",
                })
                .into();
                self.dirty = true;
            }
            Action::EnqueueSelected => {
                if let Some(id) = self.selected_track_id() {
                    self.queue.push(id);
                    self.queue_dirty = true;
                    self.status = t!("status.added_to_queue").into();
                    self.dirty = true;
                }
            }
            Action::ToggleFavorite => {
                if let Some(id) = self.selected_track_id() {
                    let value = self.db.toggle_favorite(&id)?;
                    self.set_favorite_local(&id, value);
                    self.status = if value {
                        "Añadida a favoritos"
                    } else {
                        "Eliminada de favoritos"
                    }
                    .into();
                }
            }
            Action::Remote(command) => self.handle_remote_action(command)?,
            Action::Setting(input) => self.adjust_setting(input)?,
        }
        Ok(())
    }

    fn handle_input(&mut self, key: KeyEvent) -> Result<()> {
        // Only reached while a modal is open, but a panic here would leave the
        // terminal in raw mode with no way back.
        let Some(mode) = self.input.clone() else {
            return Ok(());
        };
        match mode {
            InputMode::ChoosePlaylist {
                track_id,
                mut selected,
            } => match key.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.input = Some(InputMode::ChoosePlaylist { track_id, selected });
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(self.playlists.len().saturating_sub(1));
                    self.input = Some(InputMode::ChoosePlaylist { track_id, selected });
                }
                KeyCode::Enter => {
                    if let Some(playlist) = self.playlists.get_mut(selected) {
                        self.db.add_to_playlist(playlist.id, &track_id)?;
                        playlist.track_ids.push(track_id);
                        self.status = t!("status.added_to_playlist", name = playlist.name);
                        self.dirty = true;
                    }
                    self.input = None;
                }
                _ => {}
            },
            InputMode::LoadQueue { mut selected } => {
                match key.code {
                    KeyCode::Esc => self.input = None,
                    KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(self.saved_queues.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        if let Some(queue) = self.saved_queues.get(selected) {
                            self.queue = queue.track_ids.clone();
                            self.queue_index = (!self.queue.is_empty()).then_some(0);
                            self.queue_dirty = true;
                            self.status = t!("status.queue_loaded", name = queue.name);
                        }
                        self.input = None;
                    }
                    _ => {}
                }
                if matches!(self.input, Some(InputMode::LoadQueue { .. })) {
                    self.input = Some(InputMode::LoadQueue { selected });
                }
            }
            InputMode::ConfirmClearQueue => match key.code {
                KeyCode::Char('y') | KeyCode::Char('s') | KeyCode::Enter => {
                    self.queue.clear();
                    self.queue_index = None;
                    self.queue_dirty = true;
                    self.mpv.stop()?;
                    self.playback.status = PlaybackStatus::Stopped;
                    self.input = None;
                    self.status = t!("status.queue_cleared").into();
                }
                KeyCode::Esc | KeyCode::Char('n') => self.input = None,
                _ => {}
            },
            InputMode::Context { mut selected } => {
                match key.code {
                    KeyCode::Esc => self.input = None,
                    KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(CONTEXT_ACTIONS.len() - 1)
                    }
                    KeyCode::Enter => {
                        self.run_context_action(selected)?;
                        if !matches!(self.input, Some(InputMode::ChoosePlaylist { .. })) {
                            self.input = None;
                        }
                    }
                    _ => {}
                }
                if matches!(self.input, Some(InputMode::Context { .. })) {
                    self.input = Some(InputMode::Context { selected });
                }
            }
            InputMode::SmartEditor {
                mut playlist,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Esc => self.input = None,
                    KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        selected = (selected + 1).min(playlist.rules.len().saturating_sub(1))
                    }
                    KeyCode::Char('m') => {
                        playlist.match_mode =
                            if playlist.match_mode == crate::model::SmartMatch::All {
                                crate::model::SmartMatch::Any
                            } else {
                                crate::model::SmartMatch::All
                            };
                    }
                    KeyCode::Char('a') => playlist.rules.push(SmartRule {
                        field: "genre".into(),
                        operator: "contains".into(),
                        value: serde_json::Value::String(String::new()),
                    }),
                    KeyCode::Char('d') if !playlist.rules.is_empty() => {
                        playlist.rules.remove(selected);
                        selected = selected.min(playlist.rules.len().saturating_sub(1));
                    }
                    KeyCode::Tab if !playlist.rules.is_empty() => {
                        cycle_smart_rule(&mut playlist.rules[selected]);
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.db.save_smart_playlist(&playlist)?;
                        self.refresh_smart_playlists()?;
                        self.input = None;
                        self.status = t!("status.smart_saved").into();
                        self.dirty = true;
                        return Ok(());
                    }
                    KeyCode::Char('s') => cycle_smart_sort(&mut playlist),
                    KeyCode::Char('l') => {
                        playlist.limit = match playlist.limit {
                            None => Some(25),
                            Some(25) => Some(100),
                            _ => None,
                        }
                    }
                    KeyCode::Enter if !playlist.rules.is_empty() => {
                        self.input_buffer = display_rule_value(&playlist.rules[selected]);
                        self.input = Some(InputMode::SmartValue { playlist, selected });
                        self.dirty = true;
                        return Ok(());
                    }
                    _ => {}
                }
                if matches!(self.input, Some(InputMode::SmartEditor { .. })) {
                    self.input = Some(InputMode::SmartEditor { playlist, selected });
                }
            }
            InputMode::SmartValue {
                mut playlist,
                selected,
            } => match key.code {
                KeyCode::Esc => self.input = Some(InputMode::SmartEditor { playlist, selected }),
                KeyCode::Enter => {
                    set_rule_value(&mut playlist.rules[selected], self.input_buffer.trim());
                    self.input_buffer.clear();
                    self.input = Some(InputMode::SmartEditor { playlist, selected });
                }
                KeyCode::Backspace => {
                    self.input_buffer.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input_buffer.push(character)
                }
                _ => {}
            },
            mode @ (InputMode::Search | InputMode::NewPlaylist | InputMode::SaveQueue) => {
                match key.code {
                    KeyCode::Esc => {
                        self.input = None;
                        self.input_buffer.clear();
                    }
                    KeyCode::Enter => {
                        if mode == InputMode::NewPlaylist && !self.input_buffer.trim().is_empty() {
                            self.db.create_playlist(self.input_buffer.trim())?;
                            self.refresh_playlists()?;
                            self.status =
                                t!("status.playlist_created", name = self.input_buffer.trim());
                        }
                        if mode == InputMode::Search {
                            self.query = self.input_buffer.clone();
                            self.refresh_search();
                        }
                        if mode == InputMode::SaveQueue && !self.input_buffer.trim().is_empty() {
                            let name = self.input_buffer.trim().to_owned();
                            let queue = self.queue.clone();
                            self.db.save_queue(&name, &queue)?;
                            self.saved_queues = self.db.load_saved_queues()?;
                            self.status = t!("status.queue_saved", name = name);
                        }
                        self.input = None;
                    }
                    KeyCode::Backspace => {
                        self.input_buffer.pop();
                        if mode == InputMode::Search {
                            self.query = self.input_buffer.clone();
                            self.refresh_search();
                            self.selected = 0;
                        }
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.input_buffer.push(c);
                        if mode == InputMode::Search {
                            self.query = self.input_buffer.clone();
                            self.refresh_search();
                            self.selected = 0;
                        }
                    }
                    _ => {}
                }
            }
        }
        self.dirty = true;
        Ok(())
    }

    pub(super) fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::ScrollUp if keys::Scope::AlbumGrid.matches(self.view, self.focus) => {
                self.move_album_selection(-(self.album_columns as isize))
            }
            MouseEventKind::ScrollDown if keys::Scope::AlbumGrid.matches(self.view, self.focus) => {
                self.move_album_selection(self.album_columns as isize)
            }
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::Down(MouseButton::Left)
                if mouse.column < 22 && (4..(4 + VIEWS.len() as u16)).contains(&mouse.row) =>
            {
                let index = (mouse.row - 4) as usize;
                if let Some(view) = VIEWS.get(index) {
                    self.view = *view;
                    self.clear_nav();
                    self.selected = 0;
                    self.focus = Focus::Content;
                    self.dirty = true;
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.selected_track_id().is_some() => {
                self.input = Some(InputMode::Context { selected: 0 });
                self.dirty = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn run_context_action(&mut self, index: usize) -> Result<()> {
        let Some(track_id) = self.selected_track_id() else {
            return Ok(());
        };
        let Some((action, _)) = CONTEXT_ACTIONS.get(index) else {
            return Ok(());
        };
        match action {
            ContextAction::PlayNow => self.activate_selection()?,
            ContextAction::PlayNext => {
                let position = self.queue_index.map_or(0, |index| index + 1);
                self.queue.insert(position.min(self.queue.len()), track_id);
                self.queue_dirty = true;
                self.status = t!("status.playing_next").into();
            }
            ContextAction::Enqueue => {
                self.queue.push(track_id);
                self.queue_dirty = true;
                self.status = t!("status.added_to_queue_end").into();
            }
            ContextAction::ToggleFavorite => {
                let favorite = self.db.toggle_favorite(&track_id)?;
                self.set_favorite_local(&track_id, favorite);
                self.status = if favorite {
                    "Añadida a favoritos"
                } else {
                    "Eliminada de favoritos"
                }
                .into();
            }
            ContextAction::AddToPlaylist => {
                self.input = Some(InputMode::ChoosePlaylist {
                    track_id,
                    selected: 0,
                });
            }
            ContextAction::ShowAlbum => {
                if let Some(track) = self
                    .track_index
                    .get(&track_id)
                    .and_then(|index| self.tracks.get(*index))
                    && let Some(album) = self.albums.iter().find(|album| {
                        album.title.eq_ignore_ascii_case(&track.album)
                            && album.artist.eq_ignore_ascii_case(&track.album_artist)
                    })
                {
                    let key = album.key.clone();
                    let position = album
                        .track_ids
                        .iter()
                        .position(|id| id == &track_id)
                        .unwrap_or(0);
                    self.push_nav(NavTarget::Album(key), View::AlbumDetail);
                    self.selected = position;
                }
            }
            ContextAction::ShowArtist => {
                if let Some(track) = self
                    .track_index
                    .get(&track_id)
                    .and_then(|index| self.tracks.get(*index))
                {
                    let name = track.artist.clone();
                    self.push_nav(NavTarget::Artist(name), View::ArtistDetail);
                    self.refresh_artist_releases();
                }
            }
        }
        Ok(())
    }
}

pub(super) fn handle_terminal_event(app: &mut App, event: Event) -> Result<()> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
        Event::Mouse(mouse) => app.handle_mouse(mouse),
        Event::Resize(_, _) => {
            app.dirty = true;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn cycle_smart_rule(rule: &mut SmartRule) {
    const FIELDS: [&str; 11] = [
        "genre",
        "title",
        "artist",
        "album",
        "favorite",
        "available",
        "played",
        "play_count",
        "duration_ms",
        "added_days",
        "last_played_days",
    ];
    let current = FIELDS
        .iter()
        .position(|field| *field == rule.field)
        .unwrap_or(0);
    rule.field = FIELDS[(current + 1) % FIELDS.len()].into();
    match rule.field.as_str() {
        "favorite" | "available" | "played" => {
            rule.operator = "is".into();
            rule.value = serde_json::Value::Bool(true);
        }
        "play_count" | "duration_ms" => {
            rule.operator = "gte".into();
            rule.value = serde_json::Value::from(1);
        }
        "added_days" | "last_played_days" => {
            rule.operator = "lte".into();
            rule.value = serde_json::Value::from(30);
        }
        _ => {
            rule.operator = "contains".into();
            rule.value = serde_json::Value::String(String::new());
        }
    }
}

fn cycle_smart_sort(playlist: &mut SmartPlaylist) {
    const SORTS: [&str; 5] = ["title", "added_at", "last_played", "play_count", "duration"];
    let current = SORTS
        .iter()
        .position(|field| *field == playlist.sort_field)
        .unwrap_or(0);
    playlist.sort_field = SORTS[(current + 1) % SORTS.len()].into();
    playlist.descending = matches!(
        playlist.sort_field.as_str(),
        "added_at" | "last_played" | "play_count" | "duration"
    );
}

pub(super) fn display_rule_value(rule: &SmartRule) -> String {
    match &rule.value {
        serde_json::Value::String(value) => value.clone(),
        value => value.to_string(),
    }
}

fn set_rule_value(rule: &mut SmartRule, value: &str) {
    rule.value = match rule.field.as_str() {
        // Which words count as yes is itself translated: a Spanish user
        // typing "sí" should not have it read as false.
        "favorite" | "available" | "played" => {
            let typed = value.trim().to_lowercase();
            serde_json::Value::Bool(t!("truthy").split(',').any(|option| option == typed))
        }
        "play_count" | "duration_ms" | "added_days" | "last_played_days" => {
            serde_json::Value::from(value.parse::<i64>().unwrap_or_default())
        }
        _ => serde_json::Value::String(value.to_owned()),
    };
}
