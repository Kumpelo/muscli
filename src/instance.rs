#[cfg(unix)]
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::Path,
};

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
#[cfg(unix)]
use fs4::FileExt;

#[cfg(unix)]
pub struct InstanceGuard {
    file: File,
}

#[cfg(unix)]
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

#[cfg(unix)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(windows)]
pub struct InstanceGuard {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl InstanceGuard {
    pub fn acquire(_path: &std::path::Path) -> Result<Self> {
        use windows::{
            Win32::{
                Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
                System::Threading::CreateMutexW,
            },
            core::PCWSTR,
        };

        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        let name = format!("Local\\muscli-{user}");
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }?;
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { windows::Win32::Foundation::CloseHandle(handle) }.ok();
            anyhow::bail!("muscli is already running");
        }
        Ok(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::ReleaseMutex(self.handle);
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}
