//! Key bindings and the pure resolution from a key press to an action.
//!
//! Splitting resolution from execution buys two things. Resolution becomes
//! testable without constructing an `App` (which drags in a database, mpv and
//! four threads), and the bindings become data that a user configuration and
//! the help screen can both be derived from.
//!
//! Order in `BINDINGS` is significant: the first match wins, so specific
//! scopes must precede `Anywhere`. `Left` is the clearest case — it closes an
//! album, or moves within the album grid, or focuses the sidebar, depending on
//! where you are.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{Focus, View};
use crate::t;
use crate::{control::RemoteCommand, model::PlayerAction};

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Anywhere,
    View(View),
    /// The album grid: a grid-shaped view with the content pane focused.
    AlbumGrid,
}

impl Scope {
    pub(super) fn matches(self, view: View, focus: Focus) -> bool {
        match self {
            Self::Anywhere => true,
            Self::View(expected) => view == expected,
            Self::AlbumGrid => {
                matches!(view, View::Albums | View::ArtistDetail) && focus == Focus::Content
            }
        }
    }
}

/// How a settings row was nudged. The settings view reads horizontal movement
/// as "adjust this value" and a plain activation as "toggle it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingInput {
    Decrease,
    Increase,
    Toggle,
}

impl SettingInput {
    pub(super) fn is_horizontal(self) -> bool {
        matches!(self, Self::Decrease | Self::Increase)
    }

