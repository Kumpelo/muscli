use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{fsutil::atomic_replace, paths::AppPaths};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayGainMode {
    Album,
    Track,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sources: Vec<PathBuf>,
    pub auto_discover_removable: bool,
    pub cover_cache_mb: u64,
    pub discord_enabled: bool,
    pub discord_application_id: Option<String>,
    pub discord_large_image: String,
    pub replaygain_enabled: bool,
    pub replaygain_mode: ReplayGainMode,
    pub replaygain_target_lufs: f64,
    pub history_enabled: bool,
    pub resume_enabled: bool,
    pub compact_default: bool,
    pub show_covers: bool,
    pub volume_step: u8,
    /// Worker threads for library scanning. `0` derives a value from the
    /// machine, capped so a spinning disk is not thrashed by seeks.
    pub scan_threads: usize,
    /// Let mpv open the next track before the current one ends, so album sides
    /// run together without a gap.
    pub gapless: bool,
    /// Interface language: "auto", "en" or "es". Auto follows the system
    /// locale and falls back to English.
    pub language: String,
    /// File extensions to index. Empty means the built-in list.
    pub audio_extensions: Vec<String>,
    /// Equaliser gains in decibels, one per band of `EQUALIZER_BANDS`. Empty
    /// or all zero means no equaliser at all.
    pub equalizer: Vec<f32>,
}

/// Centre frequencies of the equaliser bands, an octave apart.
pub const EQUALIZER_BANDS: [u32; 8] = [60, 150, 400, 1_000, 2_400, 6_000, 12_000, 16_000];

impl Default for Config {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            auto_discover_removable: true,
            cover_cache_mb: 64,
            discord_enabled: false,
            discord_application_id: None,
            discord_large_image: "peter".into(),
            replaygain_enabled: true,
            replaygain_mode: ReplayGainMode::Album,
            replaygain_target_lufs: -18.0,
            history_enabled: true,
            resume_enabled: true,
            compact_default: false,
            show_covers: true,
            volume_step: 5,
            scan_threads: 0,
            gapless: true,
            language: "auto".into(),
            audio_extensions: Vec::new(),
            equalizer: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let path = paths.config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        toml::from_str(&raw).context("invalid muscli config")
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        paths.ensure()?;
        let rendered = toml::to_string_pretty(self)?;
        let target = paths.config_file();
        let tmp = target.with_extension("toml.tmp");
        fs::write(&tmp, rendered)?;
        atomic_replace(&tmp, &target)?;
        Ok(())
    }

    pub fn add_source(&mut self, path: &Path) -> Result<bool> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("source does not exist: {}", path.display()))?;
        if !canonical.is_dir() {
            anyhow::bail!("source is not a directory: {}", canonical.display());
        }
        if self.sources.contains(&canonical) {
            return Ok(false);
        }
        self.sources.push(canonical);
        self.sources.sort();
        Ok(true)
    }

    pub fn remove_source(&mut self, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let before = self.sources.len();
        self.sources
            .retain(|item| item != &canonical && item != path);
        before != self.sources.len()
    }
}

#[cfg(unix)]
pub fn discover_removable_roots() -> Vec<PathBuf> {
    let user = std::env::var("USER").unwrap_or_default();
    let mut found = Vec::new();
    for base in [
        PathBuf::from("/run/media").join(&user),
        PathBuf::from("/media").join(&user),
    ] {
        let Ok(entries) = fs::read_dir(base) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.push(path);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

#[cfg(windows)]
pub fn discover_removable_roots() -> Vec<PathBuf> {
    use windows::{
        Win32::{
            Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives},
            System::WindowsProgramming::DRIVE_REMOVABLE,
        },
        core::PCWSTR,
    };

    let drives = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|index| drives & (1 << index) != 0)
        .filter_map(|index| {
            let letter = (b'A' + index as u8) as char;
            let root = format!("{letter}:\\");
            let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            (unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) } == DRIVE_REMOVABLE)
                .then(|| PathBuf::from(root))
        })
        .collect()
}

pub fn all_sources(config: &Config) -> Vec<PathBuf> {
    let mut sources = config.sources.clone();
    if config.auto_discover_removable {
        sources.extend(discover_removable_roots());
    }
    sources.sort();
    sources.dedup();
    sources
}

impl Config {
    /// The equaliser bands, paired with their configured gains.
    ///
    /// A configuration with too few or too many gains is used as far as it
    /// goes rather than rejected: a hand-edited file should not stop playback.
    pub fn equalizer_bands(&self) -> Vec<(u32, f32)> {
        EQUALIZER_BANDS
            .iter()
            .zip(self.equalizer.iter().copied().chain(std::iter::repeat(0.0)))
            .map(|(frequency, gain)| (*frequency, gain.clamp(-12.0, 12.0)))
            .collect()
    }

    /// The scan options this configuration asks for.
    pub fn scan_options(&self) -> crate::library::ScanOptions {
        crate::library::ScanOptions {
            threads: self.scan_threads,
            cover_cache_bytes: self.cover_cache_mb * 1024 * 1024,
            full: true,
            extensions: if self.audio_extensions.is_empty() {
                crate::library::DEFAULT_EXTENSIONS
                    .iter()
                    .map(|extension| (*extension).to_owned())
                    .collect()
            } else {
                self.audio_extensions.clone()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_equalizer_is_flat() {
        let config = Config::default();
        let bands = config.equalizer_bands();
        assert_eq!(bands.len(), EQUALIZER_BANDS.len());
        assert!(bands.iter().all(|(_, gain)| *gain == 0.0));
    }

    #[test]
    fn a_short_list_of_gains_is_used_as_far_as_it_goes() {
        // A hand-edited configuration file should not stop playback.
        let config = Config {
            equalizer: vec![3.0, -2.0],
            ..Config::default()
        };
        let bands = config.equalizer_bands();
        assert_eq!(bands[0], (EQUALIZER_BANDS[0], 3.0));
        assert_eq!(bands[1], (EQUALIZER_BANDS[1], -2.0));
        assert!(bands[2..].iter().all(|(_, gain)| *gain == 0.0));
    }

    #[test]
    fn extra_gains_are_ignored_and_absurd_ones_clamped() {
        let config = Config {
            equalizer: vec![99.0; EQUALIZER_BANDS.len() + 4],
            ..Config::default()
        };
        let bands = config.equalizer_bands();
        assert_eq!(bands.len(), EQUALIZER_BANDS.len());
        assert!(bands.iter().all(|(_, gain)| *gain == 12.0));
    }
}
