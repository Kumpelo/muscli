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
        let runtime_base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            runtime_dir: runtime_base.join("muscli"),
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
        self.runtime_dir.join("mpv.sock")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.runtime_dir.join("instance.lock")
    }

    pub fn control_socket(&self) -> PathBuf {
        self.runtime_dir.join("control.sock")
    }

    pub fn image_protocol_file(&self) -> PathBuf {
        self.data_dir.join("image-protocol.txt")
    }
}
