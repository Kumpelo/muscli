use std::{fs, path::PathBuf, process::Command};

use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};

use crate::{doctor::omarchy_media_enabled, paths::AppPaths};

#[derive(Debug, Serialize, Deserialize)]
struct SetupState {
    widget_was_enabled: bool,
    desktop_file: PathBuf,
    backup_file: Option<PathBuf>,
    #[serde(default)]
    bindings_file: Option<PathBuf>,
    #[serde(default)]
    bindings_backup: Option<PathBuf>,
    #[serde(default)]
    hyprland_file: Option<PathBuf>,
    #[serde(default)]
    hyprland_backup: Option<PathBuf>,
}

const BINDINGS_BLOCK: &str = r#"-- >>> muscli managed integration >>>
hl.unbind("SUPER + SHIFT + ALT + M")
o.bind("SUPER + SHIFT + ALT + M", "muscli", "kitty --class muscli-compact --title muscli -e muscli")
hl.unbind("SHIFT + XF86AudioRaiseVolume")
o.bind("SHIFT + XF86AudioRaiseVolume", "muscli volume up", "muscli remote volume up", { locked = true, repeating = true })
hl.unbind("SHIFT + XF86AudioLowerVolume")
o.bind("SHIFT + XF86AudioLowerVolume", "muscli volume down", "muscli remote volume down", { locked = true, repeating = true })
-- <<< muscli managed integration <<<
"#;

const LEGACY_BINDINGS_BLOCK: &str = r#"-- >>> muscli managed integration >>>
hl.unbind("SUPER + SHIFT + ALT + M")
o.bind("SUPER + SHIFT + ALT + M", "muscli compact", "kitty --class muscli-compact --title muscli-compact -e muscli --compact")
hl.unbind("SHIFT + XF86AudioRaiseVolume")
o.bind("SHIFT + XF86AudioRaiseVolume", "muscli volume up", "muscli remote volume up", { locked = true, repeating = true })
hl.unbind("SHIFT + XF86AudioLowerVolume")
o.bind("SHIFT + XF86AudioLowerVolume", "muscli volume down", "muscli remote volume down", { locked = true, repeating = true })
-- <<< muscli managed integration <<<
"#;

const WINDOW_BLOCK: &str = r#"-- >>> muscli player window >>>
o.window({ class = "^muscli-compact$" }, {
  float = true,
  center = true,
  size = { 1280, 800 },
})
-- <<< muscli player window <<<
"#;

const LEGACY_WINDOW_BLOCK: &str = r#"-- >>> muscli compact window >>>
o.window({ class = "^muscli-compact$" }, {
  float = true,
  center = true,
  size = { 760, 520 },
})
-- <<< muscli compact window <<<
"#;

pub fn setup(paths: &AppPaths) -> Result<String> {
    paths.ensure()?;
    let state_path = state_file(paths);
    if state_path.exists() {
        let mut state: SetupState = serde_json::from_slice(&fs::read(&state_path)?)?;
        if !omarchy_media_enabled() {
            enable_media_widget()?;
        }
        if !state.desktop_file.exists() {
            if let Some(parent) = state.desktop_file.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&state.desktop_file, desktop_entry())?;
        } else if let Ok(contents) = fs::read_to_string(&state.desktop_file)
            && [legacy_desktop_entry(), compact_desktop_entry()].contains(&contents.as_str())
        {
            atomic_write(&state.desktop_file, desktop_entry().as_bytes())?;
        }
        install_hyprland(&mut state)?;
        atomic_write(&state_path, &serde_json::to_vec_pretty(&state)?)?;
        return Ok(format!(
            "Omarchy integration already configured; verified {} and compact/global controls",
            state.desktop_file.display()
        ));
    }
    let shell = home_dir()?.join(".config/omarchy/shell.json");
    let backup = if shell.exists() {
        let backup = shell.with_extension(format!(
            "json.muscli-backup.{}",
            Local::now().format("%Y%m%d%H%M%S")
        ));
        fs::copy(&shell, &backup)
            .with_context(|| format!("could not back up {}", shell.display()))?;
        Some(backup)
    } else {
        None
    };
    let widget_was_enabled = omarchy_media_enabled();
    if !widget_was_enabled {
        enable_media_widget()?;
    }
    let desktop_file = home_dir()?.join(".local/share/applications/muscli.desktop");
    if let Some(parent) = desktop_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&desktop_file, desktop_entry())?;
    let mut state = SetupState {
        widget_was_enabled,
        desktop_file: desktop_file.clone(),
        backup_file: backup.clone(),
        bindings_file: None,
        bindings_backup: None,
        hyprland_file: None,
        hyprland_backup: None,
    };
    install_hyprland(&mut state)?;
    atomic_write(&state_path, &serde_json::to_vec_pretty(&state)?)?;
    Ok(format!(
        "Omarchy integration enabled. Desktop entry: {}{}",
        desktop_file.display(),
        backup
            .as_ref()
            .map(|p| format!("; backup: {}", p.display()))
            .unwrap_or_default()
    ))
}

