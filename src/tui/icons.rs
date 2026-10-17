//! Portable terminal symbols. Windows Terminal ships with fonts that do not
//! contain Nerd Font's private-use icons, so start with ASCII on Windows.
//! Users with a Nerd Font can opt in via MUSCLI_ICONS=nerd.
//! Linux keeps the existing visuals unless MUSCLI_ICONS=ascii is selected.

use std::sync::LazyLock;

static USE_NERD_FONT: LazyLock<bool> = LazyLock::new(|| {
    let override_style = std::env::var("MUSCLI_ICONS").ok();
    use_nerd_font(cfg!(windows), override_style.as_deref())
});

fn use_nerd_font(windows: bool, override_style: Option<&str>) -> bool {
    match override_style.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("nerd") => true,
        Some(value) if value.eq_ignore_ascii_case("ascii") => false,
        _ => !windows,
    }
}

/// One-cell ASCII fallbacks avoid missing-glyph boxes and cursor drift even
/// with the default Windows Terminal font.
pub(super) fn display(nerd: &'static str, ascii: &'static str) -> &'static str {
    if *USE_NERD_FONT { nerd } else { ascii }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_defaults_to_supported_glyphs_but_allows_nerd_fonts() {
        assert!(!use_nerd_font(true, None));
        assert!(use_nerd_font(true, Some("nerd")));
        assert!(!use_nerd_font(false, Some("ascii")));
        assert!(use_nerd_font(false, None));
    }
}
