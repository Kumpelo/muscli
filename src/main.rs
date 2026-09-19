use std::path::Path;

use anyhow::{Context, Result};
use clap::Parser;
use muscli::t;

use muscli::{
    cli::{
        Cli, Command, LibraryCommand, LyricsCommand, PlaylistCommand,
        RemoteCommand as CliRemoteCommand, SetupCommand, VolumeCommand,
    },
    config::{Config, all_sources},
    control::{self, RemoteCommand},
    db::Database,
    doctor,
    i18n::{self, Language},
    library,
    library::{prune_cover_cache, prune_unreferenced_covers},
    lyrics, m3u, omarchy,
    paths::AppPaths,
    replaygain,
    tui::keys,
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let compact = cli.compact;
    let paths = AppPaths::discover()?;
    paths.ensure()?;
    let mut config = Config::load(&paths)?;
    // Settled before anything can produce output. The flag wins over the
    // config file, which wins over the system locale.
    i18n::set_language(resolve_language(cli.lang.as_deref(), &config.language));

    match cli.command {
        Some(Command::Library { command }) => match command {
            LibraryCommand::Add { path } => {
                if config.add_source(&path)? {
                    config.save(&paths)?;
                    println!("{}", t!("cli.added", path = path.display()));
                } else {
                    println!("{}", t!("cli.already_configured", path = path.display()));
                }
            }
            LibraryCommand::Remove { path } => {
                if config.remove_source(&path) {
                    config.save(&paths)?;
                    println!("{}", t!("cli.removed", path = path.display()));
                } else {
                    println!("{}", t!("cli.not_configured", path = path.display()));
                }
            }
            LibraryCommand::List => {
                let configured = config.sources.clone();
                for source in all_sources(&config) {
                    let kind = t!(if configured.contains(&source) {
                        "cli.configured"
                    } else {
                        "cli.removable"
                    });
                    println!("{kind:10} {}", source.display());
                }
            }
            LibraryCommand::Rescan => {
                if control::send(&paths.control_socket(), RemoteCommand::Rescan)? {
                    println!("{}", t!("cli.rescan_requested"));
                    return Ok(());
                }
                let sources = all_sources(&config);
                let mut db = Database::open(&paths.database_file())?;
                let report =
                    library::scan_to_database(&mut db, &paths, &sources, &config.scan_options())?;
                println!(
                    "{}",
                    t!(
                        "cli.indexed",
                        tracks = report.tracks,
                        sources = report.sources,
                        skipped = report.skipped
                    )
                );
                for error in report.errors {
                    eprintln!("warning: {error}");
                }
            }
            LibraryCommand::Prune => {
                if control::send(&paths.control_socket(), RemoteCommand::Prune)? {
                    println!("{}", t!("cli.prune_requested"));
                    return Ok(());
                }
                let mut db = Database::open(&paths.database_file())?;
                let tracks = db.prune_missing_tracks()?;
                let dangling = db.clear_dangling_cover_paths()?;
                prune_unreferenced_covers(&paths.cover_cache_dir(), &db.referenced_cover_paths()?)?;
                let removed = prune_cover_cache(
                    &paths.cover_cache_dir(),
                    config.cover_cache_mb * 1024 * 1024,
                )?;
                let evicted = db.clear_cover_paths(&removed)?;
                println!(
                    "{}",
                    t!("cli.pruned", tracks = tracks, covers = dangling + evicted)
                );
            }
            LibraryCommand::WriteGain { yes } => {
                // The one command that changes the user's files, so it refuses
                // to run on a bare invocation rather than asking mid-stream.
                if !yes {
                    anyhow::bail!("{}", t!("cli.write_gain_confirm"));
                }
                let db = Database::open(&paths.database_file())?;
                let mut written = 0usize;
                let mut skipped = 0usize;
                for track in db.load_tracks()? {
                    if !track.available || !track.path.exists() {
                        skipped += 1;
                        continue;
                    }
                    let Some(analysis) = db.track_gain(&track.id, false)? else {
                        skipped += 1;
                        continue;
                    };
                    match replaygain::write_tags(&track.path, analysis) {
                        Ok(()) => written += 1,
                        Err(error) => {
                            skipped += 1;
                            eprintln!("warning: {}: {error:#}", track.path.display());
                        }
                    }
                }
                println!(
                    "{}",
                    t!("cli.write_gain_done", written = written, skipped = skipped)
                );
            }
            LibraryCommand::AnalyzeGain { force } => {
                let db = Database::open(&paths.database_file())?;
                let candidates = db.gain_analysis_candidates(force)?;
                let total = candidates.len();
                for (index, (id, path, size, modified)) in candidates.into_iter().enumerate() {
                    match replaygain::analyze(&path, config.replaygain_target_lufs) {
                        Ok(result) => {
                            db.save_gain(&id, result, size, modified)?;
                            println!("[{}/{}] {}", index + 1, total, path.display());
                        }
                        Err(error) => eprintln!("warning: {}: {error:#}", path.display()),
                    }
                }
            }
        },
        Some(Command::Keys) => {
            let overrides = keys::KeyOverrides::load(&paths.keybindings_file());
            for problem in &overrides.problems {
                eprintln!("warning: keybindings.toml: {problem}");
            }
            println!("{}", t!("cli.keys_header"));
            for (action, bound) in keys::binding_listing(&keys::effective_bindings(&overrides)) {
                println!("{action:<24}  {bound}");
            }
        }
        Some(Command::Playlist { command }) => match command {
            PlaylistCommand::Export { name, path } => {
                let db = Database::open(&paths.database_file())?;
                let playlist = db
                    .load_playlists()?
                    .into_iter()
                    .find(|playlist| playlist.name.eq_ignore_ascii_case(&name))
                    .with_context(|| format!("no playlist named {name}"))?;
                let tracks = db.load_tracks()?;
                let rows: Vec<_> = playlist
                    .track_ids
                    .iter()
                    .filter_map(|id| tracks.iter().find(|track| &track.id == id))
                    .map(|track| {
                        (
                            track.path.clone(),
                            format!("{} - {}", track.display_artist(), track.title),
                            track.duration_ms,
                        )
                    })
                    .collect();
                // Relative to the playlist's own directory, so it stays valid
                // if the whole tree moves.
                let base = path.parent().map(Path::to_path_buf);
                std::fs::write(&path, m3u::render(&rows, base.as_deref()))?;
                println!(
                    "{}",
                    t!(
                        "cli.playlist_exported",
                        count = rows.len(),
                        path = path.display()
                    )
                );
            }
            PlaylistCommand::Import { path, name } => {
                let source = std::fs::read_to_string(&path)
                    .with_context(|| format!("could not read {}", path.display()))?;
                let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                let entries = m3u::parse(&source, &base);

                let db = Database::open(&paths.database_file())?;
                let tracks = db.load_tracks()?;
                let name = name.unwrap_or_else(|| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Imported".to_owned())
                });
                let playlist = db.create_playlist(&name)?;

                let mut added = 0usize;
                let mut missing = Vec::new();
                for entry in entries {
                    // Match on the canonical path so a playlist written with a
                    // different spelling of the same location still resolves.
                    let wanted = entry.path.canonicalize().unwrap_or(entry.path.clone());
                    match tracks.iter().find(|track| {
                        track.path == wanted || track.path.canonicalize().is_ok_and(|p| p == wanted)
                    }) {
                        Some(track) => {
                            db.add_to_playlist(playlist, &track.id)?;
                            added += 1;
                        }
                        None => missing.push(
                            entry
                                .label
                                .unwrap_or_else(|| entry.path.display().to_string()),
                        ),
                    }
                }
                println!(
                    "{}",
                    t!("cli.playlist_imported", name = name, count = added)
                );
                for entry in &missing {
                    eprintln!("warning: not in the library: {entry}");
                }
            }
        },
        Some(Command::Lyrics { command }) => match command {
            LyricsCommand::Where => println!("{}", paths.lyrics_dir().display()),
            LyricsCommand::Import {
                path,
                track,
                artist,
                title,
            } => {
                // Naming by track id ties the file to one exact file on disk;
                // naming by tags survives that file being moved.
                let name = match (track, artist, title) {
                    (Some(id), _, _) => {
                        let db = Database::open(&paths.database_file())?;
                        let stored_id = db
                            .load_tracks()?
                            .into_iter()
                            .find(|candidate| candidate.id == id)
                            .map(|candidate| candidate.id)
                            .with_context(|| format!("unknown track id: {id}"))?;
                        format!("{stored_id}.lrc")
                    }
                    (None, Some(artist), Some(title)) => {
                        lyrics::descriptive_name(artist.as_str(), title.as_str())
                    }
                    _ => anyhow::bail!(
                        "give --track ID, or both --artist and --title, to say which song these lyrics belong to"
                    ),
                };
                let written = lyrics::import(&paths, &path, &name)?;
                println!("{}", t!("cli.lyrics_imported", path = written.display()));
            }
        },
        Some(Command::Summary { days }) => {
            let db = Database::open(&paths.database_file())?;
            let since = days.map(|days| chrono::Utc::now().timestamp() - days.max(0) * 86_400);
            let summary = db.listening_summary(since)?;
            let hours = summary.listened_ms as f64 / 3_600_000.0;
            println!(
                "{}",
                t!(
                    "cli.summary_totals",
                    plays = summary.plays,
                    hours = format!("{hours:.1}")
                )
            );
            if !summary.top_tracks.is_empty() {
                println!("\n{}", t!("cli.summary_tracks"));
                for (rank, row) in summary.top_tracks.iter().enumerate() {
                    println!("{:>3}. {:<52} {}", rank + 1, row.label, row.count);
                }
            }
            if !summary.top_artists.is_empty() {
                println!("\n{}", t!("cli.summary_artists"));
                for (rank, row) in summary.top_artists.iter().enumerate() {
                    println!("{:>3}. {:<52} {}", rank + 1, row.label, row.count);
                }
            }
        }
        Some(Command::Doctor) => {
            for check in doctor::run(&paths, &config)? {
                println!(
                    "{} {:24} {}",
                    if check.ok { "✓" } else { "✗" },
                    check.name,
                    check.detail
                );
            }
        }
        Some(Command::Setup { command }) => match command {
            SetupCommand::Omarchy { undo } => {
                println!(
                    "{}",
                    if undo {
                        omarchy::undo(&paths)?
                    } else {
                        omarchy::setup(&paths)?
                    }
                );
            }
            SetupCommand::Discord {
                application_id,
                large_image,
            } => {
                if application_id.is_empty()
                    || !application_id
                        .chars()
                        .all(|character| character.is_ascii_digit())
                {
                    anyhow::bail!("{}", t!("cli.discord_digits"));
                }
                config.discord_enabled = true;
                config.discord_application_id = Some(application_id.clone());
                config.discord_large_image = large_image.clone();
                config.save(&paths)?;
                println!(
                    "{}",
                    t!(
                        "cli.discord_enabled",
                        application = application_id,
                        asset = large_image
                    )
                );
            }
        },
        Some(Command::Remote { command }) => {
            let command = match command {
                CliRemoteCommand::Volume { command } => match command {
                    VolumeCommand::Up => RemoteCommand::VolumeUp,
                    VolumeCommand::Down => RemoteCommand::VolumeDown,
                    VolumeCommand::Set { percent } => RemoteCommand::VolumeSet(percent.min(100)),
                },
                CliRemoteCommand::MuteToggle => RemoteCommand::MuteToggle,
            };
            let _ = control::send(&paths.control_socket(), command)?;
        }
        None => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, muscli::tui::run(paths, config, compact))?;
        }
    }
    Ok(())
}

/// Pick the interface language.
///
/// An unrecognised name is not an error worth refusing to start over; it falls
/// through to detection, and then to English.
fn resolve_language(flag: Option<&str>, configured: &str) -> Language {
    flag.and_then(Language::from_tag)
        .or_else(|| Language::from_tag(configured))
        .or_else(i18n::detect_language)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_wins_over_the_configuration() {
        assert_eq!(resolve_language(Some("es"), "en"), Language::Spanish);
        assert_eq!(resolve_language(Some("en"), "es"), Language::English);
    }

    #[test]
    fn the_configuration_is_used_when_no_flag_is_given() {
        assert_eq!(resolve_language(None, "es"), Language::Spanish);
    }

    #[test]
    fn auto_and_nonsense_fall_through_to_detection() {
        // "auto" is the default setting, and a typo should not be fatal; both
        // land on detection, which ends at English when nothing matches.
        for configured in ["auto", "klingon", ""] {
            let resolved = resolve_language(None, configured);
            assert!(
                matches!(resolved, Language::English | Language::Spanish),
                "{configured}"
            );
        }
    }
}
