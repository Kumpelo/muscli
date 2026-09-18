//! Small filesystem helpers shared across modules.

use std::{fs, path::Path};

use anyhow::Result;

/// Replace `target` with `source`, atomically where the platform allows it.
///
/// On Unix a rename over an existing path is atomic. Windows refuses that, so
/// the obvious remove-then-rename is used instead - except it leaves a window
/// where the target does not exist, and a concurrent reader sees nothing.
/// MoveFileExW with MOVEFILE_REPLACE_EXISTING has no such window.
#[cfg(unix)]
pub fn atomic_replace(source: &Path, target: &Path) -> Result<()> {
    fs::rename(source, target)?;
    Ok(())
}

#[cfg(windows)]
pub fn atomic_replace(source: &Path, target: &Path) -> Result<()> {
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };

    let source: Vec<u16> = source
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let target: Vec<u16> = target
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }?;
    Ok(())
}
