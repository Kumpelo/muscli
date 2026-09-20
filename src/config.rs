use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{fsutil::atomic_replace, paths::AppPaths};

/// Which player actually makes the sound.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioBackendChoice {
    /// mpv, driven over its IPC socket. Plays everything, including the
    /// formats the native path has no decoder for.
    #[default]
    Mpv,
    /// The built-in path: decode, process and write to the device here, so
    /// the equaliser, the gain and the conversion are this program's own
    /// arithmetic rather than somebody else's.
    Native,
}

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
    pub discord_large_image: String,
    pub replaygain_enabled: bool,
    pub replaygain_mode: ReplayGainMode,
    pub replaygain_target_lufs: f64,
    pub history_enabled: bool,
    pub resume_enabled: bool,
    pub compact_default: bool,
    pub show_covers: bool,
    pub volume_step: u8,
    /// How far below unity the volume control reaches at the bottom of its
    /// travel, in decibels.
    ///
    /// The control moves in decibels, so every step is the same size to the
    /// ear: three decibels a press at the default 5%. -60 rather than a mixing
    /// desk's -80, since music is already inaudible at -60.
    pub volume_range_db: f64,
    /// Worker threads for library scanning. `0` derives a value from the
    /// machine, capped so a spinning disk is not thrashed by seeks.
    pub scan_threads: usize,
    /// Let mpv open the next track before the current one ends, so album sides
    /// run together without a gap.
    pub gapless: bool,
    /// Interface language: "auto", "en" or "es". Auto follows the system
    /// locale and falls back to English.
    pub language: String,
    /// Colour theme: "light" (the default), "dark", "high-contrast", "nord",
    /// "gruvbox", "solarized-light", or "system" to follow the desktop — the
    /// current Omarchy palette on Linux, light everywhere else.
    pub theme: String,
    /// File extensions to index. Empty means the built-in list.
    pub audio_extensions: Vec<String>,
    /// Equaliser gains in decibels, one per band of `EQUALIZER_BANDS`. Empty
    /// or all zero means no equaliser at all.
    pub equalizer: Vec<f32>,
    /// Which player makes the sound.
    pub audio_backend: AudioBackendChoice,
    /// Output device for the native backend, by the name `muscli devices`
    /// prints. Empty means the system default.
    pub audio_device: String,
    /// Hand the decoder's samples to the device untouched, giving up the
    /// volume, ReplayGain and the equaliser. The settings view says so.
    pub bit_perfect: bool,
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
            discord_large_image: "peter".into(),
            replaygain_enabled: true,
            replaygain_mode: ReplayGainMode::Album,
            replaygain_target_lufs: -18.0,
            history_enabled: true,
            resume_enabled: true,
            compact_default: false,
            show_covers: true,
            volume_step: 5,
            volume_range_db: -60.0,
            scan_threads: 0,
            gapless: true,
            language: "auto".into(),
            theme: "light".into(),
            audio_extensions: Vec::new(),
            equalizer: Vec::new(),
            audio_backend: AudioBackendChoice::default(),
            audio_device: String::new(),
            bit_perfect: false,
        }
    }
}

impl Config {
    /// The usable span of the volume control, guarded against a setting that
    /// would make it either pointless or unusable.
    fn volume_range(&self) -> f64 {
        self.volume_range_db.clamp(-120.0, -6.0)
    }

    /// The amplitude a control position asks for.
    ///
    /// The bottom of the travel is true silence rather than merely very
    /// quiet: a volume control that cannot reach zero is a broken one.
    pub fn volume_gain(&self, position: f64) -> f64 {
        let position = position.clamp(0.0, 1.0);
        if position <= 0.0 {
            return 0.0;
        }
        10.0f64.powf((1.0 - position) * self.volume_range() / 20.0)
    }

    /// Where an amplitude sits on the control. The inverse of
    /// [`volume_gain`](Self::volume_gain).
    pub fn volume_position(&self, gain: f64) -> f64 {
        if gain <= 0.0 {
            return 0.0;
        }
        (1.0 - 20.0 * gain.clamp(0.0, 1.0).log10() / self.volume_range()).clamp(0.0, 1.0)
    }

