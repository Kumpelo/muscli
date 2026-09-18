use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant, SystemTime},
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
use tokio::sync::mpsc as tokio_mpsc;

use crate::{
    config::{Config, ReplayGainMode, all_sources},
    control::{ControlServer, RemoteCommand},
    db::{Database, HistoryUpdate, group_albums, group_artists},
    discord::DiscordPresence,
    features::{Genre, SearchIndex, evaluate_smart_playlist, group_genres},
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

#[derive(Debug)]
struct CoverDecodeRequest {
    path: PathBuf,
    size: u32,
}

struct CoverDecodeResult {
    path: PathBuf,
    size: u32,
    image: Option<image::DynamicImage>,
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
    fn source_paths() -> [Option<PathBuf>; 2] {
        let state_home = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            });
        let config_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".config"));
        [
            state_home.map(|root| root.join("omarchy/current/theme/colors.toml")),
            config_home.map(|root| root.join("omarchy/current/theme/colors.toml")),
        ]
    }

    fn load_with_source() -> (Self, Option<PathBuf>, Option<SystemTime>) {
        for path in Self::source_paths().into_iter().flatten() {
            if let Some(theme) = Self::from_file(&path) {
                let modified = fs::metadata(&path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok());
                return (theme, Some(path), modified);
            }
        }
        (Self::default(), None, None)
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
    favorite_indices: Vec<usize>,
    albums: Vec<Album>,
    album_index: HashMap<String, usize>,
    artists: Vec<Artist>,
    artist_index: HashMap<String, usize>,
    playlists: Vec<Playlist>,
    smart_playlists: Vec<SmartPlaylist>,
    smart_matches: HashMap<i64, Vec<String>>,
    saved_queues: Vec<SavedQueue>,
    stats: HashMap<String, TrackStats>,
    added_at: HashMap<String, i64>,
    history: Vec<HistoryEntry>,
    home_tracks: Vec<String>,
    genres: Vec<Genre>,
    genre_album_cache: HashMap<String, Vec<usize>>,
    genre_artist_cache: HashMap<String, Vec<usize>>,
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
    search_index: SearchIndex,
    search_matches: Vec<usize>,
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
    actions: tokio_mpsc::UnboundedReceiver<PlayerAction>,
    remote_actions: tokio_mpsc::UnboundedReceiver<RemoteCommand>,
    _control_server: ControlServer,
    gain_rx: Option<Receiver<GainMessage>>,
    gain_progress: Option<(usize, usize)>,
    scan_rx: Receiver<ScanMessage>,
    scan_tx: Sender<ScanMessage>,
    watch_rx: Receiver<()>,
    scan_running: bool,
    scan_pending: bool,
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
    album_cover_order: VecDeque<PathBuf>,
    cover_decode_tx: Sender<CoverDecodeRequest>,
    cover_decode_rx: tokio_mpsc::UnboundedReceiver<CoverDecodeResult>,
    cover_decode_pending: HashSet<(PathBuf, u32)>,
    album_columns: usize,
    last_mpris_signature: Option<(Option<u64>, PlaybackStatus, u64, bool, RepeatMode, bool, bool)>,
    last_mpris_position_signature: Option<(u64, u64)>,
    last_discord_signature: Option<(Option<u64>, PlaybackStatus, u64, u64)>,
    theme: UiTheme,
    theme_path: Option<PathBuf>,
    theme_modified: Option<SystemTime>,
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
    let (action_tx, actions) = tokio_mpsc::unbounded_channel();
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
    let (theme, theme_path, theme_modified) = UiTheme::load_with_source();
    let (cover_decode_tx, cover_decode_requests) = mpsc::channel();
    let (cover_results_tx, cover_decode_rx) = tokio_mpsc::unbounded_channel();
    start_cover_decode_worker(cover_decode_requests, cover_results_tx);

    let mut app = App {
        paths,
        config,
        db,
        tracks: Vec::new(),
        track_index: HashMap::new(),
        favorite_indices: Vec::new(),
        albums: Vec::new(),
        album_index: HashMap::new(),
        artists: Vec::new(),
        artist_index: HashMap::new(),
        playlists: Vec::new(),
        smart_playlists: Vec::new(),
        smart_matches: HashMap::new(),
        saved_queues: Vec::new(),
        stats: HashMap::new(),
        added_at: HashMap::new(),
        history: Vec::new(),
        home_tracks: Vec::new(),
        genres: Vec::new(),
        genre_album_cache: HashMap::new(),
        genre_artist_cache: HashMap::new(),
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
        search_index: SearchIndex::default(),
        search_matches: Vec::new(),
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
        scan_pending: false,
        last_scan: Instant::now() - Duration::from_secs(5),
        status: mpris_warning.unwrap_or_else(|| "Cargando biblioteca…".into()),
        should_quit: false,
        dirty: true,
        picker,
        cover: None,
        cover_sig: 0,
        cover_sig_now: 0,
        album_covers: HashMap::new(),
        album_cover_order: VecDeque::new(),
        cover_decode_tx,
        cover_decode_rx,
        cover_decode_pending: HashSet::new(),
        album_columns: 1,
        last_mpris_signature: None,
        last_mpris_position_signature: None,
        last_discord_signature: None,
        theme,
        theme_path,
        theme_modified,
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

    let mut shutdown_rx = start_shutdown_listener()?;
    let mut terminal_events = start_terminal_event_reader();
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let loop_result: Result<()> = async {
        while !app.should_quit {
            let maintenance_delay = if app.scan_running || app.gain_rx.is_some() {
                Duration::from_millis(100)
            } else if app.playback.status == PlaybackStatus::Playing {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(1)
            };

            tokio::select! {
                event = terminal_events.recv() => {
                    if let Some(event) = event {
                        handle_terminal_event(&mut app, event)?;
                    }
                }
                action = app.actions.recv() => {
                    if let Some(action) = action {
                        app.handle_action(action)?;
                    }
                }
                event = app.mpv.recv_event() => {
                    if let Some(event) = event {
                        app.handle_player_event(event)?;
                    }
                }
                action = app.remote_actions.recv() => {
                    if let Some(action) = action {
                        app.handle_remote_action(action)?;
                    }
                }
                result = app.cover_decode_rx.recv() => {
                    if let Some(result) = result {
                        app.handle_cover_decode_result(result);
                    }
                }
                shutdown = shutdown_rx.recv() => {
                    if shutdown.is_some() {
                        app.should_quit = true;
                        continue;
                    }
                }
                _ = tokio::time::sleep(maintenance_delay) => {}
            }

            app.playback.position_ms = app.mpv.position_ms();
            while let Ok(event) = terminal_events.try_recv() {
                handle_terminal_event(&mut app, event)?;
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
            if app.watch_rx.try_recv().is_ok() {
                while app.watch_rx.try_recv().is_ok() {}
                app.scan_pending = true;
            }
            if app.scan_pending
                && !app.scan_running
                && app.last_scan.elapsed() > Duration::from_secs(2)
            {
                app.scan_pending = false;
                app.start_scan();
            }

            let periodic_draw_due = app.playback.status == PlaybackStatus::Playing
                && last_draw.elapsed() >= Duration::from_millis(250);
            if app.dirty || periodic_draw_due {
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
        let _profile = crate::profiling::span("reload_library");
        self.tracks = self.db.load_tracks()?;
        self.search_index = SearchIndex::build(&self.tracks);
        self.refresh_search();
        self.track_index = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), i))
            .collect();
        self.favorite_indices = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| track.favorite.then_some(index))
            .collect();
        self.albums = group_albums(&self.tracks);
        self.album_index = self
            .albums
            .iter()
            .enumerate()
            .map(|(index, album)| (album.key.clone(), index))
            .collect();
        self.artists = group_artists(&self.tracks);
        self.artist_index = self
            .artists
            .iter()
            .enumerate()
            .map(|(index, artist)| (artist.name.clone(), index))
            .collect();
        self.genres = group_genres(&self.tracks);
        self.rebuild_genre_indices();
        self.playlists = self.db.load_playlists()?;
        self.smart_playlists = self.db.load_smart_playlists()?;
        self.saved_queues = self.db.load_saved_queues()?;
        self.stats = self.db.load_track_stats()?;
        self.added_at = self.db.load_added_at()?;
        self.history = self.db.load_history(500)?;
        self.rebuild_home_tracks();
        let now = chrono::Utc::now().timestamp();
        self.smart_matches = self
            .smart_playlists
            .iter()
            .map(|playlist| {
                (
                    playlist.id,
                    evaluate_smart_playlist(
                        playlist,
                        &self.tracks,
                        &self.search_index,
                        &self.stats,
                        &self.added_at,
                        now,
                    ),
                )
            })
            .collect();
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

        #[cfg(unix)]
        {
            let modified = self
                .theme_path
                .as_ref()
                .and_then(|path| fs::metadata(path).ok())
                .and_then(|metadata| metadata.modified().ok());
            if self.theme_path.is_some() && modified == self.theme_modified {
                return;
            }

            let (theme, path, modified) = UiTheme::load_with_source();
            self.theme_path = path;
            self.theme_modified = modified;
            if theme != self.theme {
                self.theme = theme;
                self.status = "Tema de Omarchy actualizado".into();
                self.dirty = true;
            }
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
                self.dirty = true;
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
            View::Favorites => self.favorite_indices.len(),
            View::History => self.history.len(),
            View::Search => self.search_results().len(),
            View::Queue => self.queue.len(),
            View::Settings => SETTINGS.len(),
            View::Help => 0,
        }
    }

    fn track_view_len(&self) -> usize {
        match self.view {
            View::Tracks => self.tracks.len(),
            View::Favorites => self.favorite_indices.len(),
            View::Search => self.search_matches.len(),
            View::History => self.history.len(),
            View::SmartPlaylistDetail => self.smart_track_ids().len(),
            View::Queue => self.queue.len(),
            View::AlbumDetail => self.opened_album().map_or(0, |album| album.track_ids.len()),
            _ => 0,
        }
    }

    fn track_index_at_view_position(&self, position: usize) -> Option<usize> {
        match self.view {
            View::Tracks => (position < self.tracks.len()).then_some(position),
            View::Favorites => self.favorite_indices.get(position).copied(),
            View::Search => self.search_matches.get(position).copied(),
            View::History => self
                .history
                .get(position)
                .and_then(|entry| self.track_index.get(&entry.track_id).copied()),
            View::SmartPlaylistDetail => self
                .smart_track_ids()
                .get(position)
                .and_then(|id| self.track_index.get(id).copied()),
            View::Queue => self
                .queue
                .get(position)
                .and_then(|id| self.track_index.get(id).copied()),
            View::AlbumDetail => self
                .opened_album()
                .and_then(|album| album.track_ids.get(position))
                .and_then(|id| self.track_index.get(id).copied()),
            _ => None,
        }
    }

    fn search_results(&self) -> &[usize] {
        &self.search_matches
    }

    fn refresh_search(&mut self) {
        self.search_matches = self.search_index.search(&self.query, 100);
    }

    fn view_track_ids(&self) -> Vec<String> {
        match self.view {
            View::Home => self.home_track_ids().to_vec(),
            View::Tracks => self.tracks.iter().map(|t| t.id.clone()).collect(),
            View::Favorites => self
                .tracks
                .iter()
                .filter(|t| t.favorite)
                .map(|t| t.id.clone())
                .collect(),
            View::Search => self
                .search_results()
                .iter()
                .map(|&i| self.tracks[i].id.clone())
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
            View::GenreDetail => self.genre_track_ids().to_vec(),
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
                .map(|playlist| self.evaluate_smart(playlist).to_vec())
                .unwrap_or_default(),
            View::SmartPlaylistDetail => self.smart_track_ids().to_vec(),
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
                .favorite_indices
                .get(self.selected)
                .map(|&index| self.tracks[index].id.clone()),
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

    fn rebuild_home_tracks(&mut self) {
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
        let mut seen = ids.iter().cloned().collect::<HashSet<_>>();
        for entry in &self.history {
            if seen.insert(entry.track_id.clone()) {
                ids.push(entry.track_id.clone());
            }
            if ids.len() >= 20 {
                break;
            }
        }
        self.home_tracks = ids;
    }

    fn home_track_ids(&self) -> &[String] {
        &self.home_tracks
    }

    fn opened_genre(&self) -> Option<&Genre> {
        let name = self.opened_genre_name.as_deref()?;
        self.genres.iter().find(|genre| genre.name == name)
    }

    fn genre_track_ids(&self) -> &[String] {
        self.opened_genre()
            .map(|genre| genre.track_ids.as_slice())
            .unwrap_or(&[])
    }

    fn rebuild_genre_indices(&mut self) {
        let track_genre = self
            .genres
            .iter()
            .flat_map(|genre| {
                genre
                    .track_ids
                    .iter()
                    .map(move |track_id| (track_id.as_str(), genre.name.as_str()))
            })
            .collect::<HashMap<_, _>>();

        let mut album_cache = HashMap::<String, Vec<usize>>::new();
        for (index, album) in self.albums.iter().enumerate() {
            let mut seen = HashSet::new();
            for track_id in &album.track_ids {
                if let Some(genre) = track_genre.get(track_id.as_str())
                    && seen.insert(*genre)
                {
                    album_cache
                        .entry((*genre).to_owned())
                        .or_default()
                        .push(index);
                }
            }
        }

        let mut artist_cache = HashMap::<String, Vec<usize>>::new();
        for (index, artist) in self.artists.iter().enumerate() {
            let mut seen = HashSet::new();
            for track_id in &artist.track_ids {
                if let Some(genre) = track_genre.get(track_id.as_str())
                    && seen.insert(*genre)
                {
                    artist_cache
                        .entry((*genre).to_owned())
                        .or_default()
                        .push(index);
                }
            }
        }

        self.genre_album_cache = album_cache;
        self.genre_artist_cache = artist_cache;
    }

    fn genre_album_indices(&self) -> &[usize] {
        self.opened_genre_name
            .as_deref()
            .and_then(|name| self.genre_album_cache.get(name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn genre_artist_indices(&self) -> &[usize] {
        self.opened_genre_name
            .as_deref()
            .and_then(|name| self.genre_artist_cache.get(name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
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

    fn evaluate_smart(&self, playlist: &SmartPlaylist) -> &[String] {
        self.smart_matches
            .get(&playlist.id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn smart_track_ids(&self) -> &[String] {
        self.opened_smart_playlist
            .and_then(|id| self.smart_matches.get(&id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
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
        let index = self.album_index.get(key).copied()?;
        self.albums.get(index)
    }

    fn opened_artist(&self) -> Option<&Artist> {
        let name = self.opened_artist_name.as_deref()?;
        let index = self.artist_index.get(name).copied()?;
        self.artists.get(index)
    }

    fn visible_album_len(&self) -> usize {
        if self.view == View::ArtistDetail {
            self.artist_release_keys.len()
        } else {
            self.albums.len()
        }
    }

    fn visible_album_index_at(&self, position: usize) -> Option<usize> {
        if self.view == View::ArtistDetail {
            self.artist_release_keys
                .get(position)
                .and_then(|key| self.album_index.get(key).copied())
        } else {
            (position < self.albums.len()).then_some(position)
        }
    }

    fn selected_album(&self) -> Option<&Album> {
        if self.view == View::ArtistDetail {
            let key = self.artist_release_keys.get(self.selected)?;
            let index = self.album_index.get(key).copied()?;
            self.albums.get(index)
        } else {
            self.albums.get(self.selected)
        }
    }

    fn refresh_artist_releases(&mut self) {
        let Some(name) = self.opened_artist_name.as_deref() else {
            self.artist_release_keys.clear();
            return;
        };
        let Some(artist) = self
            .artist_index
            .get(name)
            .and_then(|index| self.artists.get(*index))
        else {
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
                    self.artist_index.get(name).copied()
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
        let playing = self.playback.status == PlaybackStatus::Playing;
        if playing && self.history_id.is_some() {
            let millis = elapsed.as_millis().min(u64::MAX as u128) as u64;
            self.listened_this_session_ms = self.listened_this_session_ms.saturating_add(millis);
            self.pending_listen_ms = self.pending_listen_ms.saturating_add(millis);
        }

        let history_due = playing
            && self.history_id.is_some()
            && self.last_history_flush.elapsed() >= Duration::from_secs(5);
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

    fn flush_history(&mut self, completed: bool) -> Result<()> {
        self.flush_history_inner(completed, false)
    }

    fn flush_history_with_playback(&mut self, completed: bool) -> Result<()> {
        self.flush_history_inner(completed, true)
    }

    fn flush_history_inner(&mut self, completed: bool, save_playback: bool) -> Result<()> {
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
        if save_playback {
            let state = self.playback_snapshot();
            self.db.update_history_and_playback(
                HistoryUpdate {
                    history_id,
                    track_id: &track_id,
                    listened_delta_ms: self.pending_listen_ms,
                    position_ms: self.playback.position_ms,
                    was_counted: self.history_counted,
                    count_now,
                    completed,
                },
                &state,
            )?;
            self.last_playback_save = Instant::now();
        } else {
            self.db.update_history(HistoryUpdate {
                history_id,
                track_id: &track_id,
                listened_delta_ms: self.pending_listen_ms,
                position_ms: self.playback.position_ms,
                was_counted: self.history_counted,
                count_now,
                completed,
            })?;
        }
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

    fn refresh_cover(&mut self) {
        if !self.config.show_covers {
            self.cover = None;
            self.album_covers.clear();
            self.album_cover_order.clear();
            self.cover_decode_pending.clear();
            return;
        }

        let path = self
            .detail_track()
            .and_then(|track| track.cover_path.clone());
        match path {
            Some(path) if self.cover.as_ref().is_some_and(|cover| cover.path == path) => {}
            Some(path) => {
                self.cover = None;
                self.request_cover_decode(path, 512);
            }
            None => self.cover = None,
        }
    }

    fn request_cover_decode(&mut self, path: PathBuf, size: u32) {
        let key = (path.clone(), size);
        if !self.cover_decode_pending.insert(key.clone()) {
            return;
        }
        if self
            .cover_decode_tx
            .send(CoverDecodeRequest { path, size })
            .is_err()
        {
            self.cover_decode_pending.remove(&key);
        }
    }

    fn handle_cover_decode_result(&mut self, result: CoverDecodeResult) {
        self.cover_decode_pending
            .remove(&(result.path.clone(), result.size));
        let Some(image) = result.image else {
            return;
        };

        match result.size {
            512 => {
                let wanted = self
                    .detail_track()
                    .and_then(|track| track.cover_path.as_ref());
                if wanted.is_some_and(|path| path == &result.path) {
                    self.cover = Some(CoverState {
                        path: result.path,
                        protocol: self.picker.new_resize_protocol(image),
                    });
                    self.dirty = true;
                }
            }
            256 => {
                self.album_covers.insert(
                    result.path.clone(),
                    self.picker.new_resize_protocol(image),
                );
                self.album_cover_order.retain(|path| path != &result.path);
                self.album_cover_order.push_back(result.path);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn current_track_hash(&self) -> Option<u64> {
        self.current_track().map(|track| {
            let mut hasher = DefaultHasher::new();
            track.id.hash(&mut hasher);
            hasher.finish()
        })
    }

    async fn sync_mpris(&mut self) {
        let index = self.queue_index.unwrap_or(0);
        let state_signature = (
            self.current_track_hash(),
            self.playback.status,
            self.playback.volume.to_bits(),
            self.shuffle,
            self.repeat,
            index > 0,
            index + 1 < self.queue.len(),
        );
        let position_signature = (self.playback.duration_ms, self.playback.position_ms / 1000);

        if let Some(mpris) = &self.mpris {
            if self.last_mpris_signature != Some(state_signature) {
                let _ = mpris
                    .sync(
                        self.current_track(),
                        &self.playback,
                        self.shuffle,
                        self.repeat,
                        state_signature.5,
                        state_signature.6,
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

    fn sync_discord(&mut self) {
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

    fn save_state(&mut self) -> Result<()> {
        self.flush_history(false)?;
        self.persist_playback()
    }

    fn playback_snapshot(&self) -> SavedPlayback {
        SavedPlayback {
            queue: self.queue.clone(),
            current_index: self.queue_index,
            position_ms: self.playback.position_ms,
            volume: self.playback.volume,
            last_nonzero_volume: self.muted_volume,
            shuffle: self.shuffle,
            repeat: self.repeat,
        }
    }

    fn persist_playback(&mut self) -> Result<()> {
        self.db.save_playback(&self.playback_snapshot())?;
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

            let mut known_removable = crate::config::discover_removable_roots()
                .into_iter()
                .filter(|path| path.exists())
                .collect::<BTreeSet<_>>();
            let mut watched_removable = BTreeSet::new();
            loop {
                let current_removable = crate::config::discover_removable_roots()
                    .into_iter()
                    .filter(|path| path.exists())
                    .collect::<BTreeSet<_>>();

                if current_removable != known_removable {
                    let _ = tx.send(());
                    known_removable = current_removable.clone();
                }

                let added = current_removable
                    .difference(&watched_removable)
                    .cloned()
                    .collect::<Vec<_>>();
                for root in added {
                    if watcher.watch(&root, RecursiveMode::Recursive).is_ok() {
                        watched_removable.insert(root);
                    }
                }

                let removed = watched_removable
                    .difference(&current_removable)
                    .cloned()
                    .collect::<Vec<_>>();
                for root in removed {
                    let _ = watcher.unwatch(&root);
                    watched_removable.remove(&root);
                }

                thread::sleep(Duration::from_secs(2));
            }
        })
        .ok();
}

fn start_cover_decode_worker(
    requests: Receiver<CoverDecodeRequest>,
    results: tokio_mpsc::UnboundedSender<CoverDecodeResult>,
) {
    thread::Builder::new()
        .name("muscli-cover-decode".into())
        .spawn(move || {
            while let Ok(request) = requests.recv() {
                let image = image::ImageReader::open(&request.path)
                    .ok()
                    .and_then(|reader| reader.decode().ok())
                    .map(|image| image.thumbnail(request.size, request.size));
                if results
                    .send(CoverDecodeResult {
                        path: request.path,
                        size: request.size,
                        image,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .ok();
}

fn start_terminal_event_reader() -> tokio_mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = tokio_mpsc::unbounded_channel();
    thread::Builder::new()
        .name("muscli-terminal-events".into())
        .spawn(move || {
            while let Ok(event) = event::read() {
                if tx.send(event).is_err() {
                    break;
                }
            }
        })
        .ok();
    rx
}

fn handle_terminal_event(app: &mut App, event: Event) -> Result<()> {
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

#[cfg(unix)]
fn start_shutdown_listener() -> Result<tokio_mpsc::UnboundedReceiver<()>> {
    let (shutdown_tx, shutdown_rx) = tokio_mpsc::unbounded_channel();
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
fn start_shutdown_listener() -> Result<tokio_mpsc::UnboundedReceiver<()>> {
    let (shutdown_tx, shutdown_rx) = tokio_mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(());
    });
    Ok(shutdown_rx)
}

include!("tui/render.rs");

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
    fn track_viewport_matches_table_scroll_behavior() {
        assert_eq!(track_viewport(100, 0, 10), 0..9);
        assert_eq!(track_viewport(100, 8, 10), 0..9);
        assert_eq!(track_viewport(100, 9, 10), 1..10);
        assert_eq!(track_viewport(100, 99, 10), 91..100);
        assert_eq!(track_viewport(3, 2, 10), 0..3);
        assert_eq!(track_viewport(3, 2, 1), 0..0);
    }

    #[test]
    fn grid_navigation_moves_by_columns_and_clamps() {
        assert_eq!(shifted_index(2, 4, 10), 6);
        assert_eq!(shifted_index(2, -4, 10), 0);
        assert_eq!(shifted_index(8, 4, 10), 9);
        assert_eq!(shifted_index(0, 1, 0), 0);
    }
}
