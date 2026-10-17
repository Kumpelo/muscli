pub mod audio;
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
pub mod lyrics;
pub mod m3u;
pub mod mix_analysis;
pub mod model;
pub mod mpris;
#[cfg(unix)]
pub mod omarchy;
#[cfg(windows)]
pub mod omarchy {
    use anyhow::Result;

    use crate::paths::AppPaths;

    pub fn launch_large(_language: Option<&str>, _theme: Option<&str>) -> Result<()> {
        anyhow::bail!("--large requires Linux and Kitty");
    }

    pub fn setup(_paths: &AppPaths) -> Result<String> {
        anyhow::bail!("Omarchy integration is only available on Linux");
    }

    pub fn undo(_paths: &AppPaths) -> Result<String> {
        anyhow::bail!("Omarchy integration is only available on Linux");
    }
}
pub mod paths;
pub mod profiling;
pub mod replaygain;
pub mod tui;

pub mod widget;
