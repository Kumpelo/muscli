//! Rendering for the terminal UI.
//!
//! This module draws `App`; it never mutates application state except for the
//! image protocols and cover-placement signature that drawing itself owns.
//!
//! `use super::*` pulls in `App`, the view enums, the shared constants and the
//! imports declared in the parent module.

use super::*;
use crate::t;

/// Mix which image is drawn, and where, into the signature.
fn note_cover_placement(signature: &mut u64, key: &str, area: Rect) {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    (area.x, area.y, area.width, area.height).hash(&mut h);
    *signature = signature.rotate_left(13) ^ h.finish();
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
    if app.compact {
        draw_compact(frame, app);
        if app.input.is_some() {
            draw_modal(frame, app);
        }
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, rows[0], app);
    let body = if area.width >= 92 {
        Layout::horizontal([
            Constraint::Length(21),
            Constraint::Min(36),
            Constraint::Length(30),
        ])
        .split(rows[1])
    } else {
        Layout::horizontal([Constraint::Length(18), Constraint::Min(28)]).split(rows[1])
    };
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
    let area = frame.area();
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, rows[0], app);
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
                track.artist
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

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let title = Line::from(vec![
        Span::styled(
            "  mus",
            Style::default()
                .fg(app.theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "cli",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let stats = t!(
        "label.header_counts",
        tracks = app.tracks.len(),
        albums = app.albums.len(),
        artists = app.artists.len()
    );
    frame.render_widget(
        Paragraph::new(title).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(app.theme.border)),
        ),
        area,
    );
    let stats_area = Rect::new(area.x, area.y + 1, area.width, 1);
    frame.render_widget(
        Paragraph::new(stats)
            .alignment(Alignment::Right)
            .style(Style::default().fg(app.theme.muted)),
        stats_area,
    );
}

fn draw_sidebar(frame: &mut Frame<'_>, area: Rect, app: &App) {
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
}

fn draw_content(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = match app.view {
        View::AlbumDetail => app
            .opened_album()
            .map(|album| t!("panel.back_hint", name = album.title))
            .unwrap_or_else(|| " Álbum ".into()),
        View::ArtistDetail => app
            .opened_artist_name()
            .map(|artist| t!("panel.artist_hint", name = artist))
            .unwrap_or_else(|| " Artista ".into()),
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
                track.artist,
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
                "󰌳  {:<36} {}",
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
                ListItem::new(format!("󰀥  {} — {}", album.title, album.artist))
            })
            .collect(),
        1 => viewport
            .clone()
            .filter_map(|position| app.genre_artist_indices().get(position).copied())
            .map(|index| ListItem::new(format!("󰠃  {}", app.artists[index].name)))
            .collect(),
        _ => viewport
            .filter_map(|position| app.genre_track_ids().get(position))
            .filter_map(|id| app.track_index.get(id).map(|index| &app.tracks[*index]))
            .map(|track| ListItem::new(format!("󰎆  {} — {}", track.title, track.artist)))
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
                "󰘬  {:<32} {}",
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
                t!(row.label),
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
    let card_height = 7u16;
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
                album.artist.clone(),
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
        );
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
            .borders(Borders::RIGHT | Borders::BOTTOM)
            .border_style(Style::default().fg(app.theme.border));
        let inner = card.inner(rect);
        frame.render_widget(card, rect);
        let rows = Layout::vertical([Constraint::Length(3), Constraint::Length(3)]).split(inner);
        if let Some(path) = cover_path
            && let Some(protocol) = app.covers.grid.get_mut(&path)
        {
            let area = rows[0].inner(Margin {
                horizontal: 1,
                vertical: 0,
            });
            note_cover_placement(
                &mut app.covers.pending_signature,
                &path.to_string_lossy(),
                area,
            );
            frame.render_widget(Clear, area); // same reason as the detail panel
            frame.render_stateful_widget(StatefulImage::new(), area, protocol);
        } else {
            frame.render_widget(
                Paragraph::new("󰀥").alignment(Alignment::Center).style(
                    Style::default()
                        .fg(if selected {
                            app.theme.background
                        } else {
                            app.theme.muted
                        })
                        .bg(if selected {
                            app.theme.accent
                        } else {
                            Color::Reset
                        }),
                ),
                rows[0],
            );
        }
        let text = vec![
            Line::from(Span::styled(
                format!(" {}", title),
                style.add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(format!(" {}", artist), style)),
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
                "󰠃  {:<32}  {}",
                artist.name,
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
                "󰲸  {:<36}  {}",
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
                Cell::from(track.artist.as_str()),
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
    let parts = Layout::vertical([
        Constraint::Length(inner.height.saturating_sub(7).max(5)),
        Constraint::Length(7),
    ])
    .split(inner);
    if let Some(cover) = app.covers.current.as_mut() {
        let area = parts[0].inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        // Clear before drawing: with the kitty image protocol the previous
        // cover leaves a band of itself behind unless the cells are wiped
        // first.
        note_cover_placement(&mut app.covers.pending_signature, "panel", area);
        frame.render_widget(Clear, area);
        frame.render_stateful_widget(StatefulImage::new(), area, &mut cover.protocol);
    } else {
        frame.render_widget(
            Paragraph::new("\n\n󰀥\nSin portada")
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
                track.artist.clone(),
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
    let lado = (inner.width / 4).clamp(16, 34);
    let columns = Layout::horizontal([
        Constraint::Length(lado),
        Constraint::Min(0),
        Constraint::Length(lado),
    ])
    .split(inner);
    let (title, subtitle) = app
        .current_track()
        .map(|t| (t.title.as_str(), t.artist.as_str()))
        .unwrap_or((t!("empty.nothing_playing"), t!("empty.press_enter")));
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
    let icon = match app.playback.status {
        PlaybackStatus::Playing => "󰏤",
        PlaybackStatus::Paused => "󰐊",
        PlaybackStatus::Stopped => "󰓛",
    };
    let label = format!(
        "󰒮   {icon}   󰒭       {} / {}",
        format_duration(app.playback.position_ms),
        format_duration(app.playback.duration_ms)
    );
    let progress = if app.playback.duration_ms > 0 {
        app.playback.position_ms as f64 / app.playback.duration_ms as f64
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
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n󰕾 {:>3}%",
            t!(
                "label.shuffle_repeat",
                shuffle = if app.shuffle { "󰒟" } else { "󰒞" },
                repeat = repeat
            ),
            (app.playback.volume * 100.0) as u8
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