    pub(super) fn increases(self) -> bool {
        matches!(self, Self::Increase)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Action {
    Quit,
    /// Leave the current detail view. A no-op anywhere else.
    Back,
    OpenView(View),
    ToggleCompact,
    OpenSearch,
    NewPlaylist,
    AddSelectedToPlaylist,
    NextGenreTab,
    ToggleFocus,
    FocusSidebar,
    FocusContent,
    MoveSelection(isize),
    /// Move within the album grid by whole items.
    AlbumStep(isize),
    /// Move within the album grid by whole rows; scaled by the column count.
    AlbumRow(isize),
    SelectFirst,
    SelectLast,
    Activate,
    OpenContextMenu,
    QueueMove(isize),
    QueueRemove,
    QueueClear,
    QueueSave,
    QueueLoad,
    EditSmartPlaylist,
    Player(PlayerAction),
    ToggleShuffle,
    CycleRepeat,
    EnqueueSelected,
    ToggleFavorite,
    Remote(RemoteCommand),
    Setting(SettingInput),
}

pub struct Binding {
    pub(super) code: KeyCode,
    pub(super) mods: KeyModifiers,
    pub(super) scope: Scope,
    pub(super) action: Action,
}

const fn bind(code: KeyCode, scope: Scope, action: Action) -> Binding {
    Binding {
        code,
        mods: KeyModifiers::NONE,
        scope,
        action,
    }
}

const fn ctrl(code: KeyCode, scope: Scope, action: Action) -> Binding {
    Binding {
        code,
        mods: KeyModifiers::CONTROL,
        scope,
        action,
    }
}

use Action as A;
use Scope::{AlbumGrid, Anywhere, View as In};

pub(super) const BINDINGS: &[Binding] = &[
    ctrl(KeyCode::Char('c'), Anywhere, A::Quit),
    bind(KeyCode::Char('q'), Anywhere, A::Quit),
    bind(KeyCode::Esc, Anywhere, A::Back),
    bind(KeyCode::Char('?'), Anywhere, A::OpenView(View::Help)),
    bind(KeyCode::Char(','), Anywhere, A::OpenView(View::Settings)),
    bind(KeyCode::Char('m'), Anywhere, A::ToggleCompact),
    bind(KeyCode::Char('/'), Anywhere, A::OpenSearch),
    bind(KeyCode::Char('c'), Anywhere, A::NewPlaylist),
    bind(KeyCode::Char('P'), Anywhere, A::AddSelectedToPlaylist),
    bind(KeyCode::Tab, In(View::GenreDetail), A::NextGenreTab),
    bind(KeyCode::Tab, Anywhere, A::ToggleFocus),
    bind(
        KeyCode::Char('['),
        Anywhere,
        A::Player(PlayerAction::SeekRelative(-5_000)),
    ),
    bind(
        KeyCode::Char(']'),
        Anywhere,
        A::Player(PlayerAction::SeekRelative(5_000)),
    ),
    // The settings view claims the horizontal keys and space before the
    // navigation and playback bindings below can see them.
    bind(
        KeyCode::Left,
        In(View::Settings),
        A::Setting(SettingInput::Decrease),
    ),
    bind(
        KeyCode::Right,
        In(View::Settings),
        A::Setting(SettingInput::Increase),
    ),
    bind(
        KeyCode::Char(' '),
        In(View::Settings),
        A::Setting(SettingInput::Toggle),
    ),
    bind(
        KeyCode::Enter,
        In(View::Settings),
        A::Setting(SettingInput::Toggle),
    ),
    bind(KeyCode::Left, In(View::AlbumDetail), A::Back),
    bind(KeyCode::Char('h'), In(View::AlbumDetail), A::Back),
    bind(KeyCode::Left, AlbumGrid, A::AlbumStep(-1)),
    bind(KeyCode::Char('h'), AlbumGrid, A::AlbumStep(-1)),
    bind(KeyCode::Left, Anywhere, A::FocusSidebar),
    bind(KeyCode::Char('h'), Anywhere, A::FocusSidebar),
    bind(KeyCode::Right, AlbumGrid, A::AlbumStep(1)),
    bind(KeyCode::Char('l'), AlbumGrid, A::AlbumStep(1)),
    bind(KeyCode::Right, Anywhere, A::FocusContent),
    bind(KeyCode::Char('l'), Anywhere, A::FocusContent),
    bind(KeyCode::Up, AlbumGrid, A::AlbumRow(-1)),
    bind(KeyCode::Char('k'), AlbumGrid, A::AlbumRow(-1)),
    bind(KeyCode::Down, AlbumGrid, A::AlbumRow(1)),
    bind(KeyCode::Char('j'), AlbumGrid, A::AlbumRow(1)),
    bind(KeyCode::Up, Anywhere, A::MoveSelection(-1)),
    bind(KeyCode::Char('k'), Anywhere, A::MoveSelection(-1)),
    bind(KeyCode::Down, Anywhere, A::MoveSelection(1)),
    bind(KeyCode::Char('j'), Anywhere, A::MoveSelection(1)),
    bind(KeyCode::Home, Anywhere, A::SelectFirst),
    bind(KeyCode::End, Anywhere, A::SelectLast),
    bind(KeyCode::Enter, Anywhere, A::Activate),
    bind(KeyCode::Char('x'), Anywhere, A::OpenContextMenu),
    bind(KeyCode::Char('J'), In(View::Queue), A::QueueMove(1)),
    bind(KeyCode::Char('K'), In(View::Queue), A::QueueMove(-1)),
    bind(KeyCode::Delete, In(View::Queue), A::QueueRemove),
    bind(KeyCode::Char('d'), In(View::Queue), A::QueueRemove),
    bind(KeyCode::Char('C'), In(View::Queue), A::QueueClear),
    bind(KeyCode::Char('S'), In(View::Queue), A::QueueSave),
    bind(KeyCode::Char('L'), In(View::Queue), A::QueueLoad),
    bind(
        KeyCode::Char('e'),
        In(View::SmartPlaylists),
        A::EditSmartPlaylist,
    ),
    bind(
        KeyCode::Char(' '),
        Anywhere,
        A::Player(PlayerAction::Toggle),
    ),
    bind(KeyCode::Char('n'), Anywhere, A::Player(PlayerAction::Next)),
    bind(
        KeyCode::Char('p'),
        Anywhere,
        A::Player(PlayerAction::Previous),
    ),
    bind(KeyCode::Char('s'), Anywhere, A::ToggleShuffle),
    bind(KeyCode::Char('r'), Anywhere, A::CycleRepeat),
    bind(KeyCode::Char('a'), Anywhere, A::EnqueueSelected),
    bind(KeyCode::Char('f'), Anywhere, A::ToggleFavorite),
    bind(
        KeyCode::Char('+'),
        Anywhere,
        A::Remote(RemoteCommand::VolumeUp),
    ),
    bind(
        KeyCode::Char('='),
        Anywhere,
        A::Remote(RemoteCommand::VolumeUp),
    ),
    bind(
        KeyCode::Char('-'),
        Anywhere,
        A::Remote(RemoteCommand::VolumeDown),
    ),
];

/// Shift is already encoded in the character itself, so a binding on `J` must
/// not also demand the modifier: terminals disagree about whether they report
/// it. Every other modifier is matched exactly, which is what stops `Ctrl+j`
/// from being treated as a bare `j`.
fn effective_modifiers(key: &KeyEvent) -> KeyModifiers {
    let mut mods = key.modifiers;
    if matches!(key.code, KeyCode::Char(_)) {
        mods.remove(KeyModifiers::SHIFT);
    }
    mods
}

/// Where a binding appears on the help screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Category {
    Navigation,
    Playback,
    Library,
    Queue,
    Windows,
}

impl Category {
    /// Translation key for the section heading.
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Navigation => "help.category.navigation",
            Self::Playback => "help.category.playback",
            Self::Library => "help.category.library",
            Self::Queue => "help.category.queue",
            Self::Windows => "help.category.windows",
        }
    }

    const ORDER: [Self; 5] = [
        Self::Navigation,
        Self::Playback,
        Self::Library,
        Self::Queue,
        Self::Windows,
    ];
}

