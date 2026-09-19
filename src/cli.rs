use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "muscli",
    version,
    about = "Fast local music player for the terminal"
)]
pub struct Cli {
    /// Start with the compact player layout
    #[arg(long, global = true)]
    pub compact: bool,
    /// Interface language: auto, en or es
    #[arg(long, global = true)]
    pub lang: Option<String>,
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
    /// Show what you have been listening to
    ///
    /// Computed from the local index; nothing leaves the machine.
    Summary {
        /// Only count plays from the last N days
        #[arg(long)]
        days: Option<i64>,
    },
    /// Check runtime and desktop integration
    Doctor,
    /// Configure desktop integration
    Setup {
        #[command(subcommand)]
        command: SetupCommand,
    },
    /// List the key bindings in effect
    Keys,
    /// Import and export M3U playlists
    Playlist {
        #[command(subcommand)]
        command: PlaylistCommand,
    },
    /// Manage lyrics, which are never imported automatically
    Lyrics {
        #[command(subcommand)]
        command: LyricsCommand,
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
    /// Write cached ReplayGain results into the audio files themselves
    ///
    /// This is the only command that modifies your files. Nothing else muscli
    /// does writes to them.
    WriteGain {
        /// Required: confirms you want the files on disk changed
        #[arg(long)]
        yes: bool,
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
    /// Raise the volume by one step
    Up,
    /// Lower the volume by one step
    Down,
    /// Set the volume to an exact percentage
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

#[derive(Debug, Subcommand)]
pub enum LyricsCommand {
    /// Copy an .lrc file into the lyrics directory
    Import {
        path: PathBuf,
        /// Attach to this track id instead of naming the file by its tags
        #[arg(long)]
        track: Option<String>,
        /// Artist to name the file after, when no track id is given
        #[arg(long)]
        artist: Option<String>,
        /// Title to name the file after, when no track id is given
        #[arg(long)]
        title: Option<String>,
    },
    /// Show where lyrics are stored
    Where,
}

#[derive(Debug, Subcommand)]
pub enum PlaylistCommand {
    /// Write a playlist to an .m3u8 file
    Export {
        /// Name of the playlist to export
        name: String,
        /// Where to write it
        path: PathBuf,
    },
    /// Read an .m3u/.m3u8 file into a playlist
    Import {
        path: PathBuf,
        /// Playlist name; defaults to the file name
        #[arg(long)]
        name: Option<String>,
    },
}
