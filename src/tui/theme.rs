//! Colour theme for the terminal UI.
//!
//! On Linux the palette tracks the current Omarchy theme, which is re-read when
//! the file on disk changes; everywhere else the built-in palette is used.

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
    pub(super) fn load_with_source() -> (Self, Option<PathBuf>, Option<SystemTime>) {
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

    #[cfg(any(unix, test))]
    pub(super) fn from_file(path: &Path) -> Option<Self> {
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
}
