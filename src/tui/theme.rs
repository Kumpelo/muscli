//! Colour theme for the terminal UI.
//!
//! The palette is chosen by the `theme` setting and is the same set of choices
//! on every platform, so a Windows user is not left with whatever the terminal
//! happens to do. The default is the light palette: a white background reads on
//! the terminals people meet muscli in without any configuration.
//!
//! `system` is the one platform-dependent choice. On Linux it tracks the
//! current Omarchy theme, re-read when the file on disk changes; anywhere else,
//! and on a Linux machine without Omarchy, it falls back to the light palette.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UiTheme {
    pub(super) accent: Color,
    pub(super) selection: Color,
    pub(super) foreground: Color,
    pub(super) background: Color,
    pub(super) muted: Color,
    pub(super) border: Color,
}

/// A named palette the `theme` setting can ask for.
///
/// Stored in the configuration by name rather than by position, so inserting a
/// palette here cannot silently repaint somebody else's interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ThemeChoice {
    /// Follow the desktop: the Omarchy palette on Linux, light elsewhere.
    System,
    #[default]
    Light,
    Dark,
    HighContrast,
    Nord,
    Gruvbox,
    SolarizedLight,
}

/// Every choice, in the order the settings view cycles through them.
pub(super) const THEME_CHOICES: [ThemeChoice; 7] = [
    ThemeChoice::System,
    ThemeChoice::Light,
    ThemeChoice::Dark,
    ThemeChoice::HighContrast,
    ThemeChoice::Nord,
    ThemeChoice::Gruvbox,
    ThemeChoice::SolarizedLight,
];

impl ThemeChoice {
    /// The name written to the configuration file.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::HighContrast => "high-contrast",
            Self::Nord => "nord",
            Self::Gruvbox => "gruvbox",
            Self::SolarizedLight => "solarized-light",
        }
    }

    /// The translation key for the name shown in the settings view.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::System => "theme.system",
            Self::Light => "theme.light",
            Self::Dark => "theme.dark",
            Self::HighContrast => "theme.high_contrast",
            Self::Nord => "theme.nord",
            Self::Gruvbox => "theme.gruvbox",
            Self::SolarizedLight => "theme.solarized_light",
        }
    }

    /// Parse a configured name.
    ///
    /// Lenient about spelling — `high_contrast` and `High-Contrast` both work,
    /// and `auto` is accepted as a synonym of `system` because that is what the
    /// language setting calls the same idea. An unknown name is `None`, and the
    /// caller falls back to the default rather than refusing to start.
    pub(super) fn from_name(name: &str) -> Option<Self> {
        let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "system" | "auto" | "omarchy" => Some(Self::System),
            "light" | "white" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "high-contrast" => Some(Self::HighContrast),
            "nord" => Some(Self::Nord),
            "gruvbox" => Some(Self::Gruvbox),
            "solarized-light" | "solarized" => Some(Self::SolarizedLight),
            _ => None,
        }
    }

    /// The choice `name` asks for, or the default when it names nothing.
    pub(super) fn parse_or_default(name: &str) -> Self {
        Self::from_name(name).unwrap_or_default()
    }

    /// The next choice in the cycle, wrapping in either direction.
    pub(super) fn step(self, forward: bool) -> Self {
        let position = THEME_CHOICES
            .iter()
            .position(|choice| *choice == self)
            .unwrap_or(0);
        let count = THEME_CHOICES.len();
        let next = if forward {
            (position + 1) % count
        } else {
            (position + count - 1) % count
        };
        THEME_CHOICES[next]
    }

    /// The built-in palette for this choice, if it has one. `System` has none:
    /// it is read from the desktop, and only falls back to a built-in palette
    /// when there is nothing to read.
    fn palette(self) -> Option<UiTheme> {
        match self {
            Self::System => None,
            Self::Light => Some(UiTheme::light()),
            Self::Dark => Some(UiTheme::dark()),
            Self::HighContrast => Some(UiTheme::high_contrast()),
            Self::Nord => Some(UiTheme::nord()),
            Self::Gruvbox => Some(UiTheme::gruvbox()),
            Self::SolarizedLight => Some(UiTheme::solarized_light()),
        }
    }
}

const fn rgb(value: u32) -> Color {
    Color::Rgb(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    )
}

impl Default for UiTheme {
    fn default() -> Self {
        Self::light()
    }
}

impl UiTheme {
    /// The default. `selection` is a tint rather than a saturated colour
    /// because the selected row draws `foreground` on top of it.
    pub(super) const fn light() -> Self {
        Self {
            accent: rgb(0x6d31a8),
            selection: rgb(0xe2d9f3),
            foreground: rgb(0x1c1e21),
            background: rgb(0xffffff),
            muted: rgb(0x6b7280),
            border: rgb(0xc9ccd1),
        }
    }

    /// Deliberately the terminal's own palette rather than fixed values: a
    /// terminal already themed dark should keep its own black and magenta.
    pub(super) const fn dark() -> Self {
        Self {
            accent: Color::Magenta,
            selection: Color::Magenta,
            foreground: Color::White,
            background: Color::Black,
            muted: Color::Gray,
            border: Color::DarkGray,
        }
    }

    pub(super) const fn high_contrast() -> Self {
        Self {
            accent: rgb(0x0000aa),
            selection: rgb(0xd0d0d0),
            foreground: rgb(0x000000),
            background: rgb(0xffffff),
            muted: rgb(0x3d3d3d),
            border: rgb(0x000000),
        }
    }

