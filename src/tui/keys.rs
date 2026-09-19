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

pub(super) struct Binding {
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

pub(super) fn resolve(key: &KeyEvent, view: View, focus: Focus) -> Option<Action> {
    let mods = effective_modifiers(key);
    BINDINGS
        .iter()
        .find(|binding| {
            binding.code == key.code && binding.mods == mods && binding.scope.matches(view, focus)
        })
        .map(|binding| binding.action)
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

/// The help screen, derived from the bindings themselves.
///
/// Written by hand it drifted: `c`, `e`, Home/End and Ctrl+C were bound but
/// undocumented, and the smart-playlist editor hints listed two keys fewer than
/// the editor implements. Deriving it means a binding cannot be added without
/// appearing here.
pub(super) fn help_sections() -> Vec<(&'static str, Vec<HelpEntry>)> {
    Category::ORDER
        .into_iter()
        .map(|category| {
            let mut entries: Vec<HelpEntry> = Vec::new();
            for binding in BINDINGS {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_keys_resolve_anywhere() {
        assert_eq!(
            resolve(&press(KeyCode::Char('q')), View::Home, Focus::Content),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve(&press(KeyCode::Char('n')), View::Queue, Focus::Sidebar),
            Some(Action::Player(PlayerAction::Next))
        );
    }

    #[test]
    fn a_modifier_the_binding_does_not_ask_for_is_not_ignored() {
        // Ctrl+j used to fall through to the bare `j` arm and move the
        // selection, because handle_key only ever matched on the key code.
        assert_eq!(
            resolve(
                &with(KeyCode::Char('j'), KeyModifiers::CONTROL),
                View::Home,
                Focus::Content
            ),
            None
        );
        assert_eq!(
            resolve(&press(KeyCode::Char('j')), View::Home, Focus::Content),
            Some(Action::MoveSelection(1))
        );
    }

    #[test]
    fn control_c_quits_but_plain_c_starts_a_playlist() {
        assert_eq!(
            resolve(
                &with(KeyCode::Char('c'), KeyModifiers::CONTROL),
                View::Home,
                Focus::Content
            ),
            Some(Action::Quit)
        );
        assert_eq!(
            resolve(&press(KeyCode::Char('c')), View::Home, Focus::Content),
            Some(Action::NewPlaylist)
        );
    }

    #[test]
    fn shift_is_accepted_on_uppercase_bindings() {
        // Terminals disagree about reporting shift alongside an uppercase char.
        for mods in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                resolve(&with(KeyCode::Char('J'), mods), View::Queue, Focus::Content),
                Some(Action::QueueMove(1)),
                "Shift handling must not depend on how the terminal reports it"
            );
        }
    }

    #[test]
    fn the_left_key_cascades_by_specificity() {
        assert_eq!(
            resolve(&press(KeyCode::Left), View::AlbumDetail, Focus::Content),
            Some(Action::Back)
        );
        assert_eq!(
            resolve(&press(KeyCode::Left), View::Albums, Focus::Content),
            Some(Action::AlbumStep(-1))
        );
        assert_eq!(
            resolve(&press(KeyCode::Left), View::Albums, Focus::Sidebar),
            Some(Action::FocusSidebar),
            "without the content pane focused there is no grid to move in"
        );
        assert_eq!(
            resolve(&press(KeyCode::Left), View::Tracks, Focus::Content),
            Some(Action::FocusSidebar)
        );
    }

    #[test]
    fn vertical_movement_switches_between_rows_and_items() {
        assert_eq!(
            resolve(&press(KeyCode::Down), View::Albums, Focus::Content),
            Some(Action::AlbumRow(1))
        );
        assert_eq!(
            resolve(&press(KeyCode::Down), View::Tracks, Focus::Content),
            Some(Action::MoveSelection(1))
        );
    }

    #[test]
    fn the_settings_view_claims_the_keys_it_needs() {
        assert_eq!(
            resolve(&press(KeyCode::Char(' ')), View::Settings, Focus::Content),
            Some(Action::Setting(SettingInput::Toggle))
        );
        assert_eq!(
            resolve(&press(KeyCode::Enter), View::Settings, Focus::Content),
            Some(Action::Setting(SettingInput::Toggle))
        );
        assert_eq!(
            resolve(&press(KeyCode::Left), View::Settings, Focus::Content),
            Some(Action::Setting(SettingInput::Decrease))
        );
        assert_eq!(
            resolve(&press(KeyCode::Right), View::Settings, Focus::Content),
            Some(Action::Setting(SettingInput::Increase))
        );
        // Elsewhere those keys keep their normal meaning.
        assert_eq!(
            resolve(&press(KeyCode::Char(' ')), View::Home, Focus::Content),
            Some(Action::Player(PlayerAction::Toggle))
        );
        assert_eq!(
            resolve(&press(KeyCode::Enter), View::Home, Focus::Content),
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
                resolve(&press(code), View::Queue, Focus::Content),
                Some(action)
            );
            assert_eq!(resolve(&press(code), View::Home, Focus::Content), None);
        }
        // `d` is the one queue binding that shares a key with nothing else.
        assert_eq!(
            resolve(&press(KeyCode::Char('d')), View::Queue, Focus::Content),
            Some(Action::QueueRemove)
        );
        assert_eq!(
            resolve(&press(KeyCode::Char('d')), View::Home, Focus::Content),
            None
        );
    }

    #[test]
    fn tab_prefers_the_genre_tabs_over_switching_panes() {
        assert_eq!(
            resolve(&press(KeyCode::Tab), View::GenreDetail, Focus::Content),
            Some(Action::NextGenreTab)
        );
        assert_eq!(
            resolve(&press(KeyCode::Tab), View::Genres, Focus::Content),
            Some(Action::ToggleFocus)
        );
    }

    #[test]
    fn every_binding_appears_in_the_help() {
        // The anti-drift guarantee. Adding a binding without giving it a
        // category and a description fails here rather than quietly leaving the
        // help screen wrong, which is how it got out of date before.
        let listed: usize = help_sections()
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
        let text: String = help_sections()
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
        let sections = help_sections();
        // Matched on the translation key rather than the rendered text, so the
        // test says nothing about which language is active.
        let row = sections
            .iter()
            .flat_map(|(_, entries)| entries)
            .find(|entry| entry.description == "action.down" && entry.scope.is_none())
            .expect("a row for moving down");
        assert_eq!(row.keys, "↓ / j", "arrow and vim keys belong on one row");
    }

    #[test]
    fn unbound_keys_resolve_to_nothing() {
        assert_eq!(
            resolve(&press(KeyCode::F(5)), View::Home, Focus::Content),
            None
        );
        assert_eq!(
            resolve(&press(KeyCode::Char('Z')), View::Home, Focus::Content),
            None
        );
    }
}