impl Action {
    fn category(self) -> Category {
        match self {
            Self::MoveSelection(_)
            | Self::AlbumStep(_)
            | Self::AlbumRow(_)
            | Self::SelectFirst
            | Self::SelectLast
            | Self::Activate
            | Self::Back
            | Self::ToggleFocus
            | Self::FocusSidebar
            | Self::FocusContent
            | Self::NextGenreTab => Category::Navigation,
            Self::Player(_)
            | Self::ToggleShuffle
            | Self::CycleRepeat
            | Self::Remote(_)
            | Self::ToggleCompact => Category::Playback,
            Self::OpenSearch
            | Self::NewPlaylist
            | Self::AddSelectedToPlaylist
            | Self::EnqueueSelected
            | Self::ToggleFavorite
            | Self::OpenContextMenu
            | Self::EditSmartPlaylist => Category::Library,
            Self::QueueMove(_)
            | Self::QueueRemove
            | Self::QueueClear
            | Self::QueueSave
            | Self::QueueLoad => Category::Queue,
            Self::OpenView(_) | Self::Quit | Self::Setting(_) => Category::Windows,
        }
    }

    /// Translation key for the help screen.
    fn describe(self) -> &'static str {
        match self {
            Self::Quit => "action.quit",
            Self::Back => "action.back",
            Self::OpenView(View::Help) => "action.help",
            Self::OpenView(View::Settings) => "action.settings",
            Self::OpenView(_) => "action.open_view",
            Self::ToggleCompact => "action.compact",
            Self::OpenSearch => "action.search",
            Self::NewPlaylist => "action.new_playlist",
            Self::AddSelectedToPlaylist => "action.add_to_playlist",
            Self::NextGenreTab => "action.genre_tab",
            Self::ToggleFocus => "action.toggle_focus",
            Self::FocusSidebar => "action.focus_sidebar",
            Self::FocusContent => "action.focus_content",
            Self::MoveSelection(amount) if amount < 0 => "action.up",
            Self::MoveSelection(_) => "action.down",
            Self::AlbumStep(amount) if amount < 0 => "action.album_previous",
            Self::AlbumStep(_) => "action.album_next",
            Self::AlbumRow(amount) if amount < 0 => "action.row_previous",
            Self::AlbumRow(_) => "action.row_next",
            Self::SelectFirst => "action.first",
            Self::SelectLast => "action.last",
            Self::Activate => "action.activate",
            Self::OpenContextMenu => "action.context_menu",
            Self::QueueMove(amount) if amount < 0 => "action.queue_up",
            Self::QueueMove(_) => "action.queue_down",
            Self::QueueRemove => "action.queue_remove",
            Self::QueueClear => "action.queue_clear",
            Self::QueueSave => "action.queue_save",
            Self::QueueLoad => "action.queue_load",
            Self::EditSmartPlaylist => "action.edit_smart",
            Self::Player(PlayerAction::SeekRelative(ms)) if ms < 0 => "action.seek_backward",
            Self::Player(PlayerAction::SeekRelative(_)) => "action.seek_forward",
            Self::Player(PlayerAction::Toggle) => "action.play_pause",
            Self::Player(PlayerAction::Next) => "action.next_track",
            Self::Player(PlayerAction::Previous) => "action.previous_track",
            Self::Player(_) => "action.playback",
            Self::ToggleShuffle => "action.shuffle",
            Self::CycleRepeat => "action.repeat",
            Self::EnqueueSelected => "action.enqueue",
            Self::ToggleFavorite => "action.favorite",
            Self::Remote(RemoteCommand::VolumeUp) => "action.volume_up",
            Self::Remote(RemoteCommand::VolumeDown) => "action.volume_down",
            Self::Remote(_) => "action.volume",
            Self::Setting(SettingInput::Decrease) => "action.setting_decrease",
            Self::Setting(SettingInput::Increase) => "action.setting_increase",
            Self::Setting(SettingInput::Toggle) => "action.setting_toggle",
        }
    }
}

