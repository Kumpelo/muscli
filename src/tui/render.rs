//! Rendering for the terminal UI.
//!
//! This module draws `App`; it never mutates application state except for the
//! image protocols and cover-placement signature that drawing itself owns.
//!
//! `use super::*` pulls in `App`, the view enums, the shared constants and the
//! imports declared in the parent module.

use super::*;
use crate::{model::artist_label, t};

/// Mix a place an image can be drawn into the signature.
///
/// Where, not what: an image appearing in a slot that was already reserved
/// changes nothing, because the cells it lands on are wiped and rewritten on
/// their own. Only a slot that moved leaves a band of the old image behind.
fn note_cover_placement(signature: &mut u64, key: &str, area: Rect) {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    (area.x, area.y, area.width, area.height).hash(&mut h);
    *signature = signature.rotate_left(13) ^ h.finish();
}

/// Keep a single outer gutter, giving it back on narrow terminals.
fn content_area(area: Rect) -> Rect {
    area.inner(Margin {
        horizontal: if area.width >= 64 { 1 } else { 0 },
        vertical: 0,
    })
}

fn interface_rows(area: Rect) -> [Rect; 4] {
    Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area)
}

fn body_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    if area.width >= 92 {
        Layout::horizontal([
            Constraint::Length(21),
            Constraint::Min(36),
            Constraint::Length(30),
        ])
        .split(area)
    } else {
        Layout::horizontal([Constraint::Length(18), Constraint::Min(28)]).split(area)
    }
}

fn album_card_borders(height: u16) -> Borders {
    if height >= 4 {
        Borders::RIGHT | Borders::BOTTOM
    } else {
        Borders::RIGHT
    }
}

fn album_card_rows(area: Rect) -> [Rect; 2] {
    // Preserve the three metadata lines before assigning space to the cover.
    Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(area)
}

/// Clear the old graphics placement, then restore every cell to the current
/// theme. Clear alone resets cells to the terminal default (black in many
/// Windows profiles), leaving dark gutters beside narrow covers in light mode.
fn clear_cover_area(frame: &mut Frame<'_>, area: Rect, theme: UiTheme) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().fg(theme.foreground).bg(theme.background)),
        area,
    );
}

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    app.covers.pending_signature = 0;
    let area = frame.area();
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.background),
        ),
        area,
    );
    let area = content_area(area);
    app.sidebar_hitbox = Rect::default();
    if app.compact {
        draw_compact(frame, app);
        if app.input.is_some() {
            draw_modal(frame, app);
        }
        return;
    }
    let rows = interface_rows(area);
    draw_header(frame, rows[0], app.theme);
    let body = body_columns(rows[1]);
    draw_sidebar(frame, body[0], app);
    draw_content(frame, body[1], app);
    if body.len() > 2 {
        draw_details(frame, body[2], app);
    }
    draw_player(frame, rows[2], app);
    frame.render_widget(
        Paragraph::new(app.status.clone()).style(Style::default().fg(app.theme.muted)),
        rows[3],
    );
    if app.input.is_some() {
        draw_modal(frame, app);
    }
}

fn draw_compact(frame: &mut Frame<'_>, app: &mut App) {
    let area = content_area(frame.area());
    let rows = interface_rows(area);
    draw_header(frame, rows[0], app.theme);
    let body =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(rows[1]);
    draw_details(frame, body[0], app);
    let queue_indices = app
        .queue
        .iter()
        .enumerate()
        .skip(app.queue_index.unwrap_or(0))
        .take(body[1].height.saturating_sub(2) as usize)
        .filter_map(|(position, id)| {
            app.track_index
                .get(id)
                .map(|index| (position, &app.tracks[*index]))
        })
        .map(|(position, track)| {
            ListItem::new(format!(
                "{:02}  {} — {}",
                position + 1,
                track.title,
                artist_label(&track.artist)
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(queue_indices).block(
            Block::default()
                .title(t!("panel.compact_queue"))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.accent)),
        ),
        body[1],
    );
    draw_player(frame, rows[2], app);
    frame.render_widget(
        Paragraph::new(app.status.clone()).style(Style::default().fg(app.theme.muted)),
        rows[3],
    );
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, theme: UiTheme) {
    let title = Line::from(vec![
        Span::styled(
            "  mus",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "cli",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme.border)),
        ),
        area,
    );
}

