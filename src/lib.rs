pub mod cli;
pub mod config;
pub mod control;
pub mod db;
pub mod discord;
pub mod doctor;
pub mod features;
pub mod fsutil;
pub mod i18n;
pub mod instance;
pub mod library;
pub mod model;
pub mod mpris;
#[cfg(unix)]
pub mod omarchy;
#[cfg(windows)]
pub mod omarchy {
    use anyhow::Result;

    use crate::paths::AppPaths;

    pub fn setup(_paths: &AppPaths) -> Result<String> {
        anyhow::bail!("Omarchy integration is only available on Linux");
    }

    pub fn undo(_paths: &AppPaths) -> Result<String> {
        anyhow::bail!("Omarchy integration is only available on Linux");
    }
}
pub mod paths;
pub mod player;
pub mod profiling;
pub mod replaygain;
pub mod tui;
