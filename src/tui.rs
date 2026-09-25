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

use covers::Covers;
use input::{display_rule_value, handle_terminal_event};
use library::{LibrarySnapshot, PendingScan};
use nav::{NavFrame, NavTarget};
use playback::HistoryTally;
use render::draw;
use settings::SETTINGS;
use theme::{ThemeChoice, UiTheme};
use workers::{
    CoverDecodeRequest, CoverDecodeResult, ScanMessage, SearchRequest, SearchResult, WatchEvent,
    start_cover_decode_worker, start_library_worker, start_search_worker, start_shutdown_listener,
    start_terminal_event_reader, start_watchers,
};

use crate::{
    audio::{AudioBackend, MpvPlayer, dsp::Settings as DspSettings, hybrid::HybridPlayer},
    config::{AudioBackendChoice, Config, ReplayGainMode, all_sources},
    control::{ControlServer, RemoteCommand},
    db::{Database, HistoryUpdate, group_albums, group_artists},
    discord::DiscordPresence,
    features::{Genre, SearchIndex, evaluate_smart_playlist, group_genres},
    instance::InstanceGuard,
    library::{prune_cover_cache, prune_unreferenced_covers},
    model::{
        Album, Artist, HistoryEntry, PlaybackState, PlaybackStatus, PlayerAction, PlayerEvent,
        Playlist, RepeatMode, SavedPlayback, SavedQueue, SmartPlaylist, SmartRule, Track,
        TrackStats,
    },
    mpris::MprisBridge,
    paths::AppPaths,
    replaygain::{self, GainMessage},
    t,
};

const VIEWS: [View; 14] = [
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
    View::Lyrics,
    View::Settings,
    View::Help,
];

/// The context menu. Paired with its label rather than addressed by a bare
/// index, so reordering the menu cannot silently reassign what each entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextAction {
    PlayNow,
    PlayNext,
    Enqueue,
    ToggleFavorite,
    AddToPlaylist,
    ShowAlbum,
    ShowArtist,
}

/// Paired with a translation key rather than a label; see ContextAction.
const CONTEXT_ACTIONS: [(ContextAction, &str); 7] = [
    (ContextAction::PlayNow, "context.play_now"),
    (ContextAction::PlayNext, "context.play_next"),
    (ContextAction::Enqueue, "context.enqueue"),
    (ContextAction::ToggleFavorite, "context.favorite"),
    (ContextAction::AddToPlaylist, "context.add_to_playlist"),
    (ContextAction::ShowAlbum, "context.show_album"),
    (ContextAction::ShowArtist, "context.show_artist"),
];

/// Help for keys the binding table cannot describe: the smart-playlist editor
/// runs its own modal loop, and the Omarchy hotkeys belong to Hyprland.
const EXTRA_HELP: [(&str, &str); 2] = [
    ("help.editor.title", "help.editor.body"),
    ("help.omarchy.title", "help.omarchy.body"),
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
    Lyrics,
}