fn draw_sidebar(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let items = VIEWS
        .iter()
        .map(|view| {
            let count = match view {
                View::Albums => app.albums.len(),
                View::Artists => app.artists.len(),
                View::Genres => app.genres.len(),
                View::Tracks => app.tracks.len(),
                View::Playlists => app.playlists.len(),
                View::SmartPlaylists => app.smart_playlists.len(),
                View::Favorites => app.tracks.iter().filter(|t| t.favorite).count(),
                View::History => app.history.len(),
                View::Queue => app.queue.len(),
                _ => 0,
            };
            let suffix = if count > 0 {
                format!("  {count}")
            } else {
                String::new()
            };
            ListItem::new(format!(" {}  {}{}", view.icon(), view.title(), suffix))
        })
        .collect::<Vec<_>>();
    let sidebar_view = match app.view {
        View::ArtistDetail => View::Artists,
        View::GenreDetail => View::Genres,
        View::SmartPlaylistDetail => View::SmartPlaylists,
        View::AlbumDetail if app.nav_parent() == Some(View::ArtistDetail) => View::Artists,
        View::AlbumDetail => View::Albums,
        view => view,
    };
    let mut state = ListState::default().with_selected(Some(
        VIEWS
            .iter()
            .position(|view| *view == sidebar_view)
            .unwrap_or(0),
    ));
    let block =
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(if app.focus == Focus::Sidebar {
                Style::default().fg(app.theme.accent)
            } else {
                Style::default().fg(app.theme.border)
            });
    app.sidebar_hitbox = block.inner(area);
    frame.render_stateful_widget(
        List::new(items).block(block).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection)
                .add_modifier(Modifier::BOLD),
        ),
        area,
        &mut state,
    );
    app.sidebar_offset = state.offset();
}

fn draw_content(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = match app.view {
        View::AlbumDetail => app
            .opened_album()
            .map(|album| t!("panel.back_hint", name = album.title))
            .unwrap_or_else(|| format!(" {} ", t!("view.album"))),
        View::ArtistDetail => app
            .opened_artist_name()
            .map(|artist| t!("panel.artist_hint", name = artist_label(artist)))
            .unwrap_or_else(|| format!(" {} ", t!("view.artist"))),
        View::GenreDetail => app
            .opened_genre_name()
            .map(|genre| t!("panel.genre_hint", name = genre))
            .unwrap_or_else(|| format!(" {} ", t!("view.genre"))),
        View::SmartPlaylistDetail => app
            .opened_smart_playlist()
            .and_then(|id| {
                app.smart_playlists
                    .iter()
                    .find(|playlist| playlist.id == id)
            })
            .map(|playlist| t!("panel.back_hint", name = playlist.display_name()))
            .unwrap_or_else(|| " Lista inteligente ".into()),
        _ => format!(" {} ", app.view.title()),
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if app.focus == Focus::Content {
            Style::default().fg(app.theme.accent)
        } else {
            Style::default().fg(app.theme.border)
        });
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match app.view {
        View::Home => draw_home(frame, inner, app),
        View::Albums | View::ArtistDetail => draw_albums(frame, inner, app),
        View::Artists => draw_artists(frame, inner, app),
        View::Genres => draw_genres(frame, inner, app),
        View::GenreDetail => draw_genre_detail(frame, inner, app),
        View::Playlists => draw_playlists(frame, inner, app),
        View::SmartPlaylists => draw_smart_playlists(frame, inner, app),
        View::Settings => draw_settings(frame, inner, app),
        View::Help => draw_help(frame, inner, app),
        View::Lyrics => draw_lyrics(frame, inner, app),
        View::AlbumDetail
        | View::Tracks
        | View::Favorites
        | View::History
        | View::Search
        | View::Queue
        | View::SmartPlaylistDetail => draw_tracks(frame, inner, app),
    }
}

fn draw_home(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let ids = app.home_track_ids();
    if ids.is_empty() {
        frame.render_widget(
            Paragraph::new(t!("empty.home"))
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.muted)),
            area,
        );
        return;
    }
    let items = ids
        .iter()
        .enumerate()
        .take(20)
        .filter_map(|(_, id)| {
            let track = app.track_index.get(id).map(|index| &app.tracks[*index])?;
            let progress = app
                .stats
                .get(id)
                .map_or(0, |stats| stats.resume_position_ms);
            let section = if progress >= 30_000 {
                "Continuar"
            } else {
                "Reciente"
            };
            Some(ListItem::new(format!(
                "{section:<9}  {:<42}  {:<24}  {} / {}",
                track.title,
                artist_label(&track.artist),
                format_duration(progress),
                format_duration(track.duration_ms)
            )))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection)
                .add_modifier(Modifier::BOLD),
        ),
        area,
        &mut state,
    );
}

fn list_viewport(total: usize, selected: usize, area_height: u16) -> std::ops::Range<usize> {
    let visible_rows = area_height as usize;
    if total == 0 || visible_rows == 0 {
        return 0..0;
    }
    let selected = selected.min(total - 1);
    let first = selected.saturating_sub(visible_rows - 1);
    first..(first + visible_rows).min(total)
}