    /// What the control is set to, in decibels, or `None` at silence.
    pub fn volume_db(&self, position: f64) -> Option<f64> {
        (position > 0.0).then(|| (1.0 - position.clamp(0.0, 1.0)) * self.volume_range())
    }

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

    /// Set one equaliser band's gain, in decibels.
    ///
    /// The stored list is grown to the full set of bands first: a
    /// hand-written configuration may carry fewer gains than there are bands,
    /// and writing to band six of a two-entry list would otherwise be lost.
    pub fn set_equalizer_gain(&mut self, band: usize, gain: f32) {
        if band >= EQUALIZER_BANDS.len() {
            return;
        }
        if self.equalizer.len() < EQUALIZER_BANDS.len() {
            self.equalizer.resize(EQUALIZER_BANDS.len(), 0.0);
        }
        self.equalizer[band] = gain.clamp(-12.0, 12.0);
    }

    /// One equaliser band's configured gain, treating an absent entry as flat.
    pub fn equalizer_gain(&self, band: usize) -> f32 {
        self.equalizer
            .get(band)
            .copied()
            .unwrap_or(0.0)
            .clamp(-12.0, 12.0)
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
    fn the_ends_of_the_volume_control_are_where_they_should_be() {
        let config = Config::default();
        assert_eq!(config.volume_gain(1.0), 1.0, "the top is not unity");
        // Not merely very quiet: a volume control that cannot reach zero is
        // a broken one.
        assert_eq!(config.volume_gain(0.0), 0.0, "the bottom is not silence");
        assert_eq!(config.volume_db(0.0), None);
        assert_eq!(config.volume_db(1.0), Some(0.0));
    }

    #[test]
    fn every_step_is_the_same_size_in_decibels() {
        // The point of the whole thing. Applied as an amplitude, five per
        // cent of travel is 0.4 dB at the top and 6 dB at the bottom, so the
        // same key does something different depending on where you are.
        let config = Config::default();
        let step = f64::from(config.volume_step) / 100.0;

        let mut previous: Option<f64> = None;
        let mut position = step;
        while position <= 1.0 + f64::EPSILON {
            let db = config.volume_db(position).expect("above silence");
            if let Some(previous) = previous {
                let moved = db - previous;
                assert!(
                    (moved - 3.0).abs() < 0.01,
                    "a step at {position:.2} moved {moved:.3} dB instead of 3"
                );
            }
            previous = Some(db);
            position += step;
        }
    }

    #[test]
    fn a_position_and_an_amplitude_convert_back_and_forth() {
        // What the migration of an old saved session rests on.
        let config = Config::default();
        for position in [0.0, 0.05, 0.25, 0.5, 0.75, 1.0] {
            let round_trip = config.volume_position(config.volume_gain(position));
            assert!(
                (round_trip - position).abs() < 1e-9,
                "{position} came back as {round_trip}"
            );
        }
        // Half amplitude is 6 dB down, which is a tenth of the way down a
        // sixty decibel control.
        assert!((config.volume_position(0.5) - 0.8997).abs() < 0.001);
    }

    #[test]
    fn an_absurd_range_is_brought_back_to_something_usable() {
        let config = Config {
            volume_range_db: 0.0,
            ..Config::default()
        };
        // A range of nothing would make the control inert; it is clamped to
        // the smallest span that still does something.
        assert!(config.volume_gain(0.5) < 1.0);
    }

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
    fn setting_a_band_grows_a_short_list_instead_of_losing_the_change() {
        let mut config = Config {
            equalizer: vec![3.0],
            ..Config::default()
        };
        config.set_equalizer_gain(5, -4.0);
        assert_eq!(config.equalizer.len(), EQUALIZER_BANDS.len());
        assert_eq!(config.equalizer_gain(0), 3.0);
        assert_eq!(config.equalizer_gain(5), -4.0);
        assert_eq!(config.equalizer_gain(7), 0.0);
    }

    #[test]
    fn a_band_outside_the_set_is_ignored() {
        let mut config = Config::default();
        config.set_equalizer_gain(EQUALIZER_BANDS.len(), 6.0);
        assert!(config.equalizer.is_empty());
    }

    #[test]
    fn the_default_theme_is_light() {
        assert_eq!(Config::default().theme, "light");
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
