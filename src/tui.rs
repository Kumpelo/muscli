use std::{
    collections::{BTreeSet, HashMap, HashSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::terminal::SetSize;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rand::seq::SliceRandom;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, LineGauge, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Wrap,
    },
};
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};

use crate::{
    config::{Config, ReplayGainMode, all_sources},
    control::{ControlServer, RemoteCommand},
    db::{Database, group_albums, group_artists},
    discord::DiscordPresence,
    features::{Genre, evaluate_smart_playlist, fuzzy_search, group_genres},
    instance::InstanceGuard,
    library::{SourceScan, prune_cover_cache, prune_unreferenced_covers, scan_source},
    model::{
        Album, Artist, HistoryEntry, PlaybackState, PlaybackStatus, PlayerAction, PlayerEvent,
        Playlist, RepeatMode, SavedPlayback, SavedQueue, SmartPlaylist, SmartRule, Track,
        TrackStats,
    },
    mpris::MprisBridge,
    paths::AppPaths,
    player::MpvPlayer,
    replaygain::{self, GainMessage},
};

const VIEWS: [View; 13] = [
    View::Home,
    View::Albums,
    View::Artists,
    View::Genres,
    View::Tracks,
    View::Playlists,
    View::SmartPlaylists,
    View::Favorites,
    View::History,
    View::Search,
    View::Queue,
    View::Settings,
    View::Help,
];

const SETTINGS: [&str; 13] = [
    "ReplayGain",
    "Modo ReplayGain",
    "Objetivo LUFS",
    "Restaurar posiciones",
    "Historial",
    "Modo compacto por defecto",
    "Mostrar portadas",
    "Paso de volumen",
    "Autodetectar SD/USB",
    "Caché de portadas",
    "Discord Rich Presence",
    "Reescanear biblioteca",
    "Analizar ReplayGain",
];

const CONTEXT_ACTIONS: [&str; 7] = [
    "Reproducir ahora",
    "Reproducir después",
    "Añadir al final",
    "Favorito",
    "Añadir a playlist",
    "Mostrar álbum",
    "Mostrar artista",
];

const HELP_SECTIONS: [(&str, &str); 6] = [
    (
        "Navegación",
        "↑↓←→ / hjkl mover · Enter abrir/reproducir · Tab cambiar panel · Esc volver",
    ),
    (
        "Reproducción",
        "Space pausa · n/p siguiente/anterior · +/- volumen · s shuffle · r repetir · m compacto",
    ),
    (
        "Biblioteca",
        "/ buscar · f favorito · a añadir a cola · P playlist · x menú contextual",
    ),
    (
        "Cola",
        "Shift+J/K reordenar · d/Delete quitar · C limpiar · S guardar · L cargar",
    ),
    ("Ventanas", ", Settings · ? Ayuda · q salir"),
    (
        "Omarchy global",
        "Shift+Vol± volumen de muscli · Vol± volumen del sistema · Super+Shift+Alt+M compacto",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    Albums,
    AlbumDetail,
    Artists,
    ArtistDetail,
    Genres,
    GenreDetail,
    Tracks,
    Playlists,
    SmartPlaylists,
    SmartPlaylistDetail,
    Favorites,
    History,
    Search,
    Queue,
    Settings,
    Help,
}

impl View {
    fn title(self) -> &'static str {
        match self {
            Self::Home => "Inicio",
            Self::Albums => "Álbumes",
            Self::AlbumDetail => "Álbum",
            Self::Artists => "Artistas",
            Self::ArtistDetail => "Artista",
            Self::Genres => "Géneros",
            Self::GenreDetail => "Género",
            Self::Tracks => "Canciones",
            Self::Playlists => "Playlists",
            Self::SmartPlaylists => "Listas inteligentes",
            Self::SmartPlaylistDetail => "Lista inteligente",
            Self::Favorites => "Favoritos",
            Self::History => "Historial",
            Self::Search => "Buscar",
            Self::Queue => "Cola",
            Self::Settings => "Settings",
            Self::Help => "Ayuda",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Home => "󰋜",
            Self::Albums => "󰀥",
            Self::AlbumDetail => "󰀥",
            Self::Artists => "󰠃",
            Self::ArtistDetail => "󰠃",
            Self::Genres | Self::GenreDetail => "󰌳",
            Self::Tracks => "󰎆",
            Self::Playlists => "󰲸",
            Self::SmartPlaylists | Self::SmartPlaylistDetail => "󰘬",
            Self::Favorites => "󰋑",
            Self::History => "󰋚",
            Self::Search => "󰍉",
            Self::Queue => "󰕲",
            Self::Settings => "󰒓",
            Self::Help => "󰋖",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Content,
}

#[derive(Debug, Clone, PartialEq)]
enum InputMode {
    Search,
    NewPlaylist,
    ChoosePlaylist {
        track_id: String,
        selected: usize,
    },
    SaveQueue,
    LoadQueue {
        selected: usize,
    },
    ConfirmClearQueue,
    Context {
        selected: usize,
    },
    SmartEditor {
        playlist: SmartPlaylist,
        selected: usize,
    },
    SmartValue {
        playlist: SmartPlaylist,
        selected: usize,
    },
}

enum ScanMessage {
    Source(SourceScan),
    Done(BTreeSet<String>),
}

struct CoverState {
    path: PathBuf,
    protocol: StatefulProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiTheme {
    accent: Color,
    selection: Color,
    foreground: Color,
    background: Color,
    muted: Color,
    border: Color,
}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            accent: Color::Magenta,
            selection: Color::Magenta,
            foreground: Color::White,
            background: Color::Black,
            muted: Color::Gray,
            border: Color::DarkGray,
        }
    }
}

impl UiTheme {
    fn load() -> Self {
        let state_home = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            });
        let config_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".config"));
        let paths = [
            state_home.map(|root| root.join("omarchy/current/theme/colors.toml")),
            config_home.map(|root| root.join("omarchy/current/theme/colors.toml")),
        ];
        paths
            .into_iter()
            .flatten()
            .find_map(|path| Self::from_file(&path))
            .unwrap_or_default()
    }

    fn from_file(path: &Path) -> Option<Self> {
        let raw = fs::read_to_string(path).ok()?;
        let value = toml::from_str::<toml::Value>(&raw).ok()?;
        let color = |key: &str| value.get(key)?.as_str().and_then(parse_hex_color);
        let fallback = Self::default();
        let accent = color("accent").unwrap_or(fallback.accent);
        Some(Self {
            accent,
            selection: color("selection").unwrap_or(accent),
            foreground: color("foreground").unwrap_or(fallback.foreground),
            background: color("background").unwrap_or(fallback.background),
            muted: color("dark_foreground")
                .or_else(|| color("muted"))
                .unwrap_or(fallback.muted),
            border: color("muted").unwrap_or(fallback.border),
        })
    }
}