fn draw_genres(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let viewport = list_viewport(app.genres.len(), app.selected, area.height);
    let first = viewport.start;
    let items = app.genres[viewport]
        .iter()
        .map(|genre| {
            ListItem::new(format!(
                "{}  {:<36} {}",
                icons::display("󰌳", "G"),
                genre.name,
                t!("label.genre_summary", tracks = genre.track_ids.len())
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected.saturating_sub(first)));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection)
                .add_modifier(Modifier::BOLD),
        ),
        area,
        &mut state,
    );
}

fn draw_genre_detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let tabs = [t!("tab.albums"), t!("tab.artists"), t!("tab.tracks")];
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(area);
    frame.render_widget(
        Paragraph::new(
            tabs.iter()
                .enumerate()
                .map(|(index, title)| {
                    Span::styled(
                        format!("  {title}  "),
                        if index == app.genre_tab {
                            Style::default()
                                .fg(app.theme.foreground)
                                .bg(app.theme.selection)
                        } else {
                            Style::default().fg(app.theme.muted)
                        },
                    )
                })
                .collect::<Line>(),
        ),
        rows[0],
    );
    let total = match app.genre_tab {
        0 => app.genre_album_indices().len(),
        1 => app.genre_artist_indices().len(),
        _ => app.genre_track_ids().len(),
    };
    let viewport = list_viewport(total, app.selected, rows[1].height);
    let first = viewport.start;
    let items: Vec<ListItem<'_>> = match app.genre_tab {
        0 => viewport
            .clone()
            .filter_map(|position| app.genre_album_indices().get(position).copied())
            .map(|index| {
                let album = &app.albums[index];
                ListItem::new(format!(
                    "{}  {} — {}",
                    icons::display("󰀥", "A"),
                    album.title,
                    artist_label(&album.artist)
                ))
            })
            .collect(),
        1 => viewport
            .clone()
            .filter_map(|position| app.genre_artist_indices().get(position).copied())
            .map(|index| {
                ListItem::new(format!(
                    "{}  {}",
                    icons::display("󰠃", "@"),
                    artist_label(&app.artists[index].name)
                ))
            })
            .collect(),
        _ => viewport
            .filter_map(|position| app.genre_track_ids().get(position))
            .filter_map(|id| app.track_index.get(id).map(|index| &app.tracks[*index]))
            .map(|track| {
                ListItem::new(format!(
                    "{}  {} — {}",
                    icons::display("󰎆", "T"),
                    track.title,
                    artist_label(&track.artist)
                ))
            })
            .collect(),
    };
    let mut state = ListState::default().with_selected(Some(app.selected.saturating_sub(first)));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection),
        ),
        rows[1],
        &mut state,
    );
}

fn draw_smart_playlists(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let items = app
        .smart_playlists
        .iter()
        .map(|playlist| {
            ListItem::new(format!(
                "{}  {:<32} {}",
                icons::display("󰘬", "S"),
                playlist.display_name(),
                t!(
                    "smart.summary",
                    tracks = app.evaluate_smart(playlist).len(),
                    rules = playlist.rules.len()
                )
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection),
        ),
        area,
        &mut state,
    );
}

fn draw_settings(frame: &mut Frame<'_>, area: Rect, app: &App) {
    // One source of order: the row carries its own label and knows how to
    // render its own value.
    let items = SETTINGS
        .iter()
        .map(|row| {
            ListItem::new(format!(
                "{:<34}  {}",
                app.setting_label(row),
                app.setting_value(row.id)
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection),
        ),
        area,
        &mut state,
    );
}

fn draw_lyrics(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(lyrics) = app.lyrics.as_ref().filter(|lyrics| !lyrics.is_empty()) else {
        frame.render_widget(
            Paragraph::new(t!("empty.lyrics"))
                .style(Style::default().fg(app.theme.muted))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };

    let current = lyrics.line_at(app.playback.position_ms);
    let lines: Vec<Line> = lyrics
        .lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let style = if Some(index) == current {
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.foreground)
            };
            Line::from(Span::styled(format!("  {}", line.text), style))
        })
        .collect();

    // Keep the current line roughly in the middle rather than letting it run
    // off the bottom of a long song.
    let height = area.height as usize;
    let anchor = current.unwrap_or(0);
    let start = anchor
        .saturating_sub(height / 2)
        .min(lines.len().saturating_sub(height.max(1)));
    let window: Vec<Line> = lines.into_iter().skip(start).collect();

    frame.render_widget(Paragraph::new(window).wrap(Wrap { trim: false }), area);
}