/// How a key is written on the help screen.
fn key_label(code: KeyCode, mods: KeyModifiers) -> String {
    let base = match code {
        KeyCode::Char(' ') => t!("key.space").to_owned(),
        // Uppercase bindings are reached with Shift, which is how people think
        // of them even though the table matches on the character.
        KeyCode::Char(character) if character.is_uppercase() => format!("Shift+{character}"),
        KeyCode::Char(character) => character.to_string(),
        KeyCode::Up => "↑".to_owned(),
        KeyCode::Down => "↓".to_owned(),
        KeyCode::Left => "←".to_owned(),
        KeyCode::Right => "→".to_owned(),
        KeyCode::Enter => t!("key.enter").to_owned(),
        KeyCode::Esc => t!("key.esc").to_owned(),
        KeyCode::Tab => t!("key.tab").to_owned(),
        KeyCode::Home => t!("key.home").to_owned(),
        KeyCode::End => t!("key.end").to_owned(),
        KeyCode::Delete => t!("key.delete").to_owned(),
        other => format!("{other:?}"),
    };
    if mods.contains(KeyModifiers::CONTROL) {
        format!("Ctrl+{base}")
    } else {
        base
    }
}

/// A scope worth mentioning next to a binding.
fn scope_hint(scope: Scope) -> Option<&'static str> {
    match scope {
        Scope::Anywhere => None,
        Scope::AlbumGrid => Some("help.scope.album_grid"),
        Scope::View(view) => Some(match view {
            View::Queue => "help.scope.queue",
            View::Settings => "help.scope.settings",
            View::SmartPlaylists => "help.scope.smart_playlists",
            View::GenreDetail => "help.scope.genre",
            View::AlbumDetail => "help.scope.album",
            _ => "help.scope.this_view",
        }),
    }
}

pub(super) struct HelpEntry {
    pub(super) keys: String,
    /// Translation keys, resolved when the help screen is drawn.
    pub(super) description: &'static str,
    pub(super) scope: Option<&'static str>,
}

