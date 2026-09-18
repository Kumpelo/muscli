use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "muscli",
    version,
    about = "Fast local FLAC player for the terminal"
)]
pub struct Cli {
    /// Start with the compact player layout
    #[arg(long, global = true)]
    pub compact: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage indexed music folders
    Library {
        #[command(subcommand)]
        command: LibraryCommand,
    },
    /// Check runtime and desktop integration
    Doctor,
    /// Configure desktop integration
    Setup {
        #[command(subcommand)]
        command: SetupCommand,
    },
    /// Control the running muscli instance
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum LibraryCommand {
    /// Save an additional music folder
    Add { path: PathBuf },
    /// Remove a saved music folder
    Remove { path: PathBuf },
    /// List configured and auto-discovered folders
    List,
    /// Rebuild the incremental library index now
    Rescan,
    /// Remove stale tracks and dangling covers while preserving unplugged volumes
    Prune,
    /// Analyze loudness without modifying audio files
    AnalyzeGain {
        /// Re-analyze files that already have cached results
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum RemoteCommand {
    /// Change the volume of muscli only
    Volume {
        #[command(subcommand)]
        command: VolumeCommand,
    },
    /// Toggle mute for muscli only
    MuteToggle,
}

#[derive(Debug, Subcommand)]
pub enum VolumeCommand {
    Up,
    Down,
    Set { percent: u8 },
}

#[derive(Debug, Subcommand)]
pub enum SetupCommand {
    /// Enable the built-in Omarchy MPRIS widget and desktop launcher
    Omarchy {
        /// Undo only the changes previously made by muscli
        #[arg(long)]
        undo: bool,
    },
    /// Configure local Discord/Vesktop Rich Presence
    Discord {
        /// Discord Developer Portal application ID
        application_id: String,
        /// Uploaded Rich Presence asset key used as cover fallback
        #[arg(long, default_value = "peter")]
        large_image: String,
    },
}