fn draw_help(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let heading = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default().fg(app.theme.foreground);
    let muted = Style::default().fg(app.theme.muted);

    let mut lines = Vec::new();
    for (title, entries) in keys::help_sections(&app.bindings) {
        if entries.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(t!(title), heading)));
        for entry in entries {
            let mut spans = vec![
                Span::styled(format!("  {:<18}", entry.keys), key_style),
                Span::raw(t!(entry.description).to_owned()),
            ];
            if let Some(scope) = entry.scope {
                spans.push(Span::styled(format!("  ({})", t!(scope)), muted));
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::default());
    }
    for (title, body) in EXTRA_HELP {
        lines.push(Line::from(Span::styled(t!(title), heading)));
        lines.push(Line::from(Span::raw(format!("  {}", t!(body)))));
        lines.push(Line::default());
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_albums(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let album_count = app.visible_album_len();
    if album_count == 0 {
        let empty = if app.view == View::ArtistDetail {
            Paragraph::new(t!("empty.artist"))
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.muted))
        } else {
            empty_library(app.theme)
        };
        frame.render_widget(empty, area);
        return;
    }
    let columns = (area.width / 24).max(1) as usize;
    app.album_columns = columns;
    let card_width = area.width / columns as u16;
    let card_height = area.height.clamp(1, 7);
    let start_row = app.selected / columns;
    let visible_rows = (area.height / card_height).max(1) as usize;
    let first_row = start_row.saturating_sub(visible_rows.saturating_sub(1));
    let first = first_row * columns;
    let last = (first + visible_rows * columns).min(album_count);
    let desired_covers = (first..last)
        .filter_map(|position| app.visible_album_index_at(position))
        .filter_map(|index| app.albums[index].cover_path.clone())
        .collect::<BTreeSet<_>>();
    let prefetch_first = first.saturating_sub(columns);
    let prefetch_last = (last + columns).min(album_count);
    let prefetch_covers = (prefetch_first..prefetch_last)
        .filter_map(|position| app.visible_album_index_at(position))
        .filter_map(|index| app.albums[index].cover_path.clone())
        .collect::<BTreeSet<_>>();
    for path in &prefetch_covers {
        if !app.covers.grid.contains_key(path) {
            app.request_cover_decode(path.clone(), 256);
        }
    }
    for path in &desired_covers {
        app.covers.grid_order.retain(|cached| cached != path);
        app.covers.grid_order.push_back(path.clone());
    }
    let cover_capacity = 64usize.max(desired_covers.len());
    while app.covers.grid.len() > cover_capacity {
        let Some(oldest) = app.covers.grid_order.pop_front() else {
            break;
        };
        if desired_covers.contains(&oldest) {
            app.covers.grid_order.push_back(oldest);
            continue;
        }
        app.covers.grid.remove(&oldest);
    }
    for position in first..last {
        let Some(index) = app.visible_album_index_at(position) else {
            continue;
        };
        let (title, artist, track_count, available_tracks, cover_path) = {
            let album = &app.albums[index];
            (
                album.title.clone(),
                artist_label(&album.artist).into_owned(),
                album.track_ids.len(),
                album.available_tracks,
                album.cover_path.clone(),
            )
        };
        let row = position / columns - first_row;
        let col = position % columns;
        let rect = Rect::new(
            area.x + col as u16 * card_width,
            area.y + row as u16 * card_height,
            card_width,
            card_height,
        )
        .intersection(area);
        let selected = position == app.selected;
        let style = if selected {
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection)
        } else {
            Style::default().fg(app.theme.foreground)
        };
        let unavailable = if available_tracks == 0 {
            t!("label.offline")
        } else {
            ""
        };
        let release_kind = if app.view == View::ArtistDetail && track_count == 1 {
            t!("label.single", suffix = unavailable)
        } else {
            t!(
                "label.track_count",
                count = track_count,
                suffix = unavailable
            )
        };
        let card = Block::default()
            .borders(album_card_borders(card_height))
            .border_style(Style::default().fg(app.theme.border));
        let inner = card.inner(rect);
        frame.render_widget(card, rect);
        let rows = album_card_rows(inner);
        let cover_area = rows[0];
        // Noted whether or not the art has finished decoding, so one arriving
        // does not count as the covers having moved.
        note_cover_placement(&mut app.covers.pending_signature, "grid", cover_area);
        if let Some(path) = cover_path
            && let Some(protocol) = app.covers.grid.get_mut(&path)
        {
            clear_cover_area(frame, cover_area, app.theme);
            frame.render_stateful_widget(StatefulImage::new(), cover_area, protocol);
        } else {
            frame.render_widget(
                Paragraph::new(icons::display("󰀥", "A"))
                    .alignment(Alignment::Center)
                    .style(
                        Style::default()
                            .fg(if selected {
                                app.theme.background
                            } else {
                                app.theme.muted
                            })
                            // The theme's own background rather than Color::Reset:
                            // resetting would punch the terminal's background
                            // through a themed interface, which is a visible dark
                            // patch under a light palette.
                            .bg(if selected {
                                app.theme.accent
                            } else {
                                app.theme.background
                            }),
                    ),
                rows[0],
            );
        }
        let text = vec![
            Line::from(Span::styled(title, style.add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(artist, style)),
            Line::from(Span::styled(
                release_kind,
                if selected {
                    style
                } else {
                    Style::default().fg(app.theme.muted)
                },
            )),
        ];
        frame.render_widget(Paragraph::new(text), rows[1]);
    }
}