/// The help screen, derived from [`BINDINGS`] so a binding cannot be added
/// without appearing in it.
pub(super) fn help_sections(bindings: &[Binding]) -> Vec<(&'static str, Vec<HelpEntry>)> {
    Category::ORDER
        .into_iter()
        .map(|category| {
            let mut entries: Vec<HelpEntry> = Vec::new();
            for binding in bindings {
                if binding.action.category() != category {
                    continue;
                }
                let description = binding.action.describe();
                let scope = scope_hint(binding.scope);
                let label = key_label(binding.code, binding.mods);
                // Several keys often drive one action (↑ and k); list them
                // together instead of repeating the row.
                match entries
                    .iter_mut()
                    .find(|entry| entry.description == description && entry.scope == scope)
                {
                    Some(entry) => {
                        entry.keys.push_str(" / ");
                        entry.keys.push_str(&label);
                    }
                    None => entries.push(HelpEntry {
                        keys: label,
                        description,
                        scope,
                    }),
                }
            }
            (category.title(), entries)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// User configuration
// ---------------------------------------------------------------------------

/// Names an action in `keybindings.toml`.
///
/// Stable text rather than the enum's Debug output, so renaming a variant
/// cannot silently invalidate everyone's configuration.
fn action_name(action: Action) -> &'static str {
    match action {
        Action::Quit => "quit",
        Action::Back => "back",
        Action::OpenView(View::Help) => "help",
        Action::OpenView(View::Settings) => "settings",
        Action::OpenView(_) => "open_view",
        Action::ToggleCompact => "compact",
        Action::OpenSearch => "search",
        Action::NewPlaylist => "new_playlist",
        Action::AddSelectedToPlaylist => "add_to_playlist",
        Action::NextGenreTab => "genre_tab",
        Action::ToggleFocus => "toggle_focus",
        Action::FocusSidebar => "focus_sidebar",
        Action::FocusContent => "focus_content",
        Action::MoveSelection(amount) if amount < 0 => "up",
        Action::MoveSelection(_) => "down",
        Action::AlbumStep(amount) if amount < 0 => "album_previous",
        Action::AlbumStep(_) => "album_next",
        Action::AlbumRow(amount) if amount < 0 => "row_previous",
        Action::AlbumRow(_) => "row_next",
        Action::SelectFirst => "first",
        Action::SelectLast => "last",
        Action::Activate => "activate",
        Action::OpenContextMenu => "context_menu",
        Action::QueueMove(amount) if amount < 0 => "queue_up",
        Action::QueueMove(_) => "queue_down",
        Action::QueueRemove => "queue_remove",
        Action::QueueClear => "queue_clear",
        Action::QueueSave => "queue_save",
        Action::QueueLoad => "queue_load",
        Action::EditSmartPlaylist => "edit_smart_playlist",
        Action::Player(PlayerAction::SeekRelative(ms)) if ms < 0 => "seek_backward",
        Action::Player(PlayerAction::SeekRelative(_)) => "seek_forward",
        Action::Player(PlayerAction::Toggle) => "play_pause",
        Action::Player(PlayerAction::Next) => "next_track",
        Action::Player(PlayerAction::Previous) => "previous_track",
        Action::Player(_) => "playback",
        Action::ToggleShuffle => "shuffle",
        Action::CycleRepeat => "repeat",
        Action::EnqueueSelected => "enqueue",
        Action::ToggleFavorite => "favorite",
        Action::Remote(RemoteCommand::VolumeUp) => "volume_up",
        Action::Remote(RemoteCommand::VolumeDown) => "volume_down",
        Action::Remote(_) => "volume",
        Action::Setting(SettingInput::Decrease) => "setting_decrease",
        Action::Setting(SettingInput::Increase) => "setting_increase",
        Action::Setting(SettingInput::Toggle) => "setting_toggle",
    }
}

/// Parse a binding written as `ctrl+s`, `shift+j`, `space` or `Home`.
///
/// Case is ignored for named keys but kept for characters, because an
/// uppercase letter is how an unshifted binding is distinguished from a
/// shifted one everywhere else in this module.
fn parse_binding(text: &str) -> Option<(KeyCode, KeyModifiers)> {
    let mut mods = KeyModifiers::NONE;
    let trimmed = text.trim();
    let (parts, key): (Vec<&str>, &str) = if trimmed == "+" {
        (Vec::new(), "+")
    } else if let Some(modifiers) = trimmed.strip_suffix("++") {
        (modifiers.split('+').map(str::trim).collect(), "+")
    } else {
        let mut parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
        let key = parts.pop()?;
        (parts, key)
    };
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let code = match key.to_ascii_lowercase().as_str() {
        "space" => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "delete" | "del" => KeyCode::Delete,
        "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        other if other.len() == 1 => {
            let character = key.chars().next()?;
            // A shift written out for a letter is folded into its case, which
            // is what resolve() compares against.
            if mods.contains(KeyModifiers::SHIFT) && character.is_alphabetic() {
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::Char(character.to_ascii_uppercase())
            } else {
                KeyCode::Char(character)
            }
        }
        function if function.starts_with('f') => KeyCode::F(function[1..].parse::<u8>().ok()?),
        _ => return None,
    };
    Some((code, mods))
}

/// A user's overrides, applied on top of the built-in bindings.
#[derive(Debug, Default)]
pub struct KeyOverrides {
    /// Replacement keys per action name; an action listed here loses its
    /// defaults entirely, so a rebind does not leave the old key working.
    bindings: HashMap<String, Vec<(KeyCode, KeyModifiers)>>,
    /// Entries that could not be understood, reported once at start-up.
    pub problems: Vec<String>,
}

impl KeyOverrides {
    /// Read `keybindings.toml`, if there is one.
    ///
    /// A broken file is never fatal: muscli is a music player, and refusing to
    /// start over a typo in an optional file would be the wrong trade. Problems
    /// are collected and surfaced instead.
    pub fn load(path: &std::path::Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let parsed: Result<HashMap<String, Vec<String>>, _> = toml::from_str(&raw);
        let mut overrides = Self::default();
        let entries = match parsed {
            Ok(entries) => entries,
            Err(error) => {
                overrides.problems.push(format!("{error}"));
                return overrides;
            }
        };
        let known: Vec<&str> = BINDINGS.iter().map(|b| action_name(b.action)).collect();
        for (action, keys) in entries {
            if !known.contains(&action.as_str()) {
                overrides.problems.push(format!("unknown action: {action}"));
                continue;
            }
            let explicitly_unbound = keys.is_empty();
            let mut parsed_keys = Vec::new();
            for key in keys {
                match parse_binding(&key) {
                    Some(binding) => parsed_keys.push(binding),
                    None => overrides
                        .problems
                        .push(format!("unrecognised key for {action}: {key}")),
                }
            }
            if explicitly_unbound || !parsed_keys.is_empty() {
                overrides.bindings.insert(action, parsed_keys);
            }
        }
        overrides
    }

    fn replacement(&self, action: Action) -> Option<&[(KeyCode, KeyModifiers)]> {
        self.bindings.get(action_name(action)).map(Vec::as_slice)
    }
}

/// The bindings in effect, defaults with any overrides applied.
///
/// Scope is not configurable: it is what makes `Left` mean three different
/// things, and letting it be redefined would turn a typo into an unusable
/// interface.
pub fn effective_bindings(overrides: &KeyOverrides) -> Vec<Binding> {
    BINDINGS
        .iter()
        .flat_map(|binding| match overrides.replacement(binding.action) {
            Some(keys) => keys
                .iter()
                .map(|(code, mods)| Binding {
                    code: *code,
                    mods: *mods,
                    scope: binding.scope,
                    action: binding.action,
                })
                .collect::<Vec<_>>(),
            None => vec![Binding {
                code: binding.code,
                mods: binding.mods,
                scope: binding.scope,
                action: binding.action,
            }],
        })
        // An action with several default keys would otherwise get its
        // replacement list repeated once per default.
        .fold(Vec::new(), |mut kept, binding| {
            if !kept.iter().any(|existing: &Binding| {
                existing.code == binding.code
                    && existing.mods == binding.mods
                    && existing.scope == binding.scope
            }) {
                kept.push(binding);
            }
            kept
        })
}

/// Resolve against a specific set of bindings.
pub(super) fn resolve_with(
    bindings: &[Binding],
    key: &KeyEvent,
    view: View,
    focus: Focus,
) -> Option<Action> {
    let mods = effective_modifiers(key);
    bindings
        .iter()
        .find(|binding| {
            binding.code == key.code && binding.mods == mods && binding.scope.matches(view, focus)
        })
        .map(|binding| binding.action)
}

/// Every action that can be rebound, with the keys currently bound to it.
pub fn binding_listing(bindings: &[Binding]) -> Vec<(&'static str, String)> {
    let mut listing: Vec<(&'static str, String)> = Vec::new();
    for binding in bindings {
        let name = action_name(binding.action);
        let label = key_label(binding.code, binding.mods);
        match listing.iter_mut().find(|(existing, _)| *existing == name) {
            Some((_, keys)) => {
                keys.push(' ');
                keys.push_str(&label);
            }
            None => listing.push((name, label)),
        }
    }
    listing.sort_by_key(|(name, _)| *name);
    listing
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in bindings, as the application sees them with no overrides.
    fn defaults() -> Vec<Binding> {
        effective_bindings(&KeyOverrides::default())
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_keys_resolve_anywhere() {
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char('q')),
                View::Home,
                Focus::Content
            ),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char('n')),
                View::Queue,
                Focus::Sidebar
            ),
            Some(Action::Player(PlayerAction::Next))
        );
    }

    #[test]
    fn a_modifier_the_binding_does_not_ask_for_is_not_ignored() {
        // Ctrl+j used to fall through to the bare `j` arm and move the
        // selection, because handle_key only ever matched on the key code.
        assert_eq!(
            resolve_with(
                &defaults(),
                &with(KeyCode::Char('j'), KeyModifiers::CONTROL),
                View::Home,
                Focus::Content
            ),
            None
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char('j')),
                View::Home,
                Focus::Content
            ),
            Some(Action::MoveSelection(1))
        );
    }

    #[test]
    fn control_c_quits_but_plain_c_starts_a_playlist() {
        assert_eq!(
            resolve_with(
                &defaults(),
                &with(KeyCode::Char('c'), KeyModifiers::CONTROL),
                View::Home,
                Focus::Content
            ),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char('c')),
                View::Home,
                Focus::Content
            ),
            Some(Action::NewPlaylist)
        );
    }

    #[test]
    fn shift_is_accepted_on_uppercase_bindings() {
        // Terminals disagree about reporting shift alongside an uppercase char.
        for mods in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                resolve_with(
                    &defaults(),
                    &with(KeyCode::Char('J'), mods),
                    View::Queue,
                    Focus::Content
                ),
                Some(Action::QueueMove(1)),
                "Shift handling must not depend on how the terminal reports it"
            );
        }
    }

    #[test]
    fn the_left_key_cascades_by_specificity() {
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Left),
                View::AlbumDetail,
                Focus::Content
            ),
            Some(Action::Back)
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Left),
                View::Albums,
                Focus::Content
            ),
            Some(Action::AlbumStep(-1))
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Left),
                View::Albums,
                Focus::Sidebar
            ),
            Some(Action::FocusSidebar),
            "without the content pane focused there is no grid to move in"
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Left),
                View::Tracks,
                Focus::Content
            ),
            Some(Action::FocusSidebar)
        );
    }

    #[test]
    fn vertical_movement_switches_between_rows_and_items() {
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Down),
                View::Albums,
                Focus::Content
            ),
            Some(Action::AlbumRow(1))
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Down),
                View::Tracks,
                Focus::Content
            ),
            Some(Action::MoveSelection(1))
        );
    }

    #[test]
    fn the_settings_view_claims_the_keys_it_needs() {
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char(' ')),
                View::Settings,
                Focus::Content
            ),
            Some(Action::Setting(SettingInput::Toggle))
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Enter),
                View::Settings,
                Focus::Content
            ),
            Some(Action::Setting(SettingInput::Toggle))
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Left),
                View::Settings,
                Focus::Content
            ),
            Some(Action::Setting(SettingInput::Decrease))
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Right),
                View::Settings,
                Focus::Content
            ),
            Some(Action::Setting(SettingInput::Increase))
        );
        // Elsewhere those keys keep their normal meaning.
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char(' ')),
                View::Home,
                Focus::Content
            ),
            Some(Action::Player(PlayerAction::Toggle))
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Enter),
                View::Home,
                Focus::Content
            ),
            Some(Action::Activate)
        );
    }

    #[test]
    fn queue_bindings_do_not_leak_into_other_views() {
        for (code, action) in [
            (KeyCode::Char('C'), Action::QueueClear),
            (KeyCode::Char('S'), Action::QueueSave),
            (KeyCode::Char('L'), Action::QueueLoad),
            (KeyCode::Delete, Action::QueueRemove),
        ] {
            assert_eq!(
                resolve_with(&defaults(), &press(code), View::Queue, Focus::Content),
                Some(action)
            );
            assert_eq!(
                resolve_with(&defaults(), &press(code), View::Home, Focus::Content),
                None
            );
        }
        // `d` is the one queue binding that shares a key with nothing else.
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char('d')),
                View::Queue,
                Focus::Content
            ),
            Some(Action::QueueRemove)
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char('d')),
                View::Home,
                Focus::Content
            ),
            None
        );
    }

    #[test]
    fn tab_prefers_the_genre_tabs_over_switching_panes() {
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Tab),
                View::GenreDetail,
                Focus::Content
            ),
            Some(Action::NextGenreTab)
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Tab),
                View::Genres,
                Focus::Content
            ),
            Some(Action::ToggleFocus)
        );
    }

    #[test]
    fn every_binding_appears_in_the_help() {
        // The anti-drift guarantee. Adding a binding without giving it a
        // category and a description fails here rather than quietly leaving the
        // help screen wrong, which is how it got out of date before.
        let listed: usize = help_sections(&defaults())
            .iter()
            .flat_map(|(_, entries)| entries)
            .map(|entry| entry.keys.split(" / ").count())
            .sum();
        assert_eq!(
            listed,
            BINDINGS.len(),
            "every binding must be reachable from the help screen"
        );
    }

    #[test]
    fn keys_the_old_help_forgot_are_documented_now() {
        let text: String = help_sections(&defaults())
            .iter()
            .flat_map(|(_, entries)| entries)
            .map(|entry| entry.keys.clone())
            .collect::<Vec<_>>()
            .join(" ");
        // All of these were bound but absent from the hand-written help.
        // Key labels are themselves translated, so these are the English ones
        // that the default catalogue produces.
        for key in ["c", "e", "Home", "End", "Ctrl+c"] {
            assert!(
                text.split(' ').any(|listed| listed == key),
                "{key} should be documented; got {text}"
            );
        }
    }

    #[test]
    fn aliases_share_one_help_row() {
        let sections = help_sections(&defaults());
        // Matched on the translation key rather than the rendered text, so the
        // test says nothing about which language is active.
        let row = sections
            .iter()
            .flat_map(|(_, entries)| entries)
            .find(|entry| entry.description == "action.down" && entry.scope.is_none())
            .expect("a row for moving down");
        assert_eq!(row.keys, "↓ / j", "arrow and vim keys belong on one row");
    }

    fn overrides_from(toml: &str) -> KeyOverrides {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("keybindings.toml");
        std::fs::write(&path, toml).expect("writing the overrides");
        KeyOverrides::load(&path)
    }

    #[test]
    fn without_a_file_the_defaults_stand() {
        let bindings = effective_bindings(&KeyOverrides::default());
        assert_eq!(bindings.len(), BINDINGS.len());
        assert_eq!(
            resolve_with(
                &bindings,
                &press(KeyCode::Char('q')),
                View::Home,
                Focus::Content
            ),
            Some(Action::Quit)
        );
    }

    #[test]
    fn rebinding_replaces_the_default_rather_than_adding_to_it() {
        // A rebind that left the old key working would be a trap: the user
        // thinks they freed the key and it still does the old thing.
        let overrides = overrides_from("quit = [\"ctrl+q\"]\n");
        assert!(overrides.problems.is_empty(), "{:?}", overrides.problems);
        let bindings = effective_bindings(&overrides);

        assert_eq!(
            resolve_with(
                &bindings,
                &with(KeyCode::Char('q'), KeyModifiers::CONTROL),
                View::Home,
                Focus::Content
            ),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve_with(
                &bindings,
                &press(KeyCode::Char('q')),
                View::Home,
                Focus::Content
            ),
            None,
            "the old key must stop quitting"
        );
    }

    #[test]
    fn plus_can_be_rebound_with_or_without_modifiers() {
        let plain = overrides_from("volume_up = [\"+\"]\n");
        let plain_bindings = effective_bindings(&plain);
        assert_eq!(
            resolve_with(
                &plain_bindings,
                &press(KeyCode::Char('+')),
                View::Home,
                Focus::Content
            ),
            Some(Action::Remote(RemoteCommand::VolumeUp))
        );

        let control = overrides_from("volume_up = [\"ctrl++\"]\n");
        let control_bindings = effective_bindings(&control);
        assert_eq!(
            resolve_with(
                &control_bindings,
                &with(KeyCode::Char('+'), KeyModifiers::CONTROL),
                View::Home,
                Focus::Content
            ),
            Some(Action::Remote(RemoteCommand::VolumeUp))
        );
    }

    #[test]
    fn several_keys_can_drive_one_action() {
        let overrides = overrides_from("play_pause = [\"space\", \"p\"]\n");
        let bindings = effective_bindings(&overrides);
        for code in [KeyCode::Char(' '), KeyCode::Char('p')] {
            assert_eq!(
                resolve_with(&bindings, &press(code), View::Home, Focus::Content),
                Some(Action::Player(PlayerAction::Toggle))
            );
        }
    }

    #[test]
    fn a_shifted_letter_is_written_either_way() {
        for written in ["shift+j", "J"] {
            let overrides = overrides_from(&format!("queue_up = [\"{written}\"]\n"));
            let bindings = effective_bindings(&overrides);
            assert_eq!(
                resolve_with(
                    &bindings,
                    &press(KeyCode::Char('J')),
                    View::Queue,
                    Focus::Content
                ),
                Some(Action::QueueMove(1)),
                "{written}"
            );
        }
    }

    #[test]
    fn a_broken_file_is_reported_rather_than_fatal() {
        // Refusing to start a music player over a typo in an optional file
        // would be the wrong trade.
        let overrides = overrides_from("quit = [\"ctrl+\"]\nnot_an_action = [\"z\"]\n");
        assert_eq!(overrides.problems.len(), 2, "{:?}", overrides.problems);
        assert!(
            overrides
                .problems
                .iter()
                .any(|p| p.contains("not_an_action"))
        );
        // Invalid replacements keep that action's defaults as well as the rest
        // of the binding table.
        let bindings = effective_bindings(&overrides);
        assert_eq!(
            resolve_with(
                &bindings,
                &press(KeyCode::Char('q')),
                View::Home,
                Focus::Content
            ),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve_with(
                &bindings,
                &press(KeyCode::Char('n')),
                View::Home,
                Focus::Content
            ),
            Some(Action::Player(PlayerAction::Next))
        );
    }

    #[test]
    fn malformed_toml_does_not_lose_the_defaults() {
        let overrides = overrides_from("this is not toml");
        assert_eq!(overrides.problems.len(), 1);
        assert_eq!(effective_bindings(&overrides).len(), BINDINGS.len());
    }

    #[test]
    fn every_bindable_action_has_a_stable_name() {
        // Names go in a user's configuration file, so two actions sharing one
        // would make a binding ambiguous.
        let mut seen: Vec<(&str, Action)> = Vec::new();
        for binding in BINDINGS {
            let name = action_name(binding.action);
            if let Some((_, other)) = seen.iter().find(|(existing, _)| *existing == name) {
                assert_eq!(*other, binding.action, "{name} names two different actions");
            } else {
                seen.push((name, binding.action));
            }
        }
    }

    #[test]
    fn unbound_keys_resolve_to_nothing() {
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::F(5)),
                View::Home,
                Focus::Content
            ),
            None
        );
        assert_eq!(
            resolve_with(
                &defaults(),
                &press(KeyCode::Char('Z')),
                View::Home,
                Focus::Content
            ),
            None
        );
    }
}
