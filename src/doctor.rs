use std::{fs, path::Path, process::Command};

#[cfg(unix)]
use std::path::PathBuf;

use anyhow::Result;

use crate::{
    audio::native::device::CpalOutput,
    config::{AudioBackendChoice, Config, all_sources},
    db::Database,
    discord::MUSCLI_DISCORD_APPLICATION_ID,
    paths::AppPaths,
    t,
};

#[derive(Debug)]
pub struct Check {
    pub ok: bool,
    pub name: &'static str,
    pub detail: String,
}

/// What will actually make the sound, and whether it can.
///
/// The native path needs a device it can open; if it cannot, muscli falls
/// back to mpv at startup with a note in the status bar, and this is where a
/// listener can find out why before it happens.
fn audio_backend(config: &Config) -> Check {
    if config.audio_backend == AudioBackendChoice::Mpv {
        return Check {
            ok: true,
            name: "audio backend",
            detail: "mpv".into(),
        };
    }

    let wanted = config.audio_device.trim();
    match CpalOutput::devices() {
        Ok(devices) if devices.is_empty() => Check {
            ok: false,
            name: "audio backend",
            detail: "native, but no output device was found".into(),
        },
        Ok(devices) if !wanted.is_empty() && !devices.iter().any(|name| name == wanted) => Check {
            ok: false,
            name: "audio backend",
            detail: format!("native, but there is no device called {wanted}"),
        },
        Ok(_) => Check {
            ok: true,
            name: "audio backend",
            detail: if wanted.is_empty() {
                "native, on the default device".into()
            } else {
                format!("native, on {wanted}")
            },
        },
        Err(error) => Check {
            ok: false,
            name: "audio backend",
            detail: format!("native, but the devices could not be listed: {error}"),
        },
    }
}