fn parse_hex_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some(Color::Rgb(
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ))
}

struct App {
    paths: AppPaths,
    config: Config,
    db: Database,
    tracks: Vec<Track>,
    track_index: HashMap<String, usize>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
    playlists: Vec<Playlist>,
    smart_playlists: Vec<SmartPlaylist>,
    saved_queues: Vec<SavedQueue>,
    stats: HashMap<String, TrackStats>,
    added_at: HashMap<String, i64>,
    history: Vec<HistoryEntry>,
    genres: Vec<Genre>,
    view: View,
    focus: Focus,
    selected: usize,
    opened_album_key: Option<String>,
    album_return_selection: usize,
    album_parent_view: View,
    opened_artist_name: Option<String>,
    artist_release_keys: Vec<String>,
    artist_return_selection: usize,
    artist_parent_view: View,
    opened_genre_name: Option<String>,
    genre_return_selection: usize,
    genre_tab: usize,
    opened_smart_playlist: Option<i64>,
    query: String,
    input: Option<InputMode>,
    input_buffer: String,
    queue: Vec<String>,
    queue_index: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
    playback: PlaybackState,
    muted_volume: Option<f64>,
    compact: bool,
    mpv: MpvPlayer,
    mpris: Option<MprisBridge>,
    discord: Option<DiscordPresence>,
    actions: Receiver<PlayerAction>,
    remote_actions: Receiver<RemoteCommand>,
    _control_server: ControlServer,
    gain_rx: Option<Receiver<GainMessage>>,
    gain_progress: Option<(usize, usize)>,
    scan_rx: Receiver<ScanMessage>,
    scan_tx: Sender<ScanMessage>,
    watch_rx: Receiver<()>,
    scan_running: bool,
    last_scan: Instant,
    status: String,
    should_quit: bool,
    dirty: bool,
    picker: Picker,
    cover: Option<CoverState>,
    // Firma de qué portadas se dibujaron y dónde. Las imágenes de kitty van
    // ancladas a celdas de texto, y ratatui solo reescribe las celdas que
    // cambian: si una portada se mueve, quedan restos de la anterior mezclados
    // con la nueva. Comparando la firma entre fotogramas sabemos cuándo hace
    // falta repintar la pantalla entera.
    cover_sig: u64,
    cover_sig_now: u64,
    album_covers: HashMap<PathBuf, StatefulProtocol>,
    album_columns: usize,
    last_mpris_signature: String,
    last_discord_signature: String,
    theme: UiTheme,
    history_id: Option<i64>,
    history_track_id: Option<String>,
    listened_this_session_ms: u64,
    pending_listen_ms: u64,
    history_counted: bool,
    last_history_tick: Instant,
    last_history_flush: Instant,
    last_playback_save: Instant,
    last_theme_check: Instant,
}

pub async fn run(paths: AppPaths, config: Config, compact: bool) -> Result<()> {
    let _guard = InstanceGuard::acquire(&paths.lock_file())?;
    let (picker, protocol_note) = match Picker::from_query_stdio() {
        Ok(picker) => {
            let note = format!("{:?} (detección automática)", picker.protocol_type());
            (picker, note)
        }
        Err(error) => (
            Picker::halfblocks(),
            format!("Halfblocks (fallback: {error})"),
        ),
    };
    let _ = fs::write(paths.image_protocol_file(), protocol_note);
    let mut terminal = ratatui::init();
    if let Err(error) = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)
    {
        ratatui::restore();
        return Err(error.into());
    }
    let result = run_inner(&mut terminal, paths, config, picker, compact).await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

