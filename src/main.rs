use anyhow::Result;
use clap::Parser;

use muscli::{
    cli::{
        Cli, Command, LibraryCommand, RemoteCommand as CliRemoteCommand, SetupCommand,
        VolumeCommand,
    },
    config::{Config, all_sources},
    control::{self, RemoteCommand},
    db::Database,
    doctor, library,
    library::{prune_cover_cache, prune_unreferenced_covers},
    omarchy,
    paths::AppPaths,
    replaygain,
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let compact = cli.compact;
    let paths = AppPaths::discover()?;
    paths.ensure()?;
    let mut config = Config::load(&paths)?;

    match cli.command {
        Some(Command::Library { command }) => match command {
            LibraryCommand::Add { path } => {
                if config.add_source(&path)? {
                    config.save(&paths)?;
                    println!("Added {}", path.display());
                } else {
                    println!("Already configured: {}", path.display());
                }
            }
            LibraryCommand::Remove { path } => {
                if config.remove_source(&path) {
                    config.save(&paths)?;
                    println!("Removed {}", path.display());
                } else {
                    println!("Not configured: {}", path.display());
                }
            }
            LibraryCommand::List => {
                let configured = config.sources.clone();
                for source in all_sources(&config) {
                    let kind = if configured.contains(&source) {
                        "configured"
                    } else {
                        "removable"
                    };
                    println!("{kind:10} {}", source.display());
                }
            }
            LibraryCommand::Rescan => {
                if control::send(&paths.control_socket(), RemoteCommand::Rescan)? {
                    println!("Rescan requested from the running muscli instance");
                    return Ok(());
                }
                let sources = all_sources(&config);
                let mut db = Database::open(&paths.database_file())?;
                let report = library::scan_to_database(
                    &mut db,
                    &paths,
                    &sources,
                    config.cover_cache_mb * 1024 * 1024,
                )?;
                println!(
                    "Indexed {} tracks from {} sources ({} skipped)",
                    report.tracks, report.sources, report.skipped
                );
                for error in report.errors {
                    eprintln!("warning: {error}");
                }
            }
            LibraryCommand::Prune => {
                if control::send(&paths.control_socket(), RemoteCommand::Prune)? {
                    println!("Maintenance requested from the running muscli instance");
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
                    "Pruned {tracks} missing tracks and {} dangling cover references",
                    dangling + evicted
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
                    anyhow::bail!("Discord application ID must contain only digits");
                }
                config.discord_enabled = true;
                config.discord_application_id = Some(application_id.clone());
                config.discord_large_image = large_image.clone();
                config.save(&paths)?;
                println!(
                    "Discord Rich Presence enabled for application {application_id}; fallback asset: {large_image}"
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
