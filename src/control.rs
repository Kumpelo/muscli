use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::sync::mpsc::{self as tokio_mpsc, UnboundedReceiver};
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, Stream, ToFsName, prelude::*,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteCommand {
    VolumeUp,
    VolumeDown,
    VolumeSet(u8),
    MuteToggle,
    Rescan,
    Prune,
}

pub struct ControlServer {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    socket: PathBuf,
}

impl ControlServer {
    pub fn start(socket: &Path) -> Result<(Self, UnboundedReceiver<RemoteCommand>)> {
        if socket.exists() {
            fs::remove_file(socket).ok();
        }
        let name = socket.to_fs_name::<GenericFilePath>()?;
        let listener = ListenerOptions::new()
            .name(name)
            .nonblocking(ListenerNonblockingMode::Accept)
            .create_sync()
            .with_context(|| format!("could not bind control socket {}", socket.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        }
        let (tx, rx) = tokio_mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("muscli-control".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok(mut stream) => {
                            let mut raw = String::new();
                            if stream.read_to_string(&mut raw).is_ok()
                                && let Some(command) = parse(raw.trim())
                            {
                                let _ = tx.send(command);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok((
            Self {
                stop,
                worker: Some(worker),
                socket: socket.to_path_buf(),
            },
            rx,
        ))
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.socket);
    }
}

pub fn send(socket: &Path, command: RemoteCommand) -> Result<bool> {
    let name = socket.to_fs_name::<GenericFilePath>()?;
    let mut stream = match Stream::connect(name) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    let message = match command {
        RemoteCommand::VolumeUp => "volume up".into(),
        RemoteCommand::VolumeDown => "volume down".into(),
        RemoteCommand::VolumeSet(value) => format!("volume set {value}"),
        RemoteCommand::MuteToggle => "mute toggle".into(),
        RemoteCommand::Rescan => "library rescan".into(),
        RemoteCommand::Prune => "library prune".into(),
    };
    stream.write_all(message.as_bytes())?;
    Ok(true)
}

fn parse(value: &str) -> Option<RemoteCommand> {
    match value {
        "volume up" => Some(RemoteCommand::VolumeUp),
        "volume down" => Some(RemoteCommand::VolumeDown),
        "mute toggle" => Some(RemoteCommand::MuteToggle),
        "library rescan" => Some(RemoteCommand::Rescan),
        "library prune" => Some(RemoteCommand::Prune),
        _ => value
            .strip_prefix("volume set ")?
            .parse::<u8>()
            .ok()
            .filter(|value| *value <= 100)
            .map(RemoteCommand::VolumeSet),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_remote_volume_commands() {
        assert_eq!(parse("volume up"), Some(RemoteCommand::VolumeUp));
        assert_eq!(parse("volume set 55"), Some(RemoteCommand::VolumeSet(55)));
        assert_eq!(parse("volume set 101"), None);
    }

    #[test]
    fn remote_command_round_trip() {
        #[cfg(unix)]
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let socket = temporary.path().join("control.sock");
        #[cfg(windows)]
        let socket = PathBuf::from(format!(
            r"\\.\pipe\muscli-test-control-{}",
            std::process::id()
        ));
        let (server, receiver) = ControlServer::start(&socket).unwrap();
        assert!(send(&socket, RemoteCommand::VolumeSet(45)).unwrap());
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            RemoteCommand::VolumeSet(45)
        );
        drop(server);
    }
}
