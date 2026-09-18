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