fn draw_artists(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let viewport = list_viewport(app.artists.len(), app.selected, area.height);
    let first = viewport.start;
    let items = app.artists[viewport]
        .iter()
        .map(|artist| {
            ListItem::new(format!(
                "{}  {:<32}  {}",
                icons::display("󰠃", "@"),
                artist_label(&artist.name),
                t!(
                    "label.artist_summary",
                    albums = artist.album_count,
                    tracks = artist.track_ids.len()
                )
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected.saturating_sub(first)));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection)
                .add_modifier(Modifier::BOLD),
        ),
        area,
        &mut state,
    );
}

fn draw_playlists(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if app.playlists.is_empty() {
        frame.render_widget(
            Paragraph::new("No hay playlists. Pulsa c para crear una.")
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.muted)),
            area,
        );
        return;
    }
    let viewport = list_viewport(app.playlists.len(), app.selected, area.height);
    let first = viewport.start;
    let items = app.playlists[viewport]
        .iter()
        .map(|playlist| {
            ListItem::new(format!(
                "{}  {:<36}  {}",
                icons::display("󰲸", "P"),
                playlist.name,
                t!("label.playlist_summary", tracks = playlist.track_ids.len())
            ))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_selected(Some(app.selected.saturating_sub(first)));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.selection)
                .add_modifier(Modifier::BOLD),
        ),
        area,
        &mut state,
    );
}

pub(super) fn track_viewport(
    total: usize,
    selected: usize,
    area_height: u16,
) -> std::ops::Range<usize> {
    let visible_rows = area_height.saturating_sub(1) as usize;
    if total == 0 || visible_rows == 0 {
        return 0..0;
    }
    let selected = selected.min(total - 1);
    let first = selected.saturating_sub(visible_rows - 1);
    first..(first + visible_rows).min(total)
}

fn draw_tracks(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let total = app.track_view_len();
    if total == 0 {
        let message = if app.view == View::Search {
            t!("empty.search")
        } else {
            t!("empty.tracks")
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.muted)),
            area,
        );
        return;
    }

    let viewport = track_viewport(total, app.selected, area.height);
    let first = viewport.start;
    let rows = viewport.filter_map(|position| {
        let index = app.track_index_at_view_position(position)?;
        let track = &app.tracks[index];
        let marker = if app
            .current_track()
            .is_some_and(|current| current.id == track.id)
        {
            "▶"
        } else if track.favorite {
            "♥"
        } else {
            " "
        };
        Some(
            Row::new(vec![
                Cell::from(format!(
                    "{marker} {:02}",
                    if app.view == View::Queue {
                        position + 1
                    } else {
                        track.track_number as usize
                    }
                )),
                Cell::from(track.title.as_str()),
                Cell::from(artist_label(&track.artist)),
                Cell::from(track.album.as_str()),
                Cell::from(format_duration(track.duration_ms)),
            ])
            .style(if track.available {
                Style::default()
            } else {
                Style::default().fg(app.theme.border)
            }),
        )
    });
    let widths = [
        Constraint::Length(5),
        Constraint::Percentage(32),
        Constraint::Percentage(24),
        Constraint::Percentage(30),
        Constraint::Length(6),
    ];
    let selected = app.selected.saturating_sub(first);
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        Table::new(rows, widths)
            .header(
                Row::new([
                    t!("table.number"),
                    t!("table.title"),
                    t!("table.artist"),
                    t!("table.album"),
                    t!("table.time"),
                ])
                .style(
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .row_highlight_style(
                Style::default()
                    .fg(app.theme.foreground)
                    .bg(app.theme.selection)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(""),
        area,
        &mut state,
    );
}

fn draw_details(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = Block::default()
        .title(t!("panel.cover"))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(7)]).split(inner);
    let cover_area = parts[0];
    note_cover_placement(&mut app.covers.pending_signature, "panel", cover_area);
    if let Some(cover) = app.covers.current.as_mut() {
        // Clear before drawing: with the kitty image protocol the previous
        // cover leaves a band of itself behind unless the cells are wiped
        // first.
        clear_cover_area(frame, cover_area, app.theme);
        frame.render_stateful_widget(StatefulImage::new(), cover_area, &mut cover.protocol);
    } else {
        frame.render_widget(
            Paragraph::new(format!("\n\n{}\nSin portada", icons::display("󰀥", "A")))
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.muted)),
            parts[0],
        );
    }
    if let Some(track) = app.detail_track() {
        let lines = vec![
            Line::from(Span::styled(
                track.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                artist_label(&track.artist).into_owned(),
                Style::default().fg(app.theme.accent),
            )),
            Line::from(Span::styled(
                track.album.clone(),
                Style::default().fg(app.theme.muted),
            )),
            Line::from(format!(
                "{} · {}",
                track.genre,
                track.year.map(|v| v.to_string()).unwrap_or_default()
            )),
        ];
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), parts[1]);
    }
}

