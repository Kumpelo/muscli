use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result};
use fs4::FileExt;

pub struct InstanceGuard {
    file: File,
}

impl InstanceGuard {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        FileExt::try_lock(&file).context("muscli is already running")?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self { file })
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
