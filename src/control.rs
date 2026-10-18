use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use interprocess::local_socket::{
    ConnectOptions, GenericFilePath, ListenerNonblockingMode, ListenerOptions, Stream, ToFsName,
    prelude::*,
};
use tokio::sync::mpsc::{self as tokio_mpsc, UnboundedReceiver};

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
    snapshot: Arc<RwLock<Arc<Vec<u8>>>>,
}

impl ControlServer {
    pub fn start(socket: &Path) -> Result<(Self, UnboundedReceiver<RemoteCommand>)> {
        if socket.exists() {
            fs::remove_file(socket).ok();
        }
        let name = socket.to_fs_name::<GenericFilePath>()?;
        let listener = ListenerOptions::new()
            .name(name)
            .create_sync()
            .with_context(|| format!("could not bind control socket {}", socket.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        }
        listener.set_nonblocking(ListenerNonblockingMode::Both)?;
        let snapshot = Arc::new(RwLock::new(Arc::new(
            b"{\"schema_version\":1,\"available\":false}\n".to_vec(),
        )));
        let published = snapshot.clone();
        let (tx, rx) = tokio_mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("muscli-control".into())
            .spawn(move || {
                let mut clients = Vec::new();
                while !thread_stop.load(Ordering::Relaxed) {
                    // One accept per iteration bounds the work even under a burst.
                    if let Ok(stream) = listener.accept()
                        && clients.len() < 8
                    {
                        clients.push(Client {
                            stream,
                            request: Vec::new(),
                            response: None,
                            written: 0,
                            deadline: Instant::now() + Duration::from_secs(1),
                        });
                    }
                    clients.retain_mut(|client| client.advance(&published, &tx));
                    thread::sleep(Duration::from_millis(5));
                }
            })?;
        Ok((
            Self {
                stop,
                worker: Some(worker),
                socket: socket.to_path_buf(),
                snapshot,
            },
            rx,
        ))
    }
    pub fn publish(&self, value: &impl serde::Serialize) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        anyhow::ensure!(bytes.len() < MAX_RESPONSE, "control snapshot is too large");
        bytes.push(b'\n');
        *self
            .snapshot
            .write()
            .map_err(|_| anyhow::anyhow!("snapshot lock poisoned"))? = Arc::new(bytes);
        Ok(())
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

const MAX_REQUEST: usize = 256;
const MAX_RESPONSE: usize = 65_536;

struct Client {
    stream: Stream,
    request: Vec<u8>,
    response: Option<Arc<Vec<u8>>>,
    written: usize,
    deadline: Instant,
}

impl Client {
    fn advance(
        &mut self,
        snapshot: &RwLock<Arc<Vec<u8>>>,
        tx: &tokio_mpsc::UnboundedSender<RemoteCommand>,
    ) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        if self.response.is_none() {
            let mut bytes = [0; MAX_REQUEST + 1];
            let eof = match self.stream.read(&mut bytes) {
                Ok(n) => {
                    self.request.extend_from_slice(&bytes[..n]);
                    n == 0
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            };
            if self.request.len() > MAX_REQUEST {
                return false;
            }
            if !eof && !self.request.contains(&b'\n') {
                return true;
            }
            let Ok(raw) = std::str::from_utf8(&self.request) else {
                return false;
            };
            if raw.trim() == "status v1" {
                self.response = snapshot.read().ok().map(|value| value.clone());
            } else {
                if let Some(command) = parse(raw.trim()) {
                    let _ = tx.send(command);
                }
                return false;
            }
        }
        let Some(response) = &self.response else {
            return false;
        };
        match self.stream.write(&response[self.written..]) {
            Ok(0) => false,
            Ok(n) => {
                self.written += n;
                self.written < response.len()
            }
            Err(error) => error.kind() == std::io::ErrorKind::WouldBlock,
        }
    }
}

/// Bounded read-only client. It never opens the library or starts a player.
pub fn status(socket: &Path) -> Result<Option<serde_json::Value>> {
    let name = socket.to_fs_name::<GenericFilePath>()?;
    let mut stream: Stream = match ConnectOptions::new()
        .name(name)
        .wait_mode(interprocess::ConnectWaitMode::Timeout(
            Duration::from_millis(500),
        ))
        .nonblocking_stream(true)
        .connect_sync()
    {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    let request = b"status v1\n";
    let mut written = 0;
    let mut response = Vec::new();
    loop {
        anyhow::ensure!(Instant::now() < deadline, "control status timed out");
        if written < request.len() {
            match stream.write(&request[written..]) {
                Ok(0) => anyhow::bail!("control socket closed"),
                Ok(n) => written += n,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            let mut bytes = [0; 4096];
            match stream.read(&mut bytes) {
                Ok(0) => anyhow::bail!("incomplete control snapshot"),
                Ok(n) => {
                    response.extend_from_slice(&bytes[..n]);
                    anyhow::ensure!(
                        response.len() <= MAX_RESPONSE,
                        "control response is too large"
                    );
                    if response.last() == Some(&b'\n') {
                        return Ok(Some(serde_json::from_slice(&response)?));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        thread::sleep(Duration::from_millis(5));
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

    #[cfg(unix)]
    #[test]
    fn snapshots_are_private_bounded_and_not_blocked_by_idle_clients() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("status.sock");
        let (server, _receiver) = ControlServer::start(&socket).unwrap();
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let value = serde_json::json!({"schema_version":1,"available":true,"upcoming":[
            {"occurrence":1,"id":"same","title":"Quoted \"title\""}, {"occurrence":2,"id":"same"}]});
        server.publish(&value).unwrap();
        let idle =
            Stream::connect(socket.as_path().to_fs_name::<GenericFilePath>().unwrap()).unwrap();
        assert_eq!(status(&socket).unwrap(), Some(value.clone()));
        let mut large =
            Stream::connect(socket.as_path().to_fs_name::<GenericFilePath>().unwrap()).unwrap();
        large.write_all(&vec![b'x'; MAX_REQUEST + 1]).unwrap();
        assert_eq!(status(&socket).unwrap(), Some(value.clone()));
        assert!(server.publish(&"x".repeat(MAX_RESPONSE)).is_err());
        assert_eq!(
            status(&socket).unwrap(),
            Some(value),
            "oversized publication keeps the last valid snapshot"
        );
        let start = Instant::now();
        drop(server);
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "idle clients must not hold shutdown open"
        );
        drop(idle);
        assert!(!socket.exists());
    }

    #[test]
    fn status_without_a_running_instance_is_unavailable() {
        #[cfg(unix)]
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let socket = temporary.path().join("missing-control.sock");
        #[cfg(windows)]
        let socket = PathBuf::from(format!(
            r"\\.\pipe\muscli-test-missing-control-{}",
            std::process::id()
        ));
        assert!(
            status(&socket).unwrap().is_none(),
            "an absent control server must not be an operating-system error"
        );
    }

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
        let (server, mut receiver) = ControlServer::start(&socket).unwrap();
        assert!(send(&socket, RemoteCommand::VolumeSet(45)).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let command = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap()
        });
        assert_eq!(command, RemoteCommand::VolumeSet(45));
        drop(server);
    }
}