fn draw_player(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(app.theme.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Los dos lados miden lo mismo para que los controles y la barra de progreso
    // queden centrados de verdad en la ventana. Con anchos distintos (34/44/22)
    // el bloque central se iba a la derecha la mitad de la diferencia.
    let lado = (inner.width / 4).min(34);
    let columns = Layout::horizontal([
        Constraint::Length(lado),
        Constraint::Min(0),
        Constraint::Length(lado),
    ])
    .split(inner);
    let (title, subtitle) = app
        .presented_track()
        .map(|t| (t.title.as_str(), artist_label(&t.artist)))
        .unwrap_or((t!("empty.nothing_playing"), t!("empty.press_enter").into()));
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(subtitle, Style::default().fg(app.theme.muted))),
        ]),
        columns[0],
    );
    let presented = app.presented_playback();
    let icon = match app.playback.status {
        PlaybackStatus::Playing => icons::display("󰏤", "||"),
        PlaybackStatus::Paused => icons::display("󰐊", ">"),
        PlaybackStatus::Stopped => icons::display("󰓛", "[]"),
    };
    let mut label = format!(
        "{}   {icon}   {}    {} / {}",
        icons::display("󰒮", "<"),
        icons::display("󰒭", ">"),
        format_duration(presented.position_ms),
        format_duration(presented.duration_ms)
    );
    if let Some(mix) = &app.audible_mix {
        label.push_str(&format!(" · MIX {:.0}%", mix.progress * 100.0));
    }
    let progress = if presented.duration_ms > 0 {
        presented.position_ms as f64 / presented.duration_ms as f64
    } else {
        0.0
    };
    let progress_rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(columns[1]);
    frame.render_widget(
        Paragraph::new(label).alignment(Alignment::Center),
        progress_rows[0],
    );
    frame.render_widget(
        LineGauge::default()
            .ratio(progress.clamp(0.0, 1.0))
            .label("")
            .filled_symbol("━")
            .unfilled_symbol("━")
            .filled_style(Style::default().fg(app.theme.accent))
            .unfilled_style(Style::default().fg(app.theme.border)),
        progress_rows[1],
    );
    let repeat = match app.repeat {
        RepeatMode::Off => t!("label.repeat_off"),
        RepeatMode::Track => t!("label.repeat_track"),
        RepeatMode::Queue => t!("label.repeat_queue"),
    };
    let shuffle = if app.shuffle && !app.playback_context.is_album() {
        icons::display("󰒟", "S")
    } else {
        icons::display("󰒞", "-")
    };
    let modes = if columns[2].width < 24 {
        format!("{shuffle} · {} {repeat}", icons::display("󰑖", "R"))
    } else {
        t!("label.shuffle_repeat", shuffle = shuffle, repeat = repeat)
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n{} {:>3}% {}",
            modes,
            icons::display("󰕾", "V"),
            (app.playback.volume * 100.0).round() as u8,
            // The decibels as well as the percentage: the control moves in
            // decibels now, and the number that says how loud it is should
            // be the one that means something.
            match app.config.volume_db(app.playback.volume) {
                Some(db) => t!("label.decibels", value = format!("{db:.0}")),
                None => t!("label.silent").to_owned(),
            }
        ))
        .alignment(Alignment::Right),
        columns[2],
    );
}