async fn run_inner(
    terminal: &mut ratatui::DefaultTerminal,
    paths: AppPaths,
    config: Config,
    picker: Picker,
    compact_requested: bool,
) -> Result<()> {
    let db = Database::open(&paths.database_file())?;
    let saved = db.load_playback()?;
    let mpv = MpvPlayer::start(&paths.mpv_socket())?;
    let (control_server, remote_actions) = ControlServer::start(&paths.control_socket())?;
    let saved_volume = saved.volume.clamp(0.0, 1.0);
    mpv.set_volume(saved_volume)?;
    let (action_tx, actions) = mpsc::channel();
    let (mpris, mpris_warning) = match MprisBridge::new(action_tx).await {
        Ok(bridge) => (Some(bridge), None),
        Err(error) => (None, Some(format!("MPRIS no disponible: {error}"))),
    };
    let (scan_tx, scan_rx) = mpsc::channel();
    let (watch_tx, watch_rx) = mpsc::channel();
    start_watchers(&config, watch_tx);
    let discord = if config.discord_enabled {
        config.discord_application_id.clone().map(|application_id| {
            DiscordPresence::start(application_id, config.discord_large_image.clone())
        })
    } else {
        None
    };
    let compact = compact_requested || config.compact_default;

    let mut app = App {
        paths,
        config,
        db,
        tracks: Vec::new(),
        track_index: HashMap::new(),
        albums: Vec::new(),
        artists: Vec::new(),
        playlists: Vec::new(),
        smart_playlists: Vec::new(),
        saved_queues: Vec::new(),
        stats: HashMap::new(),
        added_at: HashMap::new(),
        history: Vec::new(),
        genres: Vec::new(),
        view: View::Home,
        focus: Focus::Content,
        selected: 0,
        opened_album_key: None,
        album_return_selection: 0,
        album_parent_view: View::Albums,
        opened_artist_name: None,
        artist_release_keys: Vec::new(),
        artist_return_selection: 0,
        artist_parent_view: View::Artists,
        opened_genre_name: None,
        genre_return_selection: 0,
        genre_tab: 0,
        opened_smart_playlist: None,
        query: String::new(),
        input: None,
        input_buffer: String::new(),
        queue: saved.queue,
        queue_index: saved.current_index,
        shuffle: saved.shuffle,
        repeat: saved.repeat,
        playback: PlaybackState {
            volume: saved_volume,
            position_ms: saved.position_ms,
            ..Default::default()
        },
        muted_volume: (saved_volume == 0.0)
            .then_some(saved.last_nonzero_volume.unwrap_or(1.0).clamp(0.01, 1.0)),
        compact,
        mpv,
        mpris,
        discord,
        actions,
        remote_actions,
        _control_server: control_server,
        gain_rx: None,
        gain_progress: None,
        scan_rx,
        scan_tx,
        watch_rx,
        scan_running: false,
        last_scan: Instant::now() - Duration::from_secs(5),
        status: mpris_warning.unwrap_or_else(|| "Cargando biblioteca…".into()),
        should_quit: false,
        dirty: true,
        picker,
        cover: None,
        cover_sig: 0,
        cover_sig_now: 0,
        album_covers: HashMap::new(),
        album_columns: 1,
        last_mpris_signature: String::new(),
        last_discord_signature: String::new(),
        theme: UiTheme::load(),
        history_id: None,
        history_track_id: None,
        listened_this_session_ms: 0,
        pending_listen_ms: 0,
        history_counted: false,
        last_history_tick: Instant::now(),
        last_history_flush: Instant::now(),
        last_playback_save: Instant::now() - Duration::from_secs(5),
        last_theme_check: Instant::now(),
    };
    app.reload_library()?;
    app.queue.retain(|id| app.track_index.contains_key(id));
    if app.queue_index.is_some_and(|i| i >= app.queue.len()) {
        app.queue_index = None;
    }
    if let Some(track) = app
        .current_track()
        .cloned()
        .filter(|t| t.available && t.path.exists())
    {
        let resume_position = if app.config.resume_enabled {
            app.stats
                .get(&track.id)
                .map(|stats| stats.resume_position_ms)
                .filter(|position| *position > 0)
                .unwrap_or(saved.position_ms)
        } else {
            0
        };
        app.load_current(resume_position)?;
        app.mpv.pause(true)?;
        app.playback.status = PlaybackStatus::Paused;
        app.status = format!("Sesión restaurada: {} — {}", track.title, track.artist);
    }
    app.start_scan();
    app.start_gain_analysis()?;

    let shutdown_rx = start_shutdown_listener()?;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let loop_result: Result<()> = async {
        while !app.should_quit {
            if shutdown_rx.try_recv().is_ok() {
                app.should_quit = true;
                continue;
            }
            while event::poll(Duration::ZERO)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key)?,
                    Event::Mouse(mouse) => app.handle_mouse(mouse)?,
                    Event::Resize(_, _) => app.dirty = true,
                    _ => {}
                }
            }
            while let Some(event) = app.mpv.try_event() {
                app.handle_player_event(event)?;
            }
            while let Ok(action) = app.actions.try_recv() {
                app.handle_action(action)?;
            }
            while let Ok(action) = app.remote_actions.try_recv() {
                app.handle_remote_action(action)?;
            }
            while let Ok(message) = app.scan_rx.try_recv() {
                app.handle_scan(message)?;
            }
            app.handle_gain_messages()?;
            app.tick_history()?;
            app.refresh_theme();
            if app.watch_rx.try_recv().is_ok()
                && !app.scan_running
                && app.last_scan.elapsed() > Duration::from_secs(2)
            {
                while app.watch_rx.try_recv().is_ok() {}
                app.start_scan();
            }

            let draw_interval = if app.playback.status == PlaybackStatus::Playing {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(2)
            };
            if app.dirty || last_draw.elapsed() >= draw_interval {
                app.refresh_cover();
                terminal.draw(|frame| draw(frame, &mut app))?;
                if app.cover_sig_now != app.cover_sig {
                    // Las portadas cambiaron de sitio: un repintado parcial deja
                    // mezcladas la vieja y la nueva, así que limpio y redibujo.
                    app.cover_sig = app.cover_sig_now;
                    terminal.clear()?;
                    terminal.draw(|frame| draw(frame, &mut app))?;
                }
                app.sync_mpris().await;
                app.sync_discord();
                app.dirty = false;
                last_draw = Instant::now();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }
    .await;

    let save_result = app.save_state();
    loop_result?;
    save_result
}

