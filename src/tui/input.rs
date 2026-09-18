//! Input handling: keyboard, mouse and the modal editors.
//!
//! `handle_key` covers normal navigation; when a modal is open `handle_input`
//! takes over entirely and interprets the same keys per `InputMode`. Both are
//! reached through `handle_terminal_event`, which is what the event loop calls.

use super::*;

impl App {
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.input.is_some() {
            return self.handle_input(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc if self.view == View::AlbumDetail => self.close_album_detail(),
            KeyCode::Esc if self.view == View::ArtistDetail => self.close_artist_detail(),
            KeyCode::Esc if self.view == View::GenreDetail => {
                self.view = View::Genres;
                self.selected = self.genre_return_selection;
                self.opened_genre_name = None;
            }
            KeyCode::Esc if self.view == View::SmartPlaylistDetail => {
                self.view = View::SmartPlaylists;
                self.opened_smart_playlist = None;
                self.selected = 0;
            }
            KeyCode::Char('?') => {
                self.view = View::Help;
                self.selected = 0;
            }
            KeyCode::Char(',') => {
                self.view = View::Settings;
                self.selected = 0;
            }
            KeyCode::Char('m') => {
                self.compact = !self.compact;
                resize_terminal_for_mode(self.compact)?;
                self.status = if self.compact {
                    "Modo compacto activado"
                } else {
                    "Modo completo activado"
                }
                .into();
            }
            KeyCode::Char('/') => {
                self.view = View::Search;
                self.selected = 0;
                self.query.clear();
                self.refresh_search();
                self.input = Some(InputMode::Search);
                self.input_buffer.clear();
            }
            KeyCode::Char('c') => {
                self.input = Some(InputMode::NewPlaylist);
                self.input_buffer.clear();
            }
            KeyCode::Char('P') => {
                if let Some(id) = self.selected_track_id() {
                    if self.playlists.is_empty() {
                        let playlist = self.db.create_playlist("Mi playlist")?;
                        self.db.add_to_playlist(playlist, &id)?;
                        self.refresh_playlists()?;
                        self.status = "Añadida a Mi playlist".into();
                    } else {
                        self.input = Some(InputMode::ChoosePlaylist {
                            track_id: id,
                            selected: 0,
                        });
                    }
                }
            }
            KeyCode::Tab if self.view == View::GenreDetail => {
                self.genre_tab = (self.genre_tab + 1) % 3;
                self.selected = 0;
            }
            KeyCode::Tab => {
                self.focus = if self.focus == Focus::Sidebar {
                    Focus::Content
                } else {
                    Focus::Sidebar
                };
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if self.view == View::Settings => {
                self.adjust_setting(key.code)?
            }
            KeyCode::Left | KeyCode::Char('h') if self.view == View::AlbumDetail => {
                self.close_album_detail()
            }
            KeyCode::Left | KeyCode::Char('h')
                if matches!(self.view, View::Albums | View::ArtistDetail)
                    && self.focus == Focus::Content =>
            {
                self.move_album_selection(-1)
            }
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Sidebar,
            KeyCode::Right | KeyCode::Char('l')
                if matches!(self.view, View::Albums | View::ArtistDetail)
                    && self.focus == Focus::Content =>
            {
                self.move_album_selection(1)
            }
            KeyCode::Right | KeyCode::Char('l') => self.focus = Focus::Content,
            KeyCode::Up | KeyCode::Char('k')
                if matches!(self.view, View::Albums | View::ArtistDetail)
                    && self.focus == Focus::Content =>
            {
                self.move_album_selection(-(self.album_columns as isize))
            }
            KeyCode::Down | KeyCode::Char('j')
                if matches!(self.view, View::Albums | View::ArtistDetail)
                    && self.focus == Focus::Content =>
            {
                self.move_album_selection(self.album_columns as isize)
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Home => {
                self.selected = 0;
                self.dirty = true;
            }
            KeyCode::End => {
                self.selected = self.item_count().saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Enter if self.view == View::Settings => {
                self.adjust_setting(KeyCode::Char(' '))?
            }
            KeyCode::Enter => self.activate_or_open()?,
            KeyCode::Char('x') if self.selected_track_id().is_some() => {
                self.input = Some(InputMode::Context { selected: 0 })
            }
            KeyCode::Char('J') if self.view == View::Queue => self.move_queue_item(1),
            KeyCode::Char('K') if self.view == View::Queue => self.move_queue_item(-1),
            KeyCode::Delete | KeyCode::Char('d') if self.view == View::Queue => {
                self.remove_queue_item()
            }
            KeyCode::Char('C') if self.view == View::Queue && !self.queue.is_empty() => {
                self.input = Some(InputMode::ConfirmClearQueue)
            }
            KeyCode::Char('S') if self.view == View::Queue && !self.queue.is_empty() => {
                self.input_buffer.clear();
                self.input = Some(InputMode::SaveQueue)
            }
            KeyCode::Char('L') if self.view == View::Queue && !self.saved_queues.is_empty() => {
                self.input = Some(InputMode::LoadQueue { selected: 0 })
            }
            KeyCode::Char('e') if self.view == View::SmartPlaylists => {
                if let Some(playlist) = self.smart_playlists.get(self.selected).cloned() {
                    self.input = Some(InputMode::SmartEditor {
                        playlist,
                        selected: 0,
                    });
                }
            }
            KeyCode::Char(' ') => self.handle_action(PlayerAction::Toggle)?,
            KeyCode::Char('n') => self.handle_action(PlayerAction::Next)?,
            KeyCode::Char('p') => self.handle_action(PlayerAction::Previous)?,
            KeyCode::Char('s') => {
                self.shuffle = !self.shuffle;
                self.status = format!(
                    "Aleatorio {}",
                    if self.shuffle {
                        "activado"
                    } else {
                        "desactivado"
                    }
                );
                self.dirty = true;
            }
            KeyCode::Char('r') => {
                self.repeat = self.repeat.next();
                self.status = format!("Repetir: {:?}", self.repeat);
                self.dirty = true;
            }
            KeyCode::Char('a') => {
                if let Some(id) = self.selected_track_id() {
                    self.queue.push(id);
                    self.queue_dirty = true;
                    self.status = "Añadida a la cola".into();
                    self.dirty = true;
                }
            }
            KeyCode::Char('f') => {
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
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.handle_remote_action(RemoteCommand::VolumeUp)?
            }
            KeyCode::Char('-') => self.handle_remote_action(RemoteCommand::VolumeDown)?,
            _ => {}
        }
        Ok(())
    }

    fn handle_input(&mut self, key: KeyEvent) -> Result<()> {
        match self.input.clone().unwrap() {
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
                        self.status = format!("Añadida a {}", playlist.name);
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
                            self.status = format!("Cola cargada: {}", queue.name);
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
                    self.status = "Cola vaciada".into();
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
                        self.status = "Lista inteligente guardada".into();
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
                            self.status = format!("Playlist creada: {}", self.input_buffer.trim());
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
                            self.status = format!("Cola guardada: {name}");
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
            MouseEventKind::ScrollUp
                if matches!(self.view, View::Albums | View::ArtistDetail)
                    && self.focus == Focus::Content =>
            {
                self.move_album_selection(-(self.album_columns as isize))
            }
            MouseEventKind::ScrollDown
                if matches!(self.view, View::Albums | View::ArtistDetail)
                    && self.focus == Focus::Content =>
            {
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
                    self.opened_album_key = None;
                    self.opened_artist_name = None;
                    self.artist_release_keys.clear();
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

    fn run_context_action(&mut self, action: usize) -> Result<()> {
        let Some(track_id) = self.selected_track_id() else {
            return Ok(());
        };
        match action {
            0 => self.activate_selection()?,
            1 => {
                let position = self.queue_index.map_or(0, |index| index + 1);
                self.queue.insert(position.min(self.queue.len()), track_id);
                self.queue_dirty = true;
                self.status = "Se reproducirá después".into();
            }
            2 => {
                self.queue.push(track_id);
                self.queue_dirty = true;
                self.status = "Añadida al final de la cola".into();
            }
            3 => {
                let favorite = self.db.toggle_favorite(&track_id)?;
                self.set_favorite_local(&track_id, favorite);
                self.status = if favorite {
                    "Añadida a favoritos"
                } else {
                    "Eliminada de favoritos"
                }
                .into();
            }
            4 => {
                self.input = Some(InputMode::ChoosePlaylist {
                    track_id,
                    selected: 0,
                });
            }
            5 => {
                if let Some(track) = self
                    .track_index
                    .get(&track_id)
                    .and_then(|index| self.tracks.get(*index))
                    && let Some(album) = self.albums.iter().find(|album| {
                        album.title.eq_ignore_ascii_case(&track.album)
                            && album.artist.eq_ignore_ascii_case(&track.album_artist)
                    })
                {
                    self.opened_album_key = Some(album.key.clone());
                    self.album_parent_view = self.view;
                    self.view = View::AlbumDetail;
                    self.selected = album
                        .track_ids
                        .iter()
                        .position(|id| id == &track_id)
                        .unwrap_or(0);
                }
            }
            6 => {
                if let Some(track) = self
                    .track_index
                    .get(&track_id)
                    .and_then(|index| self.tracks.get(*index))
                {
                    self.artist_parent_view = self.view;
                    self.opened_artist_name = Some(track.artist.clone());
                    self.refresh_artist_releases();
                    self.view = View::ArtistDetail;
                    self.selected = 0;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn adjust_setting(&mut self, key: KeyCode) -> Result<()> {
        let increase = matches!(key, KeyCode::Right);
        let horizontal = matches!(key, KeyCode::Left | KeyCode::Right);
        match self.selected {
            0 => self.config.replaygain_enabled = !self.config.replaygain_enabled,
            1 => {
                self.config.replaygain_mode = match self.config.replaygain_mode {
                    ReplayGainMode::Album => ReplayGainMode::Track,
                    ReplayGainMode::Track => ReplayGainMode::Album,
                }
            }
            2 => {
                if !horizontal {
                    return Ok(());
                }
                self.config.replaygain_target_lufs = (self.config.replaygain_target_lufs
                    + if increase { 1.0 } else { -1.0 })
                .clamp(-30.0, -5.0)
            }
            3 => self.config.resume_enabled = !self.config.resume_enabled,
            4 => self.config.history_enabled = !self.config.history_enabled,
            5 => self.config.compact_default = !self.config.compact_default,
            6 => self.config.show_covers = !self.config.show_covers,
            7 => {
                if !horizontal {
                    return Ok(());
                }
                self.config.volume_step = if increase {
                    self.config.volume_step.saturating_add(1).min(20)
                } else {
                    self.config.volume_step.saturating_sub(1).max(1)
                }
            }
            8 => self.config.auto_discover_removable = !self.config.auto_discover_removable,
            9 => {
                if !horizontal {
                    return Ok(());
                }
                self.config.cover_cache_mb = if increase {
                    self.config.cover_cache_mb.saturating_add(8).min(512)
                } else {
                    self.config.cover_cache_mb.saturating_sub(8).max(16)
                };
            }
            10 => {
                if self.config.discord_application_id.is_none() {
                    self.status =
                        "Configura Discord con: muscli setup discord APPLICATION_ID".into();
                    return Ok(());
                }
                self.config.discord_enabled = !self.config.discord_enabled;
                self.discord = if self.config.discord_enabled {
                    self.config
                        .discord_application_id
                        .clone()
                        .map(|application_id| {
                            DiscordPresence::start(
                                application_id,
                                self.config.discord_large_image.clone(),
                            )
                        })
                } else {
                    None
                };
            }
            11 => {
                if horizontal {
                    return Ok(());
                }
                self.start_scan();
                self.status = "Reescaneo iniciado".into();
            }
            12 => {
                if horizontal {
                    return Ok(());
                }
                self.start_gain_analysis()?;
            }
            _ => {}
        }
        self.config.save(&self.paths)?;
        self.status = "Settings guardados".into();
        self.dirty = true;
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
        "favorite" | "available" | "played" => serde_json::Value::Bool(matches!(
            value.to_lowercase().as_str(),
            "1" | "true" | "on" | "si" | "sí"
        )),
        "play_count" | "duration_ms" | "added_days" | "last_played_days" => {
            serde_json::Value::from(value.parse::<i64>().unwrap_or_default())
        }
        _ => serde_json::Value::String(value.to_owned()),
    };
}
