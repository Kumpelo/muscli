use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("org", "muscli", "muscli")
            .context("could not determine XDG directories")?;
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .map(|base| base.join("muscli"))
            .unwrap_or_else(|| dirs.cache_dir().join("runtime"));
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            runtime_dir,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.runtime_dir,
            &self.cover_cache_dir(),
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.runtime_dir, fs::Permissions::from_mode(0o700))
                .with_context(|| {
                    format!(
                        "could not restrict runtime directory {}",
                        self.runtime_dir.display()
                    )
                })?;
        }
        Ok(())
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join("library.db")
    }

    pub fn cover_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("covers")
    }

    pub fn mpv_socket(&self) -> PathBuf {
        ipc_path("mpv", &self.runtime_dir)
    }

    pub fn lock_file(&self) -> PathBuf {
        self.runtime_dir.join("instance.lock")
    }

    pub fn control_socket(&self) -> PathBuf {
        ipc_path("control", &self.runtime_dir)
    }

    pub fn image_protocol_file(&self) -> PathBuf {
        self.data_dir.join("image-protocol.txt")
    }
}

#[cfg(unix)]
fn ipc_path(name: &str, runtime_dir: &std::path::Path) -> PathBuf {
    runtime_dir.join(format!("{name}.sock"))
}

#[cfg(windows)]
fn ipc_path(name: &str, _runtime_dir: &std::path::Path) -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let safe_user: String = user
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect();
    PathBuf::from(format!(r"\\.\pipe\muscli-{name}-{safe_user}"))
}
