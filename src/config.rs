use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

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
}

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
fn atomic_replace(source: &Path, target: &Path) -> Result<()> {
    fs::rename(source, target)?;
    Ok(())
}

#[cfg(windows)]
fn atomic_replace(source: &Path, target: &Path) -> Result<()> {
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };

    let source: Vec<u16> = source
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let target: Vec<u16> = target
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }?;
    Ok(())
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
        Win32::Storage::FileSystem::{DRIVE_REMOVABLE, GetDriveTypeW, GetLogicalDrives},
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
