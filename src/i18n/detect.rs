//! Working out which language to use.

use super::Language;

/// The language the environment asks for, if muscli speaks it.
///
/// Checked in the order the C locale standard defines: LC_ALL overrides
/// everything, LC_MESSAGES covers interface text specifically, and LANG is the
/// general fallback.
#[cfg(unix)]
pub fn detect_language() -> Option<Language> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find_map(|value| Language::from_tag(&value))
}

/// Windows has no locale environment variables, so ask the API.
#[cfg(windows)]
pub fn detect_language() -> Option<Language> {
    use windows::Win32::System::WindowsProgramming::GetUserDefaultLocaleName;

    let mut buffer = [0u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    if length <= 1 {
        return None;
    }
    // The returned length counts the terminating null.
    let tag = String::from_utf16_lossy(&buffer[..(length as usize).saturating_sub(1)]);
    Language::from_tag(&tag)
}