impl App {
    fn reload_library(&mut self) -> Result<()> {
        self.tracks = self.db.load_tracks()?;
        self.track_index = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), i))
            .collect();
        self.albums = group_albums(&self.tracks);
        self.artists = group_artists(&self.tracks);
        self.genres = group_genres(&self.tracks);
        self.playlists = self.db.load_playlists()?;
        self.smart_playlists = self.db.load_smart_playlists()?;
        self.saved_queues = self.db.load_saved_queues()?;
        self.stats = self.db.load_track_stats()?;
        self.added_at = self.db.load_added_at()?;
        self.history = self.db.load_history(500)?;
        self.refresh_artist_releases();
        if self.view == View::AlbumDetail && self.opened_album().is_none() {
            self.view = self.album_parent_view;
            self.opened_album_key = None;
        }
        if self.view == View::ArtistDetail && self.opened_artist().is_none() {
            self.view = View::Artists;
            self.opened_artist_name = None;
            self.artist_release_keys.clear();
        }
        if self.view == View::GenreDetail
            && !self.genres.iter().any(|genre| {
                self.opened_genre_name
                    .as_deref()
                    .is_some_and(|name| genre.name == name)
            })
        {
            self.view = View::Genres;
            self.opened_genre_name = None;
        }
        self.selected = self.selected.min(self.item_count().saturating_sub(1));
        self.dirty = true;
        Ok(())
    }

    fn refresh_theme(&mut self) {
        if self.last_theme_check.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_theme_check = Instant::now();
        let theme = UiTheme::load();
        if theme != self.theme {
            self.theme = theme;
            self.status = "Tema de Omarchy actualizado".into();
            self.dirty = true;
        }
    }

    fn start_scan(&mut self) {
        let roots = all_sources(&self.config);
        let tx = self.scan_tx.clone();
        let paths = self.paths.clone();
        self.scan_running = true;
        self.last_scan = Instant::now();
        self.status = format!("Escaneando {} fuente(s)…", roots.len());
        thread::Builder::new()
            .name("muscli-scanner".into())
            .spawn(move || {
                let mut ids = BTreeSet::new();
                for root in roots {
                    if let Ok(scan) = scan_source(&paths, &root) {
                        ids.insert(scan.id.clone());
                        if tx.send(ScanMessage::Source(scan)).is_err() {
                            return;
                        }
                    }
                }
                let _ = tx.send(ScanMessage::Done(ids));
            })
            .ok();
    }

    fn handle_scan(&mut self, message: ScanMessage) -> Result<()> {
        match message {
            ScanMessage::Source(scan) => {
                let moved_tracks =
                    self.db
                        .upsert_scan(&scan.id, &scan.root, &scan.label, &scan.tracks)?;
                for id in &mut self.queue {
                    if let Some((_, new_id)) = moved_tracks.iter().find(|(old_id, _)| id == old_id)
                    {
                        *id = new_id.clone();
                    }
                }
                self.db
                    .prune_missing_for_source(&scan.id, &scan.seen_paths)?;
                self.status = format!("Indexadas {} pistas de {}", scan.tracks.len(), scan.label);
                self.reload_library()?;
            }
            ScanMessage::Done(ids) => {
                self.db.mark_missing_sources(&ids)?;
                prune_unreferenced_covers(
                    &self.paths.cover_cache_dir(),
                    &self.db.referenced_cover_paths()?,
                )?;
                let removed = prune_cover_cache(
                    &self.paths.cover_cache_dir(),
                    self.config.cover_cache_mb * 1024 * 1024,
                )?;
                self.db.clear_cover_paths(&removed)?;
                self.scan_running = false;
                self.reload_library()?;
                self.status = format!(
                    "{} canciones · {} álbumes · listo",
                    self.tracks.len(),
                    self.albums.len()
                );
            }
        }
        Ok(())
    }

    fn item_count(&self) -> usize {
        match self.view {
            View::Home => self.home_track_ids().len(),
            View::Albums => self.albums.len(),
            View::ArtistDetail => self.artist_release_keys.len(),
            View::AlbumDetail => self.opened_album().map_or(0, |album| album.track_ids.len()),
            View::Artists => self.artists.len(),
            View::Genres => self.genres.len(),
            View::GenreDetail => self.genre_items_len(),
            View::Tracks => self.tracks.len(),
            View::Playlists => self.playlists.len(),
            View::SmartPlaylists => self.smart_playlists.len(),
            View::SmartPlaylistDetail => self.smart_track_ids().len(),
            View::Favorites => self.tracks.iter().filter(|t| t.favorite).count(),
            View::History => self.history.len(),
            View::Search => self.search_results().len(),
            View::Queue => self.queue.len(),
            View::Settings => SETTINGS.len(),
            View::Help => 0,
        }
    }

    fn search_results(&self) -> Vec<usize> {
        fuzzy_search(&self.tracks, &self.query, 100)
    }

    fn view_track_ids(&self) -> Vec<String> {
        match self.view {
            View::Home => self.home_track_ids(),
            View::Tracks => self.tracks.iter().map(|t| t.id.clone()).collect(),
            View::Favorites => self
                .tracks
                .iter()
                .filter(|t| t.favorite)
                .map(|t| t.id.clone())
                .collect(),
            View::Search => self
                .search_results()
                .into_iter()
                .map(|i| self.tracks[i].id.clone())
                .collect(),
            View::Queue => self.queue.clone(),
            View::Albums => self
                .albums
                .get(self.selected)
                .map(|a| a.track_ids.clone())
                .unwrap_or_default(),
            View::AlbumDetail => self
                .opened_album()
                .map(|album| album.track_ids.clone())
                .unwrap_or_default(),
            View::Artists => self
                .artists
                .get(self.selected)
                .map(|a| a.track_ids.clone())
                .unwrap_or_default(),
            View::Genres => self
                .genres
                .get(self.selected)
                .map(|genre| genre.track_ids.clone())
                .unwrap_or_default(),
            View::GenreDetail => self.genre_track_ids(),
            View::ArtistDetail => self
                .selected_album()
                .map(|album| album.track_ids.clone())
                .unwrap_or_default(),
            View::Playlists => self
                .playlists
                .get(self.selected)
                .map(|p| p.track_ids.clone())
                .unwrap_or_default(),
            View::SmartPlaylists => self
                .smart_playlists
                .get(self.selected)
                .map(|playlist| self.evaluate_smart(playlist))
                .unwrap_or_default(),
            View::SmartPlaylistDetail => self.smart_track_ids(),
            View::History => self
                .history
                .iter()
                .map(|entry| entry.track_id.clone())
                .collect(),
            View::Settings | View::Help => Vec::new(),
        }
    }

    fn selected_track_id(&self) -> Option<String> {
        match self.view {
            View::Home => self.home_track_ids().get(self.selected).cloned(),
            View::Tracks => self.tracks.get(self.selected).map(|t| t.id.clone()),
            View::Favorites => self
                .tracks
                .iter()
                .filter(|t| t.favorite)
                .nth(self.selected)
                .map(|t| t.id.clone()),
            View::Search => self
                .search_results()
                .get(self.selected)
                .map(|&i| self.tracks[i].id.clone()),
            View::Queue => self.queue.get(self.selected).cloned(),
            View::Albums => self
                .albums
                .get(self.selected)
                .and_then(|a| a.track_ids.first().cloned()),
            View::AlbumDetail => self
                .opened_album()
                .and_then(|album| album.track_ids.get(self.selected).cloned()),
            View::Artists => self
                .artists
                .get(self.selected)
                .and_then(|a| a.track_ids.first().cloned()),
            View::Genres => self
                .genres
                .get(self.selected)
                .and_then(|genre| genre.track_ids.first().cloned()),
            View::GenreDetail => self.genre_selected_track_id(),
            View::ArtistDetail => self
                .selected_album()
                .and_then(|album| album.track_ids.first().cloned()),
            View::Playlists => self
                .playlists
                .get(self.selected)
                .and_then(|p| p.track_ids.first().cloned()),
            View::SmartPlaylists => self
                .smart_playlists
                .get(self.selected)
                .and_then(|playlist| self.evaluate_smart(playlist).first().cloned()),
            View::SmartPlaylistDetail => self.smart_track_ids().get(self.selected).cloned(),
            View::History => self
                .history
                .get(self.selected)
                .map(|entry| entry.track_id.clone()),
            View::Settings | View::Help => None,
        }
    }

    fn current_track(&self) -> Option<&Track> {
        self.queue_index
            .and_then(|i| self.queue.get(i))
            .and_then(|id| self.track_index.get(id))
            .map(|&i| &self.tracks[i])
    }

    fn home_track_ids(&self) -> Vec<String> {
        let mut continuing = self
            .tracks
            .iter()
            .filter_map(|track| {
                let stats = self.stats.get(&track.id)?;
                (stats.resume_position_ms >= 30_000
                    && stats.resume_position_ms < track.duration_ms.saturating_mul(95) / 100)
                    .then_some((stats.last_played_at.unwrap_or_default(), track.id.clone()))
            })
            .collect::<Vec<_>>();
        continuing.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
        let mut ids = continuing
            .into_iter()
            .take(8)
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        for entry in &self.history {
            if !ids.contains(&entry.track_id) {
                ids.push(entry.track_id.clone());
            }
            if ids.len() >= 20 {
                break;
            }
        }
        ids
    }

    fn opened_genre(&self) -> Option<&Genre> {
        let name = self.opened_genre_name.as_deref()?;
        self.genres.iter().find(|genre| genre.name == name)
    }

    fn genre_track_ids(&self) -> Vec<String> {
        self.opened_genre()
            .map(|genre| genre.track_ids.clone())
            .unwrap_or_default()
    }

    fn genre_album_indices(&self) -> Vec<usize> {
        let ids = self.genre_track_ids().into_iter().collect::<HashSet<_>>();
        self.albums
            .iter()
            .enumerate()
            .filter(|(_, album)| album.track_ids.iter().any(|id| ids.contains(id)))
            .map(|(index, _)| index)
            .collect()
    }

    fn genre_artist_indices(&self) -> Vec<usize> {
        let ids = self.genre_track_ids().into_iter().collect::<HashSet<_>>();
        self.artists
            .iter()
            .enumerate()
            .filter(|(_, artist)| artist.track_ids.iter().any(|id| ids.contains(id)))
            .map(|(index, _)| index)
            .collect()
    }

    fn genre_items_len(&self) -> usize {
        match self.genre_tab {
            0 => self.genre_album_indices().len(),
            1 => self.genre_artist_indices().len(),
            _ => self.genre_track_ids().len(),
        }
    }

    fn genre_selected_track_id(&self) -> Option<String> {
        match self.genre_tab {
            0 => self
                .genre_album_indices()
                .get(self.selected)
                .and_then(|index| self.albums[*index].track_ids.first().cloned()),
            1 => self
                .genre_artist_indices()
                .get(self.selected)
                .and_then(|index| self.artists[*index].track_ids.first().cloned()),
            _ => self.genre_track_ids().get(self.selected).cloned(),
        }
    }

    fn evaluate_smart(&self, playlist: &SmartPlaylist) -> Vec<String> {
        evaluate_smart_playlist(
            playlist,
            &self.tracks,
            &self.stats,
            &self.added_at,
            chrono::Utc::now().timestamp(),
        )
    }

    fn smart_track_ids(&self) -> Vec<String> {
        self.opened_smart_playlist
            .and_then(|id| {
                self.smart_playlists
                    .iter()
                    .find(|playlist| playlist.id == id)
            })
            .map(|playlist| self.evaluate_smart(playlist))
            .unwrap_or_default()
    }

    fn selected_track(&self) -> Option<&Track> {
        self.selected_track_id()
            .and_then(|id| self.track_index.get(&id).copied())
            .map(|i| &self.tracks[i])
    }

    fn detail_track(&self) -> Option<&Track> {
        if matches!(self.view, View::AlbumDetail | View::ArtistDetail) {
            self.selected_track().or_else(|| self.current_track())
        } else {
            self.current_track().or_else(|| self.selected_track())
        }
    }

    fn opened_album(&self) -> Option<&Album> {
        let key = self.opened_album_key.as_deref()?;
        self.albums.iter().find(|album| album.key == key)
    }

    fn opened_artist(&self) -> Option<&Artist> {
        let name = self.opened_artist_name.as_deref()?;
        self.artists.iter().find(|artist| artist.name == name)
    }

    fn visible_album_indices(&self) -> Vec<usize> {
        if self.view == View::ArtistDetail {
            self.artist_release_keys
                .iter()
                .filter_map(|key| self.albums.iter().position(|album| &album.key == key))
                .collect()
        } else {
            (0..self.albums.len()).collect()
        }
    }

    fn selected_album(&self) -> Option<&Album> {
        let index = self.visible_album_indices().get(self.selected).copied()?;
        self.albums.get(index)
    }

    fn refresh_artist_releases(&mut self) {
        let Some(name) = self.opened_artist_name.as_deref() else {
            self.artist_release_keys.clear();
            return;
        };
        let Some(artist) = self.artists.iter().find(|artist| artist.name == name) else {
            self.artist_release_keys.clear();
            return;
        };
        let track_ids = artist.track_ids.iter().collect::<HashSet<_>>();
        self.artist_release_keys = self
            .albums
            .iter()
            .filter(|album| album.track_ids.iter().any(|id| track_ids.contains(id)))
            .map(|album| album.key.clone())
            .collect();
    }

    fn open_selected_artist(&mut self) {
        let Some(artist) = self.artists.get(self.selected) else {
            return;
        };
        self.artist_return_selection = self.selected;
        self.artist_parent_view = self.view;
        self.opened_artist_name = Some(artist.name.clone());
        self.refresh_artist_releases();
        self.view = View::ArtistDetail;
        self.selected = 0;
        self.focus = Focus::Content;
        self.status = "Selecciona un álbum o single · Esc para volver".into();
        self.dirty = true;
    }

    fn close_artist_detail(&mut self) {
        if self.view != View::ArtistDetail {
            return;
        }
        self.view = self.artist_parent_view;
        self.selected = self
            .opened_artist_name
            .as_deref()
            .and_then(|name| {
                if self.artist_parent_view == View::GenreDetail {
                    self.genre_artist_indices()
                        .iter()
                        .position(|index| self.artists[*index].name == name)
                } else {
                    self.artists.iter().position(|artist| artist.name == name)
                }
            })
            .unwrap_or(self.artist_return_selection)
            .min(self.artists.len().saturating_sub(1));
        self.opened_artist_name = None;
        self.artist_release_keys.clear();
        self.status.clear();
        self.dirty = true;
    }

    fn open_selected_album(&mut self) {
        let Some(album_key) = self.selected_album().map(|album| album.key.clone()) else {
            return;
        };
        self.album_return_selection = self.selected;
        self.album_parent_view = self.view;
        self.opened_album_key = Some(album_key);
        self.view = View::AlbumDetail;
        self.selected = 0;
        self.focus = Focus::Content;
        self.status = "Selecciona una canción · Esc para volver".into();
        self.dirty = true;
    }

    fn close_album_detail(&mut self) {
        if self.view != View::AlbumDetail {
            return;
        }
        self.view = self.album_parent_view;
        self.selected = self
            .opened_album_key
            .as_deref()
            .and_then(|key| {
                if self.album_parent_view == View::ArtistDetail {
                    self.artist_release_keys
                        .iter()
                        .position(|album_key| album_key == key)
                } else if self.album_parent_view == View::GenreDetail {
                    self.genre_album_indices()
                        .iter()
                        .position(|index| self.albums[*index].key == key)
                } else {
                    self.albums.iter().position(|album| album.key == key)
                }
            })
            .unwrap_or(self.album_return_selection)
            .min(self.item_count().saturating_sub(1));
        self.opened_album_key = None;
        self.status.clear();
        self.dirty = true;
    }

    fn activate_or_open(&mut self) -> Result<()> {
        if self.view == View::Artists {
            self.open_selected_artist();
            Ok(())
        } else if matches!(self.view, View::Albums | View::ArtistDetail) {
            self.open_selected_album();
            Ok(())
        } else if self.view == View::Genres {
            if let Some(genre) = self.genres.get(self.selected) {
                self.genre_return_selection = self.selected;
                self.opened_genre_name = Some(genre.name.clone());
                self.genre_tab = 0;
                self.selected = 0;
                self.view = View::GenreDetail;
            }
            Ok(())
        } else if self.view == View::GenreDetail && self.genre_tab == 0 {
            if let Some(index) = self.genre_album_indices().get(self.selected).copied() {
                self.album_return_selection = self.selected;
                self.album_parent_view = View::GenreDetail;
                self.opened_album_key = Some(self.albums[index].key.clone());
                self.view = View::AlbumDetail;
                self.selected = 0;
            }
            Ok(())
        } else if self.view == View::GenreDetail && self.genre_tab == 1 {
            if let Some(index) = self.genre_artist_indices().get(self.selected).copied() {
                self.artist_return_selection = self.selected;
                self.artist_parent_view = View::GenreDetail;
                self.opened_artist_name = Some(self.artists[index].name.clone());
                self.refresh_artist_releases();
                self.view = View::ArtistDetail;
                self.selected = 0;
            }
            Ok(())
        } else if self.view == View::SmartPlaylists {
            if let Some(playlist) = self.smart_playlists.get(self.selected) {
                self.opened_smart_playlist = Some(playlist.id);
                self.view = View::SmartPlaylistDetail;
                self.selected = 0;
            }
            Ok(())
        } else {
            self.activate_selection()
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
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
                        self.reload_library()?;
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
                    self.status = "Añadida a la cola".into();
                    self.dirty = true;
                }
            }
            KeyCode::Char('f') => {
                if let Some(id) = self.selected_track_id() {
                    let value = self.db.toggle_favorite(&id)?;
                    self.reload_library()?;
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
                    if let Some(playlist) = self.playlists.get(selected) {
                        self.db.add_to_playlist(playlist.id, &track_id)?;
                        self.status = format!("Añadida a {}", playlist.name);
                        self.reload_library()?;
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
                        self.reload_library()?;
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
                            self.reload_library()?;
                            self.status = format!("Playlist creada: {}", self.input_buffer.trim());
                        }
                        if mode == InputMode::Search {
                            self.query = self.input_buffer.clone();
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
                            self.selected = 0;
                        }
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.input_buffer.push(c);
                        if mode == InputMode::Search {
                            self.query = self.input_buffer.clone();
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

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> Result<()> {
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

    fn move_selection(&mut self, amount: isize) {
        if self.focus == Focus::Sidebar {
            let sidebar_view = match self.view {
                View::ArtistDetail => View::Artists,
                View::GenreDetail => View::Genres,
                View::SmartPlaylistDetail => View::SmartPlaylists,
                View::AlbumDetail if self.album_parent_view == View::ArtistDetail => View::Artists,
                View::AlbumDetail => View::Albums,
                view => view,
            };
            let current = VIEWS.iter().position(|v| *v == sidebar_view).unwrap_or(0);
            let next = (current as isize + amount).clamp(0, VIEWS.len() as isize - 1) as usize;
            self.view = VIEWS[next];
            self.opened_album_key = None;
            self.opened_artist_name = None;
            self.artist_release_keys.clear();
            self.selected = 0;
        } else {
            self.selected = (self.selected as isize + amount)
                .clamp(0, self.item_count().saturating_sub(1) as isize)
                as usize;
        }
        self.dirty = true;
    }

    fn move_album_selection(&mut self, amount: isize) {
        self.selected = shifted_index(self.selected, amount, self.item_count());
        self.dirty = true;
    }

    fn activate_selection(&mut self) -> Result<()> {
        let mut ids = self.view_track_ids();
        ids.retain(|id| {
            self.track_index
                .get(id)
                .is_some_and(|&i| self.tracks[i].available)
        });
        if ids.is_empty() {
            self.status = "No hay pistas disponibles en esta selección".into();
            return Ok(());
        }
        let target = self.selected_track_id();
        if self.shuffle {
            ids.shuffle(&mut rand::rng());
        }
        let start = target
            .and_then(|id| ids.iter().position(|candidate| candidate == &id))
            .unwrap_or(0);
        self.queue = ids;
        self.queue_index = Some(start);
        let position = self
            .current_track()
            .and_then(|track| self.stats.get(&track.id))
            .map_or(0, |stats| stats.resume_position_ms);
        self.load_current(if self.config.resume_enabled {
            position
        } else {
            0
        })
    }

    fn load_current(&mut self, position_ms: u64) -> Result<()> {
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
            self.history_id = Some(self.db.start_history(&track.id)?);
            self.history_track_id = Some(track.id.clone());
            self.listened_this_session_ms = 0;
            self.pending_listen_ms = 0;
            self.history_counted = false;
            self.last_history_tick = Instant::now();
            self.last_history_flush = Instant::now();
        }
        self.status.clear();
        self.dirty = true;
        self.persist_playback()?;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
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

    fn previous(&mut self) -> Result<()> {
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

    fn queue_track_is_playable(&self, index: usize) -> bool {
        self.queue
            .get(index)
            .and_then(|id| self.track_index.get(id))
            .map(|&track_index| &self.tracks[track_index])
            .is_some_and(|track| track.available && track.path.exists())
    }

    fn move_queue_item(&mut self, amount: isize) {
        if self.queue.is_empty() {
            return;
        }
        let target = shifted_index(self.selected, amount, self.queue.len());
        self.queue.swap(self.selected, target);
        if self.queue_index == Some(self.selected) {
            self.queue_index = Some(target);
        } else if self.queue_index == Some(target) {
            self.queue_index = Some(self.selected);
        }
        self.selected = target;
        self.dirty = true;
    }

    fn remove_queue_item(&mut self) {
        if self.selected >= self.queue.len() {
            return;
        }
        self.queue.remove(self.selected);
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

    fn run_context_action(&mut self, action: usize) -> Result<()> {
        let Some(track_id) = self.selected_track_id() else {
            return Ok(());
        };
        match action {
            0 => self.activate_selection()?,
            1 => {
                let position = self.queue_index.map_or(0, |index| index + 1);
                self.queue.insert(position.min(self.queue.len()), track_id);
                self.status = "Se reproducirá después".into();
            }
            2 => {
                self.queue.push(track_id);
                self.status = "Añadida al final de la cola".into();
            }
            3 => {
                let favorite = self.db.toggle_favorite(&track_id)?;
                self.reload_library()?;
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

    fn handle_remote_action(&mut self, action: RemoteCommand) -> Result<()> {
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
                if !self.scan_running {
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

    fn start_gain_analysis(&mut self) -> Result<()> {
        if !self.config.replaygain_enabled || self.gain_rx.is_some() {
            return Ok(());
        }
        let candidates = self.db.gain_analysis_candidates(false)?;
        if candidates.is_empty() {
            return Ok(());
        }
        let total = candidates.len();
        self.gain_rx = Some(replaygain::start(
            candidates,
            self.config.replaygain_target_lufs,
        ));
        self.gain_progress = Some((0, total));
        Ok(())
    }

    fn handle_gain_messages(&mut self) -> Result<()> {
        let Some(receiver) = self.gain_rx.take() else {
            return Ok(());
        };
        let mut done = false;
        while let Ok(message) = receiver.try_recv() {
            match message {
                GainMessage::Result {
                    track_id,
                    result,
                    size,
                    modified,
                    completed,
                    total,
                } => {
                    self.db.save_gain(&track_id, result, size, modified)?;
                    self.gain_progress = Some((completed, total));
                    self.status = format!("Analizando volumen {completed}/{total}");
                    self.dirty = true;
                }
                GainMessage::Error(error) => self.status = format!("ReplayGain: {error}"),
                GainMessage::Done => {
                    self.gain_progress = None;
                    self.status = "Análisis de volumen terminado".into();
                    done = true;
                }
            }
        }
        if !done {
            self.gain_rx = Some(receiver);
        }
        Ok(())
    }

    fn tick_history(&mut self) -> Result<()> {
        let elapsed = self.last_history_tick.elapsed();
        self.last_history_tick = Instant::now();
        if self.playback.status == PlaybackStatus::Playing && self.history_id.is_some() {
            let millis = elapsed.as_millis().min(u64::MAX as u128) as u64;
            self.listened_this_session_ms = self.listened_this_session_ms.saturating_add(millis);
            self.pending_listen_ms = self.pending_listen_ms.saturating_add(millis);
            if self.last_history_flush.elapsed() >= Duration::from_secs(5) {
                self.flush_history(false)?;
            }
        }
        if self.current_track().is_some()
            && self.last_playback_save.elapsed() >= Duration::from_secs(5)
        {
            self.persist_playback()?;
        }
        Ok(())
    }

    fn flush_history(&mut self, completed: bool) -> Result<()> {
        let (Some(history_id), Some(track_id)) = (self.history_id, self.history_track_id.clone())
        else {
            return Ok(());
        };
        let duration = self
            .track_index
            .get(&track_id)
            .map(|index| self.tracks[*index].duration_ms)
            .unwrap_or(self.playback.duration_ms);
        let threshold = (duration / 2).min(240_000);
        let count_now = self.listened_this_session_ms >= threshold && threshold > 0;
        let completed = completed
            || (duration > 0 && self.playback.position_ms >= duration.saturating_mul(95) / 100);
        self.db.update_history(
            history_id,
            &track_id,
            self.pending_listen_ms,
            self.playback.position_ms,
            count_now,
            completed,
        )?;
        self.pending_listen_ms = 0;
        self.history_counted |= count_now;
        self.last_history_flush = Instant::now();
        Ok(())
    }

    fn handle_action(&mut self, action: PlayerAction) -> Result<()> {
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

    fn handle_player_event(&mut self, event: PlayerEvent) -> Result<()> {
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
        self.dirty = true;
        Ok(())
    }

    fn refresh_cover(&mut self) {
        if !self.config.show_covers {
            self.cover = None;
            self.album_covers.clear();
            return;
        }
        let path = self
            .detail_track()
            .and_then(|track| track.cover_path.clone());
        if let Some(path) = path {
            if !self.cover.as_ref().is_some_and(|cover| cover.path == path) {
                self.cover = image::ImageReader::open(&path)
                    .ok()
                    .and_then(|reader| reader.decode().ok())
                    .map(|image| CoverState {
                        path,
                        protocol: self.picker.new_resize_protocol(image.thumbnail(512, 512)),
                    });
            }
        } else {
            self.cover = None;
        }
        // Keep the last visible album thumbnails while a detail view is open.
        // Returning to the grid can then reuse the decoded image protocols
        // instead of synchronously decoding every visible cover again.
    }

    async fn sync_mpris(&mut self) {
        let signature = format!(
            "{:?}|{}|{}|{:?}|{}|{}",
            self.playback.status,
            self.playback.volume,
            self.shuffle,
            self.repeat,
            self.queue_index.unwrap_or(usize::MAX),
            self.playback.position_ms / 1000
        );
        if signature == self.last_mpris_signature {
            return;
        }
        self.last_mpris_signature = signature;
        if let Some(mpris) = &self.mpris {
            let index = self.queue_index.unwrap_or(0);
            let _ = mpris
                .sync(
                    self.current_track(),
                    &self.playback,
                    self.shuffle,
                    self.repeat,
                    index > 0,
                    index + 1 < self.queue.len(),
                )
                .await;
        }
    }

    fn sync_discord(&mut self) {
        let track_id = self
            .current_track()
            .map(|track| track.id.as_str())
            .unwrap_or("");
        let signature = format!(
            "{track_id}|{:?}|{}|{}",
            self.playback.status,
            self.playback.duration_ms,
            self.playback.position_ms / 15_000
        );
        if signature == self.last_discord_signature {
            return;
        }
        self.last_discord_signature = signature;
        if let Some(discord) = &self.discord {
            discord.sync(self.current_track(), &self.playback);
        }
    }

    fn save_state(&mut self) -> Result<()> {
        self.flush_history(false)?;
        self.persist_playback()
    }

    fn persist_playback(&mut self) -> Result<()> {
        self.db.save_playback(&SavedPlayback {
            queue: self.queue.clone(),
            current_index: self.queue_index,
            position_ms: self.playback.position_ms,
            volume: self.playback.volume,
            last_nonzero_volume: self.muted_volume,
            shuffle: self.shuffle,
            repeat: self.repeat,
        })?;
        self.last_playback_save = Instant::now();
        Ok(())
    }
}

fn resize_terminal_for_mode(compact: bool) -> Result<()> {
    // El tamaño real de la ventana lo decide esto, no la regla de Hyprland: al
    // pedir N celdas, el terminal se redimensiona. En una pantalla de 1920x1080
    // con celdas de ~10x18 px, 180x52 ocupa unos 1790x960 px.
    let (columns, rows) = if compact { (95, 32) } else { (180, 52) };
    crossterm::execute!(std::io::stdout(), SetSize(columns, rows))?;
    Ok(())
}

fn start_watchers(config: &Config, tx: Sender<()>) {
    let configured = config.sources.clone();
    thread::Builder::new()
        .name("muscli-watcher".into())
        .spawn(move || {
            let event_tx = tx.clone();
            let Ok(mut watcher) = RecommendedWatcher::new(
                move |result: notify::Result<notify::Event>| {
                    if result.is_ok() {
                        let _ = event_tx.send(());
                    }
                },
                notify::Config::default(),
            ) else {
                return;
            };
            for root in configured.iter().filter(|p| p.exists()) {
                let _ = watcher.watch(root, RecursiveMode::Recursive);
            }

            let mut watched_removable = BTreeSet::new();
            loop {
                let current_removable = crate::config::discover_removable_roots()
                    .into_iter()
                    .filter(|path| path.exists())
                    .collect::<BTreeSet<_>>();
                let mut changed = false;

                for root in current_removable.difference(&watched_removable) {
                    if watcher.watch(root, RecursiveMode::Recursive).is_ok() {
                        changed = true;
                    }
                }
                for root in watched_removable.difference(&current_removable) {
                    let _ = watcher.unwatch(root);
                    changed = true;
                }

                if changed {
                    let _ = tx.send(());
                }
                watched_removable = current_removable;
                thread::sleep(Duration::from_secs(2));
            }
        })
        .ok();
}

#[cfg(unix)]
fn start_shutdown_listener() -> Result<Receiver<()>> {
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    for kind in [
        tokio::signal::unix::SignalKind::interrupt(),
        tokio::signal::unix::SignalKind::terminate(),
        tokio::signal::unix::SignalKind::hangup(),
    ] {
        let mut signal = tokio::signal::unix::signal(kind)?;
        let tx = shutdown_tx.clone();
        tokio::task::spawn_local(async move {
            let _ = signal.recv().await;
            let _ = tx.send(());
        });
    }
    Ok(shutdown_rx)
}

#[cfg(windows)]
fn start_shutdown_listener() -> Result<Receiver<()>> {
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    tokio::task::spawn_local(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(());
    });
    Ok(shutdown_rx)
}

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
            .into_iter()
            .map(|index| {
                let album = &app.albums[index];
                ListItem::new(format!("󰀥  {} — {}", album.title, album.artist))
            })
            .collect(),
        1 => app
            .genre_artist_indices()
            .into_iter()
            .map(|index| ListItem::new(format!("󰠃  {}", app.artists[index].name)))
            .collect(),
        _ => app
            .genre_track_ids()
            .into_iter()
            .filter_map(|id| app.track_index.get(&id).map(|index| &app.tracks[*index]))
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
    let album_indices = app.visible_album_indices();
    if album_indices.is_empty() {
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
    let last = (first + visible_rows * columns).min(album_indices.len());
    let desired_covers = album_indices[first..last]
        .iter()
        .filter_map(|&index| app.albums[index].cover_path.clone())
        .collect::<BTreeSet<_>>();
    app.album_covers
        .retain(|path, _| desired_covers.contains(path));
    for path in desired_covers {
        if app.album_covers.contains_key(&path) {
            continue;
        }
        if let Some(image) = image::ImageReader::open(&path)
            .ok()
            .and_then(|reader| reader.decode().ok())
        {
            app.album_covers.insert(
                path,
                app.picker.new_resize_protocol(image.thumbnail(256, 256)),
            );
        }
    }
    for (position, &index) in album_indices.iter().enumerate().take(last).skip(first) {
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

fn draw_tracks(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let indices: Vec<usize> = match app.view {
        View::Tracks => (0..app.tracks.len()).collect(),
        View::Favorites => app
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.favorite)
            .map(|(i, _)| i)
            .collect(),
        View::Search => app.search_results(),
        View::History => app
            .history
            .iter()
            .filter_map(|entry| app.track_index.get(&entry.track_id).copied())
            .collect(),
        View::SmartPlaylistDetail => app
            .smart_track_ids()
            .iter()
            .filter_map(|id| app.track_index.get(id).copied())
            .collect(),
        View::Queue => app
            .queue
            .iter()
            .filter_map(|id| app.track_index.get(id).copied())
            .collect(),
        View::AlbumDetail => app
            .opened_album()
            .into_iter()
            .flat_map(|album| album.track_ids.iter())
            .filter_map(|id| app.track_index.get(id).copied())
            .collect(),
        _ => Vec::new(),
    };
    if indices.is_empty() {
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
    let rows = indices.iter().enumerate().map(|(position, &index)| {
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
        Row::new(vec![
            Cell::from(format!(
                "{marker} {:02}",
                if app.view == View::Queue {
                    position + 1
                } else {
                    track.track_number as usize
                }
            )),
            Cell::from(track.title.clone()),
            Cell::from(track.artist.clone()),
            Cell::from(track.album.clone()),
            Cell::from(format_duration(track.duration_ms)),
        ])
        .style(if track.available {
            Style::default()
        } else {
            Style::default().fg(app.theme.border)
        })
    });
    let widths = [
        Constraint::Length(5),
        Constraint::Percentage(32),
        Constraint::Percentage(24),
        Constraint::Percentage(30),
        Constraint::Length(6),
    ];
    let mut state = TableState::default().with_selected(Some(app.selected));
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

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn shifted_index(current: usize, amount: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (current as isize + amount).clamp(0, len.saturating_sub(1) as isize) as usize
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

fn display_rule_value(rule: &SmartRule) -> String {
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

fn empty_library(theme: UiTheme) -> Paragraph<'static> {
    Paragraph::new("\nNo encontré archivos FLAC.\n\nInserta una SD/USB o ejecuta:\nmuscli library add /ruta/a/Music")
        .alignment(Alignment::Center).style(Style::default().fg(theme.muted))
}

fn format_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_human_readable() {
        assert_eq!(format_duration(185_000), "3:05");
    }

    #[test]
    fn centered_rect_is_inside_parent() {
        let parent = Rect::new(0, 0, 80, 24);
        assert_eq!(centered(parent, 40, 10), Rect::new(20, 7, 40, 10));
    }

    #[test]
    fn parses_omarchy_hex_colors() {
        assert_eq!(parse_hex_color("#ff2ec1"), Some(Color::Rgb(255, 46, 193)));
        assert_eq!(parse_hex_color("ff2ec1"), None);
        assert_eq!(parse_hex_color("#bad"), None);
    }

    #[test]
    fn loads_an_omarchy_colors_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("colors.toml");
        fs::write(
            &path,
            "mode = \"dark\"\naccent = \"#509475\"\nselection = \"#32473B\"\nforeground = \"#C1C497\"\nbackground = \"#111c18\"\nmuted = \"#53685B\"\n",
        )
        .unwrap();
        let theme = UiTheme::from_file(&path).unwrap();
        assert_eq!(theme.accent, Color::Rgb(80, 148, 117));
        assert_eq!(theme.selection, Color::Rgb(50, 71, 59));
        assert_eq!(theme.background, Color::Rgb(17, 28, 24));
    }

    #[test]
    fn grid_navigation_moves_by_columns_and_clamps() {
        assert_eq!(shifted_index(2, 4, 10), 6);
        assert_eq!(shifted_index(2, -4, 10), 0);
        assert_eq!(shifted_index(8, 4, 10), 9);
        assert_eq!(shifted_index(0, 1, 0), 0);
    }
}