fn draw_modal(frame: &mut Frame<'_>, app: &App) {
    let width = frame.area().width.min(76);
    let height = match &app.input {
        Some(InputMode::ChoosePlaylist { .. }) => (app.playlists.len() as u16 + 4).min(18),
        Some(InputMode::LoadQueue { .. }) => (app.saved_queues.len() as u16 + 4).min(18),
        Some(InputMode::Context { .. }) => CONTEXT_ACTIONS.len() as u16 + 2,
        Some(InputMode::SmartEditor { playlist, .. }) => (playlist.rules.len() as u16 + 7).min(22),
        _ => 5,
    };
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    match &app.input {
        Some(InputMode::Search) => frame.render_widget(
            Paragraph::new(format!("> {}_", app.input_buffer)).block(
                Block::default()
                    .title(t!("panel.search"))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.accent)),
            ),
            area,
        ),
        Some(InputMode::NewPlaylist) => frame.render_widget(
            Paragraph::new(format!("> {}_", app.input_buffer)).block(
                Block::default()
                    .title(t!("panel.new_playlist"))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.accent)),
            ),
            area,
        ),
        Some(InputMode::SaveQueue) => frame.render_widget(
            Paragraph::new(format!("> {}_", app.input_buffer)).block(
                Block::default()
                    .title(t!("panel.save_queue"))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.accent)),
            ),
            area,
        ),
        Some(InputMode::ChoosePlaylist { selected, .. }) => {
            let items = app
                .playlists
                .iter()
                .map(|p| ListItem::new(p.name.clone()))
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .title(t!("panel.add_to_playlist"))
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(app.theme.accent)),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(app.theme.foreground)
                            .bg(app.theme.selection),
                    ),
                area,
                &mut state,
            );
        }
        Some(InputMode::LoadQueue { selected }) => {
            let items = app
                .saved_queues
                .iter()
                .map(|queue| {
                    ListItem::new(format!(
                        "{} · {}",
                        queue.name,
                        t!("label.playlist_summary", tracks = queue.track_ids.len())
                    ))
                })
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .title(t!("panel.load_queue"))
                            .borders(Borders::ALL),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(app.theme.foreground)
                            .bg(app.theme.selection),
                    ),
                area,
                &mut state,
            );
        }
        Some(InputMode::ConfirmClearQueue) => frame.render_widget(
            Paragraph::new(t!("prompt.clear_queue"))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title(t!("panel.confirm"))
                        .borders(Borders::ALL),
                ),
            area,
        ),
        Some(InputMode::Context { selected }) => {
            let items = CONTEXT_ACTIONS
                .iter()
                .map(|(_, key)| ListItem::new(t!(*key)))
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .title(t!("panel.actions"))
                            .borders(Borders::ALL),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(app.theme.foreground)
                            .bg(app.theme.selection),
                    ),
                area,
                &mut state,
            );
        }
        Some(InputMode::SmartEditor { playlist, selected }) => {
            let mut items = playlist
                .rules
                .iter()
                .map(|rule| {
                    ListItem::new(format!(
                        "{} {} {}",
                        rule.field,
                        rule.operator,
                        display_rule_value(rule)
                    ))
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                items.push(ListItem::new(t!("empty.rules")));
            }
            items.push(ListItem::new(t!(
                "smart.match",
                mode = t!(match playlist.match_mode {
                    crate::model::SmartMatch::All => "smart.match_all",
                    crate::model::SmartMatch::Any => "smart.match_any",
                })
            )));
            items.push(ListItem::new(format!(
                "Orden: {} {}",
                playlist.sort_field,
                if playlist.descending { "↓" } else { "↑" }
            )));
            items.push(ListItem::new(t!(
                "smart.limit",
                value = playlist.limit.map_or_else(
                    || t!("smart.no_limit").to_owned(),
                    |value| value.to_string()
                )
            )));
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .title(t!("panel.editor"))
                            .borders(Borders::ALL),
                    )
                    .highlight_style(
                        Style::default()
                            .fg(app.theme.foreground)
                            .bg(app.theme.selection),
                    ),
                area,
                &mut state,
            );
        }
        Some(InputMode::SmartValue { .. }) => frame.render_widget(
            Paragraph::new(format!("> {}_", app.input_buffer)).block(
                Block::default()
                    .title(t!("panel.rule_value"))
                    .borders(Borders::ALL),
            ),
            area,
        ),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(slots: &[(&str, Rect)]) -> u64 {
        let mut signature = 0;
        for (key, area) in slots {
            note_cover_placement(&mut signature, key, *area);
        }
        signature
    }

    fn slot(x: u16, y: u16) -> Rect {
        Rect::new(x, y, 20, 10)
    }

    #[test]
    fn cover_clear_restores_light_palette_instead_of_terminal_black() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(12, 5)).unwrap();
        terminal
            .draw(|frame| {
                let region = Rect::new(2, 1, 8, 3);
                frame.render_widget(
                    Block::default().style(Style::default().bg(Color::Black)),
                    region,
                );
                clear_cover_area(frame, region, UiTheme::light());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 1..4 {
            for x in 2..10 {
                assert_eq!(buffer[(x, y)].bg, UiTheme::light().background);
            }
        }
    }

    #[test]
    fn cover_clear_keeps_dark_palette_dark() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(10, 3)).unwrap();
        terminal
            .draw(|frame| clear_cover_area(frame, frame.area(), UiTheme::dark()))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(5, 1)].bg, Color::Black);
    }

    #[test]
    fn header_keeps_lowercase_name_and_empty_space() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(80, 3)).unwrap();
        terminal
            .draw(|frame| draw_header(frame, frame.area(), UiTheme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let line = |y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert_eq!(line(0).trim(), "muscli");
        assert!(line(1).trim().is_empty());
    }

    #[test]
    fn outer_spacing_adapts_to_small_terminals() {
        assert_eq!(
            content_area(Rect::new(0, 0, 100, 30)),
            Rect::new(1, 0, 98, 30)
        );
        assert_eq!(
            content_area(Rect::new(0, 0, 40, 12)),
            Rect::new(0, 0, 40, 12)
        );
        for width in 0..120 {
            for height in 0..40 {
                let area = Rect::new(3, 5, width, height);
                let inner = content_area(area);
                assert_eq!(inner.intersection(area), inner);
            }
        }
    }

    #[test]
    fn panels_fit_without_overlap_at_terminal_sizes() {
        for (width, height) in [(60, 12), (80, 24), (94, 20), (120, 30), (180, 52)] {
            let area = content_area(Rect::new(0, 0, width, height));
            let rows = interface_rows(area);
            assert_eq!(rows[2].height, 3, "player at {width}x{height}");
            assert_eq!(rows[1].height, height - 7);
            assert_eq!(rows[0].y, 0);
            assert_eq!(rows[3].bottom(), height);
            for pair in rows.windows(2) {
                assert_eq!(pair[0].bottom(), pair[1].y);
            }
            let panels = body_columns(rows[1]);
            assert_eq!(panels.len(), if area.width >= 92 { 3 } else { 2 });
            for panel in panels.iter() {
                assert_eq!(panel.intersection(rows[1]), *panel);
            }
            for pair in panels.windows(2) {
                assert_eq!(pair[0].right(), pair[1].x);
            }
        }
    }

    #[test]
    fn album_metadata_fits_below_the_cover_in_short_cards() {
        use ratatui::{Terminal, backend::TestBackend};

        for height in 3..=7 {
            let mut terminal = Terminal::new(TestBackend::new(24, height)).unwrap();
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    let card = Block::default().borders(album_card_borders(height));
                    let rows = album_card_rows(card.inner(area));
                    frame.render_widget(card, area);
                    frame.render_widget(Paragraph::new("Album\nArtist\n12 tracks"), rows[1]);
                    assert_eq!(rows[1].height, 3);
                    assert_eq!(rows[0].bottom(), rows[1].y);
                    assert_eq!(rows[1].bottom(), area.bottom() - u16::from(height >= 4));
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for (row, text) in ["Album", "Artist", "12 tracks"].iter().enumerate() {
                for (column, symbol) in text.chars().enumerate() {
                    assert_eq!(
                        buffer[(
                            column as u16,
                            height - 3 - u16::from(height >= 4) + row as u16
                        )]
                            .symbol(),
                        symbol.to_string()
                    );
                }
            }
        }
    }

    #[test]
    fn reclaimed_rows_allow_an_extra_album_row() {
        let rows = interface_rows(content_area(Rect::new(0, 0, 120, 30)));
        let library = Block::default()
            .borders(Borders::ALL)
            .inner(body_columns(rows[1])[1]);
        assert_eq!(library.height / 7, 3);
        // Previously: two outer rows, three header, four player, one status,
        // and two library borders left only eighteen rows for cards.
        assert_eq!((30 - 2 - 3 - 4 - 1 - 2) / 7, 2);
    }

    #[test]
    fn the_same_slots_give_the_same_signature() {
        // What makes the signature useful: art arriving, or a different album
        // landing in a slot that was already there, must not read as the
        // covers having moved. A full repaint on every decoded thumbnail is
        // what made the album grid flicker.
        let before = signature(&[("grid", slot(0, 0)), ("grid", slot(22, 0))]);
        let after = signature(&[("grid", slot(0, 0)), ("grid", slot(22, 0))]);
        assert_eq!(before, after);
    }

    #[test]
    fn a_slot_that_moved_gives_a_different_signature() {
        let before = signature(&[("grid", slot(0, 0)), ("grid", slot(22, 0))]);
        let moved = signature(&[("grid", slot(0, 0)), ("grid", slot(22, 1))]);
        assert_ne!(before, moved, "a cover moved down a row went unnoticed");
    }

    #[test]
    fn a_slot_appearing_or_leaving_gives_a_different_signature() {
        let two = signature(&[("grid", slot(0, 0)), ("grid", slot(22, 0))]);
        let three = signature(&[
            ("grid", slot(0, 0)),
            ("grid", slot(22, 0)),
            ("grid", slot(44, 0)),
        ]);
        assert_ne!(two, three, "a slot appearing went unnoticed");
    }

    #[test]
    fn the_panel_and_the_grid_do_not_cancel_out() {
        let area = slot(0, 0);
        assert_ne!(signature(&[("panel", area)]), signature(&[("grid", area)]));
    }
}
