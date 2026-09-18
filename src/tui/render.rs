/// Mezcla en la firma qué imagen se dibuja y en qué rectángulo.
fn anotar_portada(sig: &mut u64, clave: &str, area: Rect) {
    let mut h = DefaultHasher::new();
    clave.hash(&mut h);
    (area.x, area.y, area.width, area.height).hash(&mut h);
    *sig = sig.rotate_left(13) ^ h.finish();
}

fn draw(frame: &mut Frame<'_>, app: &mut App) {
    app.cover_sig_now = 0;
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
                .title(" Cola · m modo completo ")
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
    let stats = format!(
        "{} canciones  ·  {} álbumes  ·  {} artistas  ",
        app.tracks.len(),
        app.albums.len(),
        app.artists.len()
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
        View::AlbumDetail if app.album_parent_view == View::ArtistDetail => View::Artists,
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
            .map(|album| format!(" {} · Esc para volver ", album.title))
            .unwrap_or_else(|| " Álbum ".into()),
        View::ArtistDetail => app
            .opened_artist_name
            .as_deref()
            .map(|artist| format!(" {} · álbumes y singles · Esc para volver ", artist))
            .unwrap_or_else(|| " Artista ".into()),
        View::GenreDetail => app
            .opened_genre_name
            .as_deref()
            .map(|genre| format!(" {genre} · Tab cambia sección · Esc para volver "))
            .unwrap_or_else(|| " Género ".into()),
        View::SmartPlaylistDetail => app
            .opened_smart_playlist
            .and_then(|id| {
                app.smart_playlists
                    .iter()
                    .find(|playlist| playlist.id == id)
            })
            .map(|playlist| format!(" {} · Esc para volver ", playlist.name))
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
            Paragraph::new("Reproduce música para llenar Seguir escuchando e Historial.")
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

fn draw_genres(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let items = app
        .genres
        .iter()
        .map(|genre| {
            ListItem::new(format!(
                "󰌳  {:<36} {} canciones",
                genre.name,
                genre.track_ids.len()
            ))
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

fn draw_genre_detail(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let tabs = ["Álbumes", "Artistas", "Canciones"];
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
    let items: Vec<ListItem<'_>> = match app.genre_tab {
        0 => app
            .genre_album_indices()
            .iter()
            .copied()
            .map(|index| {
                let album = &app.albums[index];
                ListItem::new(format!("󰀥  {} — {}", album.title, album.artist))
            })
            .collect(),
        1 => app
            .genre_artist_indices()
            .iter()
            .copied()
            .map(|index| ListItem::new(format!("󰠃  {}", app.artists[index].name)))
            .collect(),
        _ => app
            .genre_track_ids()
            .iter()
            .filter_map(|id| app.track_index.get(id).map(|index| &app.tracks[*index]))
            .map(|track| ListItem::new(format!("󰎆  {} — {}", track.title, track.artist)))
            .collect(),
    };
    let mut state = ListState::default().with_selected(Some(app.selected));
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
                "󰘬  {:<32} {} canciones · {} reglas",
                playlist.name,
                app.evaluate_smart(playlist).len(),
                playlist.rules.len()
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
    let values = [
        if app.config.replaygain_enabled {
            "on".into()
        } else {
            "off".into()
        },
        format!("{:?}", app.config.replaygain_mode),
        format!("{:.0} LUFS", app.config.replaygain_target_lufs),
        if app.config.resume_enabled {
            "on".into()
        } else {
            "off".into()
        },
        if app.config.history_enabled {
            "on".into()
        } else {
            "off".into()
        },
        if app.config.compact_default {
            "on".into()
        } else {
            "off".into()
        },
        if app.config.show_covers {
            "on".into()
        } else {
            "off".into()
        },
        format!("{}%", app.config.volume_step),
        if app.config.auto_discover_removable {
            "on".into()
        } else {
            "off".into()
        },
        format!("{} MiB", app.config.cover_cache_mb),
        if app.config.discord_enabled {
            "on".into()
        } else {
            "off".into()
        },
        if app.scan_running {
            "en curso".into()
        } else {
            "Enter/Space".into()
        },
        app.gain_progress
            .map(|(done, total)| format!("{done}/{total}"))
            .unwrap_or_else(|| "Enter/Space".into()),
    ];
    let items = SETTINGS
        .iter()
        .zip(values)
        .map(|(name, value)| ListItem::new(format!("{name:<34}  {value}")))
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

fn draw_help(frame: &mut Frame<'_>, area: Rect, app: &App) {
    frame.render_widget(
        Paragraph::new(
            HELP_SECTIONS
                .into_iter()
                .flat_map(|(section, keys)| {
                    [
                        Line::from(Span::styled(
                            section,
                            Style::default()
                                .fg(app.theme.accent)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(keys),
                        Line::default(),
                    ]
                })
                .collect::<Vec<_>>(),
        )
        .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_albums(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let album_count = app.visible_album_len();
    if album_count == 0 {
        let empty = if app.view == View::ArtistDetail {
            Paragraph::new("No encontré álbumes ni singles para este artista.")
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
    for path in &desired_covers {
        app.album_cover_order.retain(|cached| cached != path);
        app.album_cover_order.push_back(path.clone());
        if !app.album_covers.contains_key(path) {
            app.request_cover_decode(path.clone(), 256);
        }
    }
    let cover_capacity = 64usize.max(desired_covers.len());
    while app.album_covers.len() > cover_capacity {
        let Some(oldest) = app.album_cover_order.pop_front() else {
            break;
        };
        if desired_covers.contains(&oldest) {
            app.album_cover_order.push_back(oldest);
            continue;
        }
        app.album_covers.remove(&oldest);
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
            "  [offline]"
        } else {
            ""
        };
        let release_kind = if app.view == View::ArtistDetail && track_count == 1 {
            format!(" Single{unavailable}")
        } else {
            format!(" {track_count} pistas{unavailable}")
        };
        let card = Block::default()
            .borders(Borders::RIGHT | Borders::BOTTOM)
            .border_style(Style::default().fg(app.theme.border));
        let inner = card.inner(rect);
        frame.render_widget(card, rect);
        let rows = Layout::vertical([Constraint::Length(3), Constraint::Length(3)]).split(inner);
        if let Some(path) = cover_path
            && let Some(protocol) = app.album_covers.get_mut(&path)
        {
            let zona = rows[0].inner(Margin {
                horizontal: 1,
                vertical: 0,
            });
            anotar_portada(&mut app.cover_sig_now, &path.to_string_lossy(), zona);
            frame.render_widget(Clear, zona); // mismo motivo que en el panel
            frame.render_stateful_widget(StatefulImage::new(), zona, protocol);
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
    let items = app
        .artists
        .iter()
        .map(|artist| {
            ListItem::new(format!(
                "󰠃  {:<32}  {} álbumes · {} canciones",
                artist.name,
                artist.album_count,
                artist.track_ids.len()
            ))
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
    let items = app
        .playlists
        .iter()
        .map(|playlist| {
            ListItem::new(format!(
                "󰲸  {:<36}  {} canciones",
                playlist.name,
                playlist.track_ids.len()
            ))
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

fn track_viewport(total: usize, selected: usize, area_height: u16) -> std::ops::Range<usize> {
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
            "Escribe / para buscar por canción, artista o álbum."
        } else {
            "No hay canciones aquí."
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
                Row::new(["#", "Título", "Artista", "Álbum", "Tiempo"]).style(
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
        .title(" Portada ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let parts = Layout::vertical([
        Constraint::Length(inner.height.saturating_sub(7).max(5)),
        Constraint::Length(7),
    ])
    .split(inner);
    if let Some(cover) = app.cover.as_mut() {
        let zona = parts[0].inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        // Limpiar la zona antes de dibujar: con el protocolo de imágenes de kitty,
        // la portada anterior deja restos (una franja de la otra imagen) si no se
        // borra primero.
        anotar_portada(&mut app.cover_sig_now, "panel", zona);
        frame.render_widget(Clear, zona);
        frame.render_stateful_widget(StatefulImage::new(), zona, &mut cover.protocol);
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
        .unwrap_or(("Nada reproduciéndose", "Enter para reproducir"));
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
        RepeatMode::Off => "off",
        RepeatMode::Track => "una",
        RepeatMode::Queue => "cola",
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{} aleatorio · repetir {}\n󰕾 {:>3}%",
            if app.shuffle { "󰒟" } else { "󰒞" },
            repeat,
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
                    .title(" Buscar ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.accent)),
            ),
            area,
        ),
        Some(InputMode::NewPlaylist) => frame.render_widget(
            Paragraph::new(format!("> {}_", app.input_buffer)).block(
                Block::default()
                    .title(" Nueva playlist ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.accent)),
            ),
            area,
        ),
        Some(InputMode::SaveQueue) => frame.render_widget(
            Paragraph::new(format!("> {}_", app.input_buffer)).block(
                Block::default()
                    .title(" Guardar cola ")
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
                            .title(" Añadir a playlist ")
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
                        "{} · {} canciones",
                        queue.name,
                        queue.track_ids.len()
                    ))
                })
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .title(" Cargar cola ")
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
            Paragraph::new("¿Vaciar toda la cola?  Enter/s: sí · n/Esc: no")
                .alignment(Alignment::Center)
                .block(Block::default().title(" Confirmar ").borders(Borders::ALL)),
            area,
        ),
        Some(InputMode::Context { selected }) => {
            let items = CONTEXT_ACTIONS
                .iter()
                .map(|action| ListItem::new(*action))
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(Block::default().title(" Acciones ").borders(Borders::ALL))
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
                items.push(ListItem::new("Sin reglas · a para añadir"));
            }
            items.push(ListItem::new(format!(
                "Coincidencia: {:?}",
                playlist.match_mode
            )));
            items.push(ListItem::new(format!(
                "Orden: {} {}",
                playlist.sort_field,
                if playlist.descending { "↓" } else { "↑" }
            )));
            items.push(ListItem::new(format!(
                "Límite: {}",
                playlist
                    .limit
                    .map_or_else(|| "sin límite".into(), |value| value.to_string())
            )));
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(
                        Block::default()
                            .title(" Editor · Tab campo · Enter valor · a/d regla · m modo · Ctrl+S guardar ")
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
                    .title(" Valor de regla ")
                    .borders(Borders::ALL),
            ),
            area,
        ),
        None => {}
    }
}