pub fn undo(paths: &AppPaths) -> Result<String> {
    let state_path = state_file(paths);
    if !state_path.exists() {
        return Ok("No muscli Omarchy setup record was found; nothing changed.".into());
    }
    let state: SetupState = serde_json::from_slice(&fs::read(&state_path)?)?;
    let mut preserved = Vec::new();
    if state.desktop_file.exists() {
        if fs::read_to_string(&state.desktop_file).ok().as_deref() == Some(desktop_entry()) {
            fs::remove_file(&state.desktop_file)?;
        } else {
            preserved.push("the modified desktop entry");
        }
    }
    for (file, block, label) in [
        (
            state.bindings_file.as_ref(),
            BINDINGS_BLOCK,
            "the Hyprland bindings",
        ),
        (
            state.hyprland_file.as_ref(),
            WINDOW_BLOCK,
            "the compact window rule",
        ),
    ] {
        if let Some(file) = file
            && file.exists()
            && !remove_exact_block(file, block)?
        {
            preserved.push(label);
        }
    }
    if !state.widget_was_enabled && omarchy_media_enabled() {
        if media_widget_is_intact()? {
            let output = Command::new("omarchy")
                .args(["plugin", "disable", "omarchy.media"])
                .output()?;
            if !output.status.success() {
                anyhow::bail!(
                    "could not disable Omarchy widget: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        } else {
            preserved.push("the media widget because its placement or settings changed");
        }
    }
    if state.bindings_file.is_some() || state.hyprland_file.is_some() {
        validate_hyprland()?;
    }
    fs::remove_file(state_path)?;
    let suffix = if preserved.is_empty() {
        String::new()
    } else {
        format!(" Preserved {}.", preserved.join(" and "))
    };
    Ok(format!(
        "Undid the intact muscli Omarchy integration. Backups were preserved.{suffix}"
    ))
}

fn enable_media_widget() -> Result<()> {
    let output = Command::new("omarchy")
        .args([
            "plugin",
            "enable",
            "omarchy.media",
            "--section",
            "center",
            "--before",
            "omarchy.clock",
        ])
        .output()
        .context("could not run Omarchy; is it installed?")?;
    if !output.status.success() {
        anyhow::bail!(
            "Omarchy rejected the media widget: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn media_widget_is_intact() -> Result<bool> {
    let shell = home_dir()?.join(".config/omarchy/shell.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(shell)?)?;
    Ok(media_widget_is_intact_value(&value))
}

fn media_widget_is_intact_value(value: &serde_json::Value) -> bool {
    let Some(center) = value
        .pointer("/bar/layout/center")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    center.windows(2).any(|pair| {
        pair[0].as_object().is_some_and(|entry| {
            entry.len() == 1
                && entry.get("id").and_then(serde_json::Value::as_str) == Some("omarchy.media")
        }) && pair[1].get("id").and_then(serde_json::Value::as_str) == Some("omarchy.clock")
    })
}

fn desktop_entry() -> &'static str {
    "[Desktop Entry]\nType=Application\nName=muscli\nGenericName=FLAC Music Player\nComment=Browse and play a local FLAC library\nIcon=audio-x-generic\nExec=kitty --class muscli-compact --title muscli -e muscli\nTerminal=false\nCategories=Audio;Music;Player;AudioVideo;\nMimeType=audio/flac;\nKeywords=FLAC;music;terminal;\n"
}

fn compact_desktop_entry() -> &'static str {
    "[Desktop Entry]\nType=Application\nName=muscli\nGenericName=FLAC Music Player\nComment=Browse and play a local FLAC library\nIcon=audio-x-generic\nExec=kitty --class muscli-compact --title muscli-compact -e muscli --compact\nTerminal=false\nCategories=Audio;Music;Player;AudioVideo;\nMimeType=audio/flac;\nKeywords=FLAC;music;terminal;\n"
}

fn legacy_desktop_entry() -> &'static str {
    "[Desktop Entry]\nType=Application\nName=muscli\nGenericName=FLAC Music Player\nComment=Browse and play a local FLAC library\nIcon=audio-x-generic\nExec=xdg-terminal-exec -- muscli\nTerminal=false\nCategories=Audio;Music;Player;AudioVideo;\nMimeType=audio/flac;\nKeywords=FLAC;music;terminal;\n"
}

fn install_hyprland(state: &mut SetupState) -> Result<()> {
    let config = home_dir()?.join(".config/hypr");
    let bindings = config.join("bindings.lua");
    let hyprland = config.join("hyprland.lua");
    let bindings_backup =
        install_managed_block(&bindings, BINDINGS_BLOCK, &[LEGACY_BINDINGS_BLOCK])?;
    let hyprland_backup =
        match install_managed_block(&hyprland, WINDOW_BLOCK, &[LEGACY_WINDOW_BLOCK]) {
            Ok(backup) => backup,
            Err(error) => {
                if let Some(backup) = &bindings_backup {
                    let _ = fs::copy(backup, &bindings);
                }
                return Err(error);
            }
        };
    let installed_bindings = bindings_backup.is_some();
    let installed_window = hyprland_backup.is_some();
    state.bindings_file = Some(bindings.clone());
    state.hyprland_file = Some(hyprland.clone());
    if state.bindings_backup.is_none() {
        state.bindings_backup = bindings_backup.clone();
    }
    if state.hyprland_backup.is_none() {
        state.hyprland_backup = hyprland_backup.clone();
    }
    if let Err(error) = validate_hyprland() {
        if installed_bindings && let Some(backup) = &bindings_backup {
            let _ = fs::copy(backup, &bindings);
        }
        if installed_window && let Some(backup) = &hyprland_backup {
            let _ = fs::copy(backup, &hyprland);
        }
        let _ = Command::new("hyprctl").arg("reload").output();
        return Err(error);
    }
    Ok(())
}

fn install_managed_block(
    path: &PathBuf,
    block: &str,
    legacy_blocks: &[&str],
) -> Result<Option<PathBuf>> {
    let mut contents =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    if contents.contains(block) {
        return Ok(None);
    }
    let legacy = legacy_blocks
        .iter()
        .find(|legacy| contents.contains(**legacy))
        .copied();
    if (contents.contains(">>> muscli") || contents.contains("<<< muscli")) && legacy.is_none() {
        anyhow::bail!(
            "{} contains a modified muscli block; preserved it for manual review",
            path.display()
        );
    }
    let backup = path.with_extension(format!(
        "lua.muscli-backup.{}",
        Local::now().format("%Y%m%d%H%M%S%3f")
    ));
    fs::copy(path, &backup).with_context(|| format!("could not back up {}", path.display()))?;
    if let Some(legacy) = legacy {
        contents = contents.replace(legacy, block);
    } else {
        if !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push('\n');
        contents.push_str(block);
    }
    atomic_write(path, contents.as_bytes())?;
    Ok(Some(backup))
}

fn remove_exact_block(path: &PathBuf, block: &str) -> Result<bool> {
    let contents = fs::read_to_string(path)?;
    if !contents.contains(block) {
        return Ok(false);
    }
    let updated = contents
        .replace(&format!("\n{block}"), "")
        .replace(block, "");
    atomic_write(path, updated.as_bytes())?;
    Ok(true)
}

fn validate_hyprland() -> Result<()> {
    let reload = Command::new("hyprctl")
        .arg("reload")
        .output()
        .context("could not reload Hyprland")?;
    if !reload.status.success() {
        anyhow::bail!(
            "Hyprland reload failed: {}",
            String::from_utf8_lossy(&reload.stderr).trim()
        );
    }
    let errors = Command::new("hyprctl")
        .arg("configerrors")
        .output()
        .context("could not validate Hyprland configuration")?;
    if !errors.status.success() || !errors.stdout.iter().all(u8::is_ascii_whitespace) {
        anyhow::bail!(
            "Hyprland reported configuration errors: {}{}",
            String::from_utf8_lossy(&errors.stdout).trim(),
            String::from_utf8_lossy(&errors.stderr).trim()
        );
    }
    Ok(())
}

fn atomic_write(path: &PathBuf, contents: &[u8]) -> Result<()> {
    let temp = path.with_extension("muscli.tmp");
    fs::write(&temp, contents)?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn state_file(paths: &AppPaths) -> PathBuf {
    paths.data_dir.join("omarchy-setup.json")
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognizes_only_the_intact_widget_placement() {
        let intact = json!({"bar":{"layout":{"center":[
            {"id":"omarchy.media"}, {"id":"omarchy.clock"}
        ]}}});
        let moved = json!({"bar":{"layout":{"center":[
            {"id":"omarchy.clock"}, {"id":"omarchy.media"}
        ]}}});
        let customized = json!({"bar":{"layout":{"center":[
            {"id":"omarchy.media", "custom":true}, {"id":"omarchy.clock"}
        ]}}});
        assert!(media_widget_is_intact_value(&intact));
        assert!(!media_widget_is_intact_value(&moved));
        assert!(!media_widget_is_intact_value(&customized));
    }
}