    pub(super) const fn nord() -> Self {
        Self {
            accent: rgb(0x88c0d0),
            selection: rgb(0x3b4252),
            foreground: rgb(0xeceff4),
            background: rgb(0x2e3440),
            muted: rgb(0x8d98ab),
            border: rgb(0x4c566a),
        }
    }

    pub(super) const fn gruvbox() -> Self {
        Self {
            accent: rgb(0xd79921),
            selection: rgb(0x3c3836),
            foreground: rgb(0xebdbb2),
            background: rgb(0x282828),
            muted: rgb(0xa89984),
            border: rgb(0x504945),
        }
    }

    pub(super) const fn solarized_light() -> Self {
        Self {
            accent: rgb(0x268bd2),
            selection: rgb(0xeee8d5),
            foreground: rgb(0x073642),
            background: rgb(0xfdf6e3),
            muted: rgb(0x657b83),
            border: rgb(0xd5cdb6),
        }
    }

    /// The palette for a configured choice, with the file it came from and that
    /// file's modification time when it was read from disk.
    ///
    /// The path is what `refresh_theme` watches; a built-in palette returns
    /// none, which is how the caller knows there is nothing to watch.
    pub(super) fn load(choice: ThemeChoice) -> (Self, Option<PathBuf>, Option<SystemTime>) {
        if let Some(palette) = choice.palette() {
            return (palette, None, None);
        }
        Self::load_from_desktop()
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
    fn load_from_desktop() -> (Self, Option<PathBuf>, Option<SystemTime>) {
        for path in Self::source_paths().into_iter().flatten() {
            if let Some(theme) = Self::from_file(&path) {
                let modified = fs::metadata(&path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok());
                return (theme, Some(path), modified);
            }
        }
        (Self::light(), None, None)
    }

    /// Windows has no Omarchy theme to follow, so `system` is the default
    /// palette there.
    #[cfg(not(unix))]
    fn load_from_desktop() -> (Self, Option<PathBuf>, Option<SystemTime>) {
        (Self::light(), None, None)
    }

    #[cfg(any(unix, test))]
    pub(super) fn from_file(path: &Path) -> Option<Self> {
        let raw = fs::read_to_string(path).ok()?;
        let value = toml::from_str::<toml::Value>(&raw).ok()?;
        let color = |key: &str| value.get(key)?.as_str().and_then(parse_hex_color);
        // An Omarchy theme names its own mode, so a document that only sets an
        // accent is completed from a palette of the right brightness rather
        // than from black text on a black background.
        let fallback = match value.get("mode").and_then(toml::Value::as_str) {
            Some(mode) if mode.eq_ignore_ascii_case("light") => Self::light(),
            _ => Self::dark(),
        };
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

#[cfg(any(unix, test))]
pub(super) fn parse_hex_color(value: &str) -> Option<Color> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_sparse_document_is_completed_from_a_palette_of_its_own_mode() {
        // Only the accent is given, so everything else is borrowed; borrowing
        // from the light palette here would put light text on a light
        // background.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("colors.toml");
        fs::write(&path, "mode = \"dark\"\naccent = \"#509475\"\n").unwrap();
        let dark = UiTheme::from_file(&path).unwrap();
        assert_eq!(dark.background, UiTheme::dark().background);

        fs::write(&path, "mode = \"light\"\naccent = \"#509475\"\n").unwrap();
        let light = UiTheme::from_file(&path).unwrap();
        assert_eq!(light.background, UiTheme::light().background);
    }

    #[test]
    fn the_default_theme_is_the_light_one() {
        assert_eq!(UiTheme::default(), UiTheme::light());
        assert_eq!(ThemeChoice::default(), ThemeChoice::Light);
    }

    #[test]
    fn theme_names_round_trip() {
        for choice in THEME_CHOICES {
            assert_eq!(ThemeChoice::from_name(choice.name()), Some(choice));
        }
    }

    #[test]
    fn a_misspelled_theme_falls_back_to_the_default() {
        assert_eq!(
            ThemeChoice::parse_or_default("chartreuse"),
            ThemeChoice::Light
        );
        // Spellings a hand-edited file is likely to contain.
        assert_eq!(
            ThemeChoice::parse_or_default("High_Contrast"),
            ThemeChoice::HighContrast
        );
        assert_eq!(ThemeChoice::parse_or_default(" auto "), ThemeChoice::System);
    }

    #[test]
    fn stepping_wraps_in_both_directions() {
        let first = THEME_CHOICES[0];
        let last = THEME_CHOICES[THEME_CHOICES.len() - 1];
        assert_eq!(first.step(false), last);
        assert_eq!(last.step(true), first);
        assert_eq!(first.step(true).step(false), first);
    }

    #[test]
    fn every_built_in_palette_separates_text_from_its_background() {
        // A palette whose foreground equals its background is invisible, and
        // one whose selection equals its background hides the cursor row.
        for choice in THEME_CHOICES {
            let Some(palette) = choice.palette() else {
                continue;
            };
            assert_ne!(
                palette.foreground,
                palette.background,
                "{} has invisible text",
                choice.name()
            );
            assert_ne!(
                palette.selection,
                palette.background,
                "{} hides the selected row",
                choice.name()
            );
        }
    }
}
