use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

#[cfg(any(unix, test))]
use std::path::Path;
#[cfg(unix)]
use std::time::SystemTime;

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

use input::{display_rule_value, handle_terminal_event};
use render::draw;
use theme::UiTheme;
use workers::{
    CoverDecodeRequest, CoverDecodeResult, ScanMessage, SearchRequest, SearchResult,
    start_cover_decode_worker, start_search_worker, start_shutdown_listener,
    start_terminal_event_reader, start_watchers,
};

use crate::{
    config::{Config, ReplayGainMode, all_sources},
    control::{ControlServer, RemoteCommand},
    db::{Database, HistoryUpdate, group_albums, group_artists},
    discord::DiscordPresence,
    features::{Genre, SearchIndex, evaluate_smart_playlist, group_genres},
    instance::InstanceGuard,
    library::{prune_cover_cache, prune_unreferenced_covers, scan_source_with_database},
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

struct CoverState {
    path: PathBuf,
    protocol: StatefulProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct MediaSessionSignature {
    track_hash: Option<u64>,
    status: PlaybackStatus,
    volume_bits: u64,
    shuffle: bool,
    repeat: RepeatMode,
    can_previous: bool,
    can_next: bool,
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
    search_tx: Sender<SearchRequest>,
    search_rx: tokio_mpsc::UnboundedReceiver<SearchResult>,
    search_generation: u64,
    input: Option<InputMode>,
    input_buffer: String,
    queue: Vec<String>,
    queue_index: Option<usize>,
    queue_dirty: bool,
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
    gain_tx: tokio_mpsc::UnboundedSender<GainMessage>,
    gain_rx: tokio_mpsc::UnboundedReceiver<GainMessage>,
    gain_running: bool,
    gain_progress: Option<(usize, usize)>,
    scan_rx: tokio_mpsc::UnboundedReceiver<ScanMessage>,
    scan_tx: tokio_mpsc::UnboundedSender<ScanMessage>,
    watch_rx: tokio_mpsc::UnboundedReceiver<()>,
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
    last_mpris_signature: Option<MediaSessionSignature>,
    last_mpris_position_signature: Option<(u64, u64)>,
    last_discord_signature: Option<(Option<u64>, PlaybackStatus, u64, u64)>,
    theme: UiTheme,
    #[cfg(unix)]
    theme_path: Option<PathBuf>,
    #[cfg(unix)]
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
    let (scan_tx, scan_rx) = tokio_mpsc::unbounded_channel();
    let (watch_tx, watch_rx) = tokio_mpsc::unbounded_channel();
    let (gain_tx, gain_rx) = tokio_mpsc::unbounded_channel();
    start_watchers(&config, watch_tx);
    let discord = if config.discord_enabled {
        config.discord_application_id.clone().map(|application_id| {
            DiscordPresence::start(application_id, config.discord_large_image.clone())
        })
    } else {
        None
    };
    let compact = compact_requested || config.compact_default;
    #[cfg(unix)]
    let (theme, theme_path, theme_modified) = UiTheme::load_with_source();
    #[cfg(windows)]
    let theme = UiTheme::default();
    let (cover_decode_tx, cover_decode_requests) = mpsc::channel();
    let (cover_results_tx, cover_decode_rx) = tokio_mpsc::unbounded_channel();
    start_cover_decode_worker(cover_decode_requests, cover_results_tx);
    let (search_tx, search_requests) = mpsc::channel();
    let (search_results_tx, search_rx) = tokio_mpsc::unbounded_channel();
    start_search_worker(search_requests, search_results_tx);

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
        search_tx,
        search_rx,
        search_generation: 0,
        input: None,
        input_buffer: String::new(),
        queue: saved.queue,
        queue_index: saved.current_index,
        queue_dirty: false,
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
        gain_tx,
        gain_rx,
        gain_running: false,
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
        #[cfg(unix)]
        theme_path,
        #[cfg(unix)]
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
    let queue_len = app.queue.len();
    app.queue.retain(|id| app.track_index.contains_key(id));
    app.queue_dirty |= app.queue.len() != queue_len;
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
            let maintenance_delay = if app.playback.status == PlaybackStatus::Playing {
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
                message = app.scan_rx.recv() => {
                    if let Some(message) = message {
                        app.handle_scan(message)?;
                    }
                }
                message = app.gain_rx.recv() => {
                    if let Some(message) = message {
                        app.handle_gain_message(message)?;
                    }
                }
                changed = app.watch_rx.recv() => {
                    if changed.is_some() {
                        app.scan_pending = true;
                    }
                }
                result = app.cover_decode_rx.recv() => {
                    if let Some(result) = result {
                        app.handle_cover_decode_result(result);
                    }
                }
                result = app.search_rx.recv() => {
                    if let Some(result) = result {
                        app.handle_search_result(result);
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
            while let Ok(message) = app.gain_rx.try_recv() {
                app.handle_gain_message(message)?;
            }
            if app.watch_rx.try_recv().is_ok() {
                while app.watch_rx.try_recv().is_ok() {}
                app.scan_pending = true;
            }
            while let Ok(result) = app.search_rx.try_recv() {
                app.handle_search_result(result);
            }
            app.tick_history()?;
            app.refresh_theme();
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
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_matches.clear();
        if self.query.trim().is_empty() {
            self.dirty = true;
            return;
        }
        let request = SearchRequest {
            generation: self.search_generation,
            query: self.query.clone(),
            index: self.search_index.clone(),
        };
        if self.search_tx.send(request).is_err() {
            self.search_matches = self.search_index.search(&self.query, 100);
        }
        self.dirty = true;
    }

    fn handle_search_result(&mut self, result: SearchResult) {
        if result.generation != self.search_generation {
            return;
        }
        self.search_matches = result.matches;
        if self.view == View::Search {
            self.selected = self
                .selected
                .min(self.search_matches.len().saturating_sub(1));
        }
        self.dirty = true;
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
        self.queue_dirty = true;
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
}

fn resize_terminal_for_mode(compact: bool) -> Result<()> {
    // El tamaño real de la ventana lo decide esto, no la regla de Hyprland: al
    // pedir N celdas, el terminal se redimensiona. En una pantalla de 1920x1080
    // con celdas de ~10x18 px, 180x52 ocupa unos 1790x960 px.
    let (columns, rows) = if compact { (95, 32) } else { (180, 52) };
    crossterm::execute!(std::io::stdout(), SetSize(columns, rows))?;
    Ok(())
}

mod covers;
mod input;
mod library;
mod playback;
mod render;
mod theme;
mod workers;

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
    use super::render::track_viewport;
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