pub fn run(paths: &AppPaths, config: &Config) -> Result<Vec<Check>> {
    let mut checks = platform_checks();
    let sources = all_sources(config);
    checks.push(Check {
        ok: sources.iter().any(|p| p.exists()),
        name: "music sources",
        detail: if sources.is_empty() {
            "none found; insert a drive or run `muscli library add PATH`".into()
        } else {
            sources
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        },
    });
    checks.push(Check {
        ok: parent_writable(&paths.database_file()),
        name: "database",
        detail: paths.database_file().display().to_string(),
    });
    checks.push(audio_backend(config));
    let discord_socket = find_discord_ipc();
    checks.push(Check {
        ok: !config.discord_enabled || discord_socket.is_some(),
        name: "Discord Rich Presence",
        detail: if config.discord_enabled {
            match discord_socket {
                Some(socket) => format!(
                    "enabled for {}; IPC {}",
                    MUSCLI_DISCORD_APPLICATION_ID, socket
                ),
                None => "enabled, but Discord/Vesktop IPC is not available".into(),
            }
        } else {
            "disabled; enable it in Settings or run `muscli setup discord`".into()
        },
    });
    #[cfg(unix)]
    {
        let widget = omarchy_media_enabled();
        checks.push(Check {
            ok: widget,
            name: "Omarchy media widget",
            detail: if widget {
                "enabled".into()
            } else {
                "disabled; run `muscli setup omarchy`".into()
            },
        });
    }
    let term = std::env::var("TERM").unwrap_or_else(|_| "unknown".into());
    let protocol = fs::read_to_string(paths.image_protocol_file())
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let protocol_ok = protocol
        .as_deref()
        .is_some_and(|value| !value.starts_with("Halfblocks (fallback"));
    let protocol = protocol.unwrap_or_else(|| t!("doctor.protocol_unknown").to_owned());
    checks.push(Check {
        ok: protocol_ok,
        name: "terminal graphics",
        detail: t!("doctor.terminal_graphics", term = term, protocol = protocol),
    });
    let cache_bytes = directory_size(&paths.cover_cache_dir());
    let cache_limit = config.cover_cache_mb.saturating_mul(1024 * 1024);
    checks.push(Check {
        ok: cache_bytes <= cache_limit,
        name: "cover cache",
        detail: format!(
            "{:.1} MiB / {} MiB",
            cache_bytes as f64 / 1024.0 / 1024.0,
            config.cover_cache_mb
        ),
    });
    match Database::open(&paths.database_file()).and_then(|db| db.library_health()) {
        Ok(health) => {
            checks.push(Check {
                ok: health.dangling_covers == 0,
                name: "cover references",
                detail: format!("{} dangling", health.dangling_covers),
            });
            checks.push(Check {
                ok: health.missing_tracks_on_available_sources == 0,
                name: "library rows",
                detail: format!(
                    "{} tracks; {} unavailable; {} missing on connected sources",
                    health.tracks,
                    health.unavailable_tracks,
                    health.missing_tracks_on_available_sources
                ),
            });
            checks.push(Check {
                ok: health
                    .sources
                    .iter()
                    .all(|(root, available)| !*available || root.is_dir()),
                name: "indexed sources",
                detail: health
                    .sources
                    .iter()
                    .map(|(root, available)| {
                        format!(
                            "{} ({})",
                            root.display(),
                            if *available && root.is_dir() {
                                "mounted"
                            } else {
                                "offline"
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
        Err(error) => checks.push(Check {
            ok: false,
            name: "library database",
            detail: format!("could not inspect it: {error:#}"),
        }),
    }
    checks.push(Check {
        ok: true,
        name: "instance",
        detail: if instance_is_alive(&paths.lock_file()) {
            "running; rescan/prune will be delegated through the control socket".into()
        } else {
            "not running".into()
        },
    });
    Ok(checks)
}

fn directory_size(path: &Path) -> u64 {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

#[cfg(unix)]
fn instance_is_alive(lock: &Path) -> bool {
    fs::read_to_string(lock)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(|pid| PathBuf::from(format!("/proc/{pid}")).is_dir())
}

#[cfg(windows)]
fn instance_is_alive(_lock: &Path) -> bool {
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{MUTEX_ALL_ACCESS, OpenMutexW},
        },
        core::PCWSTR,
    };

    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let name = format!("Local\\muscli-{user}");
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let Ok(handle) = (unsafe { OpenMutexW(MUTEX_ALL_ACCESS, false, PCWSTR(wide.as_ptr())) }) else {
        return false;
    };
    unsafe { CloseHandle(handle) }.is_ok()
}

#[cfg(unix)]
fn find_discord_ipc() -> Option<String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    let roots = [
        runtime.clone(),
        runtime.join("app/com.discordapp.Discord"),
        runtime.join("app/dev.vencord.Vesktop"),
        runtime.join(".flatpak/com.discordapp.Discord/xdg-run"),
        runtime.join(".flatpak/dev.vencord.Vesktop/xdg-run"),
    ];
    roots.into_iter().find_map(|root| {
        fs::read_dir(root)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("discord-ipc-"))
            })
            .map(|path| path.display().to_string())
    })
}

#[cfg(windows)]
fn find_discord_ipc() -> Option<String> {
    let output = Command::new("tasklist")
        .args(["/NH", "/FO", "CSV"])
        .output()
        .ok()?;
    let tasks = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    (tasks.contains("discord.exe") || tasks.contains("vesktop.exe"))
        .then_some(r"\\.\pipe\discord-ipc-*".into())
}

#[cfg(unix)]
fn platform_checks() -> Vec<Check> {
    vec![
        command_check("mpv", &["--version"], "Install with `omarchy pkg add mpv`"),
        command_check(
            "ffmpeg",
            &["-version"],
            "Install FFmpeg to analyze ReplayGain",
        ),
        command_check("ionice", &["--version"], "Install util-linux"),
        command_check("findmnt", &["--version"], "Install util-linux"),
        command_check("omarchy", &["version"], "Omarchy was not found"),
        command_check(
            "busctl",
            &["--user", "status"],
            "A user D-Bus session is required",
        ),
    ]
}

#[cfg(windows)]
fn platform_checks() -> Vec<Check> {
    let app_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let mpv = app_dir.as_ref().map(|path| path.join("mpv.exe"));
    let ffmpeg = app_dir.as_ref().map(|path| path.join("ffmpeg.exe"));
    vec![
        Check {
            ok: mpv.as_ref().is_some_and(|path| path.is_file()),
            name: "mpv",
            detail: mpv.map_or_else(
                || "application directory unavailable".into(),
                |path| path.display().to_string(),
            ),
        },
        Check {
            ok: ffmpeg.as_ref().is_some_and(|path| path.is_file()),
            name: "ffmpeg",
            detail: ffmpeg.map_or_else(
                || "application directory unavailable".into(),
                |path| path.display().to_string(),
            ),
        },
        Check {
            ok: true,
            name: "Windows media controls",
            detail: "SMTC integration enabled".into(),
        },
    ]
}

#[cfg(unix)]
fn command_check(name: &'static str, args: &[&str], help: &str) -> Check {
    match Command::new(name).args(args).output() {
        Ok(output) if output.status.success() => Check {
            ok: true,
            name,
            detail: "available".into(),
        },
        _ => Check {
            ok: false,
            name,
            detail: help.into(),
        },
    }
}

fn parent_writable(path: &Path) -> bool {
    path.parent()
        .is_some_and(|p| p.exists() || std::fs::create_dir_all(p).is_ok())
}

#[cfg(unix)]
pub fn omarchy_media_enabled() -> bool {
    Command::new("omarchy")
        .args(["plugin", "list", "--json"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("id").and_then(|v| v.as_str()) == Some("omarchy.media")
                    && item.get("enabled").and_then(|v| v.as_bool()) == Some(true)
            })
        })
}

#[cfg(windows)]
pub fn omarchy_media_enabled() -> bool {
    false
}