impl View {
    fn title(self) -> &'static str {
        t!(match self {
            Self::Home => "view.home",
            Self::Albums => "view.albums",
            Self::AlbumDetail => "view.album",
            Self::Artists => "view.artists",
            Self::ArtistDetail => "view.artist",
            Self::Genres => "view.genres",
            Self::GenreDetail => "view.genre",
            Self::Tracks => "view.tracks",
            Self::Playlists => "view.playlists",
            Self::SmartPlaylists => "view.smart_playlists",
            Self::SmartPlaylistDetail => "view.smart_playlist",
            Self::Favorites => "view.favorites",
            Self::History => "view.history",
            Self::Search => "view.search",
            Self::Queue => "view.queue",
            Self::Settings => "view.settings",
            Self::Help => "view.help",
            Self::Lyrics => "view.lyrics",
        })
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
            Self::Lyrics => "󰲹",
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
    /// Open detail views, outermost first. Empty means a top-level view.
    nav: Vec<NavFrame>,
    /// Albums and singles of the innermost open artist; derived from `nav`.
    artist_release_keys: Vec<String>,
    genre_tab: usize,
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
    /// Queue index of the track handed to mpv to play next, when one is armed.
    prefetched: Option<usize>,
    shuffle: bool,
    repeat: RepeatMode,
    playback: PlaybackState,
    muted_volume: Option<f64>,
    compact: bool,
    player: Box<dyn AudioBackend>,
    player_events: tokio_mpsc::UnboundedReceiver<PlayerEvent>,
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
    watch_rx: tokio_mpsc::UnboundedReceiver<WatchEvent>,
    scan_running: bool,
    scan_pending: PendingScan,
    reload_tx: Sender<()>,
    reload_rx: tokio_mpsc::UnboundedReceiver<std::result::Result<Box<LibrarySnapshot>, String>>,
    /// A background library rebuild is in flight.
    reload_running: bool,
    /// Something changed while a rebuild was running, so run one more.
    reload_again: bool,
    last_scan: Instant,
    status: String,
    should_quit: bool,
    dirty: bool,
    covers: Covers,
    /// Lyrics for the loaded track, and which track they were loaded for.
    lyrics: Option<crate::lyrics::Lyrics>,
    lyrics_track: Option<String>,
    /// Missing lyrics are retried at a low rate so CLI imports appear live.
    last_lyrics_check: Instant,
    /// Key bindings in effect: the defaults with any user overrides applied.
    bindings: Vec<keys::Binding>,
    album_columns: usize,
    last_mpris_signature: Option<MediaSessionSignature>,
    last_mpris_position_signature: Option<(u64, u64)>,
    last_discord_signature: Option<(Option<u64>, PlaybackStatus, u64, u64)>,
    theme: UiTheme,
    /// A palette asked for with `--theme`, which wins over the configured one
    /// until the setting is changed from inside the interface.
    theme_override: Option<ThemeChoice>,
    /// The file the palette was read from, when it came from one. Only the
    /// `system` choice has one, and it is what `refresh_theme` watches.
    theme_path: Option<PathBuf>,
    theme_modified: Option<SystemTime>,
    /// Listening accounting for the loaded track.
    tally: HistoryTally,
    last_playback_save: Instant,
    last_theme_check: Instant,
}

pub async fn run(
    paths: AppPaths,
    config: Config,
    compact: bool,
    theme: Option<String>,
) -> Result<()> {
    let _guard = InstanceGuard::acquire(&paths.lock_file())?;
    let (picker, protocol_note) = match Picker::from_query_stdio() {
        Ok(picker) => {
            let note = t!(
                "label.image_protocol",
                protocol = format!("{:?}", picker.protocol_type())
            );
            (picker, note)
        }
        Err(error) => (
            Picker::halfblocks(),
            t!("label.image_protocol_fallback", error = error),
        ),
    };
    let _ = fs::write(paths.image_protocol_file(), protocol_note);
    let mut terminal = ratatui::init();
    if let Err(error) = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)
    {
        let _ = ratatui::try_restore();
        return Err(error.into());
    }
    let result = run_inner(&mut terminal, paths, config, picker, compact, theme).await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

/// Build the backend the configuration asks for.
///
/// A native backend that will not start falls back to mpv with the reason in
/// the status bar. The alternative is refusing to run at all because a sound
/// card is busy, which is not a trade anyone would choose.
fn start_player(
    paths: &AppPaths,
    config: &Config,
    volume: f64,
    events: tokio_mpsc::UnboundedSender<PlayerEvent>,
) -> Result<(Box<dyn AudioBackend>, Option<String>)> {
    if config.audio_backend == AudioBackendChoice::Native {
        let settings = DspSettings {
            volume,
            equalizer: config.equalizer_bands(),
            bit_perfect: config.bit_perfect,
            ..DspSettings::default()
        };
        let device = Some(config.audio_device.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        // The hybrid, not the native backend alone: a library with an Opus
        // file in it would otherwise skip past it, and the setting the
        // listener chose was "play this well", not "play some of this".
        match HybridPlayer::start(device, settings, &paths.mpv_socket(), events.clone()) {
            Ok(player) => return Ok((Box::new(player), None)),
            Err(error) => {
                return Ok((
                    Box::new(MpvPlayer::start(&paths.mpv_socket(), events)?),
                    Some(t!("status.native_unavailable", error = error)),
                ));
            }
        }
    }
    Ok((
        Box::new(MpvPlayer::start(&paths.mpv_socket(), events)?),
        None,
    ))
}

async fn run_inner(
    terminal: &mut ratatui::DefaultTerminal,
    paths: AppPaths,
    config: Config,
    picker: Picker,
    compact_requested: bool,
    theme_requested: Option<String>,
) -> Result<()> {
    let db = Database::open(&paths.database_file())?;
    let saved = db.load_playback()?;
    let (player_events_tx, player_events) = tokio_mpsc::unbounded_channel();
    // Sessions saved before the control had a curve hold an amplitude; read
    // as a position it would come back far quieter than it was left.
    let saved_volume = if saved.volume_is_position {
        saved.volume.clamp(0.0, 1.0)
    } else {
        config.volume_position(saved.volume)
    };
    let (mut player, backend_warning) = start_player(
        &paths,
        &config,
        config.volume_gain(saved_volume),
        player_events_tx,
    )?;
    let (control_server, remote_actions) = ControlServer::start(&paths.control_socket())?;
    player.set_volume(config.volume_gain(saved_volume))?;
    player.set_equalizer(&config.equalizer_bands())?;
    let (action_tx, actions) = tokio_mpsc::unbounded_channel();
    let (mpris, mpris_warning) = match MprisBridge::new(action_tx).await {
        Ok(bridge) => (Some(bridge), None),
        Err(error) => (None, Some(t!("status.mpris_unavailable", error = error))),
    };
    let (scan_tx, scan_rx) = tokio_mpsc::unbounded_channel();
    let (watch_tx, watch_rx) = tokio_mpsc::unbounded_channel();
    let (gain_tx, gain_rx) = tokio_mpsc::unbounded_channel();
    start_watchers(&config, watch_tx);
    let discord = if config.discord_enabled {
        Some(DiscordPresence::start(config.discord_large_image.clone()))
    } else {
        None
    };
    let compact = compact_requested || config.compact_default;
    // `--theme` applies to this run only; the configured theme is left alone
    // until the setting is changed in the settings view.
    let theme_override = theme_requested.as_deref().and_then(ThemeChoice::from_name);
    let (theme, theme_path, theme_modified) = UiTheme::load(
        theme_override.unwrap_or_else(|| ThemeChoice::parse_or_default(&config.theme)),
    );
    let (cover_decode_tx, cover_decode_requests) = mpsc::channel();
    let (cover_results_tx, cover_decode_rx) = tokio_mpsc::unbounded_channel();
    start_cover_decode_worker(cover_decode_requests, cover_results_tx);
    let (reload_tx, reload_requests) = mpsc::channel();
    let (reload_results_tx, reload_rx) = tokio_mpsc::unbounded_channel();
    start_library_worker(paths.database_file(), reload_requests, reload_results_tx);
    let overrides = keys::KeyOverrides::load(&paths.keybindings_file());
    let binding_problems = overrides.problems.clone();
    let bindings = keys::effective_bindings(&overrides);
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
        nav: Vec::new(),
        artist_release_keys: Vec::new(),
        genre_tab: 0,
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
        prefetched: None,
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
        player,
        player_events,
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
        scan_pending: PendingScan::default(),
        reload_tx,
        reload_rx,
        reload_running: false,
        reload_again: false,
        last_scan: Instant::now() - Duration::from_secs(5),
        status: binding_problems
            .first()
            .map(|problem| t!("status.keybindings_problem", problem = problem))
            .or(backend_warning)
            .or(mpris_warning)
            .unwrap_or_else(|| t!("status.loading_library").into()),
        should_quit: false,
        dirty: true,
        bindings,
        lyrics: None,
        lyrics_track: None,
        last_lyrics_check: Instant::now() - Duration::from_secs(1),
        covers: Covers {
            picker,
            current: None,
            drawn_signature: 0,
            pending_signature: 0,
            grid: HashMap::new(),
            grid_order: VecDeque::new(),
            requests: cover_decode_tx,
            results: cover_decode_rx,
            in_flight: HashSet::new(),
        },
        album_columns: 1,
        last_mpris_signature: None,
        last_mpris_position_signature: None,
        last_discord_signature: None,
        theme,
        theme_override,
        theme_path,
        theme_modified,
        tally: HistoryTally::new(),
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
        app.player.pause(true)?;
        app.playback.status = PlaybackStatus::Paused;
        app.status = t!(
            "status.session_restored",
            title = track.title,
            artist = track.artist
        );
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
                event = app.player_events.recv() => {
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
                    if let Some(event) = changed {
                        app.scan_pending.record(event);
                    }
                }
                result = app.covers.results.recv() => {
                    if let Some(result) = result {
                        app.handle_cover_decode_result(result);
                    }
                }
                result = app.search_rx.recv() => {
                    if let Some(result) = result {
                        app.handle_search_result(result);
                    }
                }
                snapshot = app.reload_rx.recv() => {
                    if let Some(result) = snapshot {
                        app.handle_reload_result(result);
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

            app.playback.position_ms = app.player.position_ms();
            while let Ok(event) = terminal_events.try_recv() {
                handle_terminal_event(&mut app, event)?;
            }
            while let Ok(event) = app.player_events.try_recv() {
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
            while let Ok(event) = app.watch_rx.try_recv() {
                app.scan_pending.record(event);
            }
            while let Ok(result) = app.search_rx.try_recv() {
                app.handle_search_result(result);
            }
            while let Ok(result) = app.reload_rx.try_recv() {
                app.handle_reload_result(result);
            }
            app.tick_history()?;
            app.refresh_theme();
            if !app.scan_pending.is_empty()
                && !app.scan_running
                && app.last_scan.elapsed() > Duration::from_secs(2)
            {
                let pending = app.scan_pending.take();
                app.start_pending_scan(pending);
            }

            let periodic_draw_due = app.playback.status == PlaybackStatus::Playing
                && last_draw.elapsed() >= Duration::from_millis(250);
            // Re-armed here rather than at every queue mutation; see
            // sync_prefetch.
            app.sync_prefetch()?;
            app.sync_lyrics();

            if app.dirty || periodic_draw_due {
                app.refresh_cover();
                terminal.draw(|frame| draw(frame, &mut app))?;
                if app.covers.pending_signature != app.covers.drawn_signature {
                    app.covers.drawn_signature = app.covers.pending_signature;
                    // Kitty's unicode-placeholder placements disappear with
                    // their cells, and halfblocks are ordinary terminal cells.
                    // Forcing a terminal resize for either protocol causes a
                    // visible flash while scrolling the album grid. Sixel and
                    // iTerm2 graphics still need the stronger full repaint.
                    if cover_layout_requires_full_repaint(app.covers.picker.protocol_type()) {
                        // Terminal::clear queries the cursor first and can
                        // stall on some terminals. Resizing the current
                        // viewport clears both buffers without that query.
                        let area = terminal.size()?;
                        terminal.resize(area.into())?;
                        terminal.draw(|frame| draw(frame, &mut app))?;
                    }
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
            // The lyrics view has no selectable list of its own; it follows
            // whatever is playing.
            View::Lyrics => 0,
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
            View::Lyrics => Vec::new(),
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
            View::Lyrics => self.current_track().map(|track| track.id.clone()),
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
        let name = self.opened_genre_name()?;
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
        self.opened_genre_name()
            .and_then(|name| self.genre_album_cache.get(name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn genre_artist_indices(&self) -> &[usize] {
        self.opened_genre_name()
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
        self.opened_smart_playlist()
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
        let key = self.opened_album_key()?;
        let index = self.album_index.get(key).copied()?;
        self.albums.get(index)
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
        let Some(name) = self.opened_artist_name().map(str::to_owned) else {
            self.artist_release_keys.clear();
            return;
        };
        let Some(artist) = self
            .artist_index
            .get(name.as_str())
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
        let name = artist.name.clone();
        self.push_nav(NavTarget::Artist(name), View::ArtistDetail);
        self.refresh_artist_releases();
        self.status = t!("status.pick_release").into();
    }

    fn close_artist_detail(&mut self) {
        if self.view != View::ArtistDetail {
            return;
        }
        let Some(frame) = self.pop_nav() else {
            return;
        };
        let NavTarget::Artist(name) = frame.target else {
            return;
        };
        // Prefer re-finding the artist over the remembered index: a rescan may
        // have shifted the list while the detail view was open.
        self.selected = if frame.parent == View::GenreDetail {
            self.genre_artist_indices()
                .iter()
                .position(|index| self.artists[*index].name == name)
        } else {
            self.artist_index.get(name.as_str()).copied()
        }
        .unwrap_or(frame.selection)
        .min(self.artists.len().saturating_sub(1));
        // An artist may still be open below this one.
        self.refresh_artist_releases();
        self.status.clear();
    }

    fn open_selected_album(&mut self) {
        let Some(album_key) = self.selected_album().map(|album| album.key.clone()) else {
            return;
        };
        self.push_nav(NavTarget::Album(album_key), View::AlbumDetail);
        self.status = t!("status.pick_track").into();
    }

    fn close_album_detail(&mut self) {
        if self.view != View::AlbumDetail {
            return;
        }
        let Some(frame) = self.pop_nav() else {
            return;
        };
        let NavTarget::Album(key) = frame.target else {
            return;
        };
        self.selected = match frame.parent {
            View::ArtistDetail => self
                .artist_release_keys
                .iter()
                .position(|album_key| album_key == &key),
            View::GenreDetail => self
                .genre_album_indices()
                .iter()
                .position(|index| self.albums[*index].key == key),
            _ => self.albums.iter().position(|album| album.key == key),
        }
        .unwrap_or(frame.selection)
        .min(self.item_count().saturating_sub(1));
        self.status.clear();
    }

    fn activate_or_open(&mut self) -> Result<()> {
        if self.view == View::Artists {
            self.open_selected_artist();
            Ok(())
        } else if matches!(self.view, View::Albums | View::ArtistDetail) {
            self.open_selected_album();
            Ok(())
        } else if self.view == View::Genres {
            if let Some(name) = self
                .genres
                .get(self.selected)
                .map(|genre| genre.name.clone())
            {
                self.push_nav(NavTarget::Genre(name), View::GenreDetail);
                self.genre_tab = 0;
            }
            Ok(())
        } else if self.view == View::GenreDetail && self.genre_tab == 0 {
            if let Some(index) = self.genre_album_indices().get(self.selected).copied() {
                let key = self.albums[index].key.clone();
                self.push_nav(NavTarget::Album(key), View::AlbumDetail);
            }
            Ok(())
        } else if self.view == View::GenreDetail && self.genre_tab == 1 {
            if let Some(index) = self.genre_artist_indices().get(self.selected).copied() {
                let name = self.artists[index].name.clone();
                self.push_nav(NavTarget::Artist(name), View::ArtistDetail);
                self.refresh_artist_releases();
            }
            Ok(())
        } else if self.view == View::SmartPlaylists {
            if let Some(id) = self.smart_playlists.get(self.selected).map(|list| list.id) {
                self.push_nav(NavTarget::SmartPlaylist(id), View::SmartPlaylistDetail);
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
                View::AlbumDetail if self.nav_parent() == Some(View::ArtistDetail) => View::Artists,
                View::AlbumDetail => View::Albums,
                view => view,
            };
            let current = VIEWS.iter().position(|v| *v == sidebar_view).unwrap_or(0);
            let next = (current as isize + amount).clamp(0, VIEWS.len() as isize - 1) as usize;
            self.view = VIEWS[next];
            self.clear_nav();
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
            self.status = t!("status.nothing_playable").into();
            return Ok(());
        }
        let target = match self.view {
            View::Albums
            | View::Artists
            | View::Genres
            | View::ArtistDetail
            | View::Playlists
            | View::SmartPlaylists => None,
            View::GenreDetail if self.genre_tab != 2 => None,
            _ => self.selected_track_id(),
        };

        let start = if self.shuffle {
            let mut rng = rand::rng();
            prepare_shuffled_queue(&mut ids, target.as_ref(), &mut rng)
        } else {
            target
                .as_ref()
                .and_then(|id| ids.iter().position(|candidate| candidate == id))
                .unwrap_or(0)
        };
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

fn prepare_shuffled_queue<T: PartialEq>(
    queue: &mut [T],
    target: Option<&T>,
    rng: &mut impl rand::Rng,
) -> usize {
    queue.shuffle(rng);

    if let Some(target) = target
        && let Some(index) = queue.iter().position(|candidate| candidate == target)
    {
        queue.swap(0, index);
    }

    0
}

fn cover_layout_requires_full_repaint(protocol: ratatui_image::picker::ProtocolType) -> bool {
    matches!(
        protocol,
        ratatui_image::picker::ProtocolType::Sixel | ratatui_image::picker::ProtocolType::Iterm2
    )
}

fn resize_terminal_for_mode(compact: bool) -> Result<()> {
    // This, not the Hyprland rule, is what decides the real window size:
    // asking for N cells makes the terminal resize. On a 1920x1080 screen with
    // roughly 10x18 px cells, 180x52 comes to about 1790x960 px.
    let (columns, rows) = if compact { (95, 32) } else { (180, 52) };
    crossterm::execute!(std::io::stdout(), SetSize(columns, rows))?;
    Ok(())
}

mod covers;
mod input;
pub mod keys;
mod library;
mod nav;
mod playback;
mod render;
mod settings;
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
    Paragraph::new(t!("empty.library"))
        .alignment(Alignment::Center)
        .style(Style::default().fg(theme.muted))
}

fn format_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::render::track_viewport;
    use super::*;
    use rand::{SeedableRng, rngs::StdRng};

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
    fn cover_layout_repaint_skips_cell_based_protocols() {
        use ratatui_image::picker::ProtocolType;

        assert!(!cover_layout_requires_full_repaint(ProtocolType::Kitty));
        assert!(!cover_layout_requires_full_repaint(
            ProtocolType::Halfblocks
        ));
        assert!(cover_layout_requires_full_repaint(ProtocolType::Sixel));
        assert!(cover_layout_requires_full_repaint(ProtocolType::Iterm2));
    }

    #[test]
    fn grid_navigation_moves_by_columns_and_clamps() {
        assert_eq!(shifted_index(2, 4, 10), 6);
        assert_eq!(shifted_index(2, -4, 10), 0);
        assert_eq!(shifted_index(8, 4, 10), 9);
        assert_eq!(shifted_index(0, 1, 0), 0);
    }

    #[test]
    fn a_new_shuffled_group_starts_at_zero_and_keeps_every_track() {
        let mut queue = vec!["a", "b", "c", "d", "e", "f"];
        let original = queue.clone();
        let mut rng = StdRng::seed_from_u64(42);

        let start = prepare_shuffled_queue(&mut queue, None, &mut rng);

        assert_eq!(start, 0);
        assert_ne!(queue, original);

        let mut actual = queue;
        let mut expected = original;
        actual.sort_unstable();
        expected.sort_unstable();

        assert_eq!(actual, expected);
    }

    #[test]
    fn a_selected_track_stays_first_in_a_new_shuffled_queue() {
        let mut queue = vec!["a", "b", "c", "d", "e", "f"];
        let original = queue.clone();
        let target = "c";
        let mut rng = StdRng::seed_from_u64(42);

        let start = prepare_shuffled_queue(&mut queue, Some(&target), &mut rng);

        assert_eq!(start, 0);
        assert_eq!(queue.first(), Some(&target));

        let mut actual = queue;
        let mut expected = original;
        actual.sort_unstable();
        expected.sort_unstable();

        assert_eq!(actual, expected);
    }
}
