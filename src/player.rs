use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use interprocess::TryClone;
use interprocess::local_socket::{GenericFilePath, Stream, ToFsName, prelude::*};
use serde_json::{Value, json};
use tokio::sync::mpsc::{self as tokio_mpsc, UnboundedReceiver};

use crate::model::PlayerEvent;

pub struct MpvPlayer {
    child: Option<Child>,
    writer: Arc<Mutex<Stream>>,
    events: UnboundedReceiver<PlayerEvent>,
    position_ms: Arc<AtomicU64>,
    socket: std::path::PathBuf,
}

impl MpvPlayer {
    pub fn start(socket: &Path) -> Result<Self> {
        if socket.exists() {
            fs::remove_file(socket).ok();
        }
        let mut child = Command::new(mpv_binary())
            .arg("--no-config")
            .arg("--terminal=no")
            .arg("--idle=yes")
            .arg("--no-video")
            .arg("--audio-display=no")
            .arg("--gapless-audio=yes")
            .arg("--keep-open=no")
            .arg(format!("--input-ipc-server={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context(mpv_install_help())?;

        let deadline = Instant::now() + Duration::from_secs(3);
        let stream = loop {
            let name = socket.to_fs_name::<GenericFilePath>()?;
            match Stream::connect(name) {
                Ok(stream) => break stream,
                Err(_error) if Instant::now() < deadline => {
                    if child.try_wait()?.is_some() {
                        anyhow::bail!("mpv exited before creating its IPC socket");
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error).context("timed out connecting to mpv IPC"),
            }
        };

        let reader = stream.try_clone()?;
        let writer = Arc::new(Mutex::new(stream));
        let (tx, events) = tokio_mpsc::unbounded_channel();
        let position_ms = Arc::new(AtomicU64::new(0));
        let event_position_ms = position_ms.clone();
        thread::Builder::new()
            .name("muscli-mpv-events".into())
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if let Some(event) = parse_event(&value) {
                        match event {
                            PlayerEvent::Position(value) => {
                                event_position_ms.store(value, Ordering::Relaxed);
                            }
                            event => {
                                let _ = tx.send(event);
                            }
                        }
                    }
                }
            })?;

        let player = Self {
            child: Some(child),
            writer,
            events,
            position_ms,
            socket: socket.to_path_buf(),
        };
        for (id, property) in [
            (1, "time-pos"),
            (2, "duration"),
            (3, "pause"),
            (4, "volume"),
        ] {
            player.command(json!(["observe_property", id, property]))?;
        }
        Ok(player)
    }

    pub fn command(&self, command: Value) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("mpv writer lock poisoned"))?;
        serde_json::to_writer(&mut *writer, &json!({"command": command}))?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    pub fn load(&self, path: &Path, position_ms: u64) -> Result<()> {
        self.position_ms.store(position_ms, Ordering::Relaxed);
        self.command(json!(["loadfile", path.to_string_lossy(), "replace"]))?;
        if position_ms > 0 {
            self.command(json!([
                "seek",
                position_ms as f64 / 1000.0,
                "absolute",
                "exact"
            ]))?;
        }
        Ok(())
    }

    pub fn pause(&self, paused: bool) -> Result<()> {
        self.command(json!(["set_property", "pause", paused]))
    }

    pub fn toggle(&self) -> Result<()> {
        self.command(json!(["cycle", "pause"]))
    }

    pub fn seek_relative(&self, seconds: f64) -> Result<()> {
        let current = self.position_ms.load(Ordering::Relaxed) as i128;
        let delta = (seconds * 1000.0) as i128;
        self.position_ms
            .store((current + delta).max(0).min(u64::MAX as i128) as u64, Ordering::Relaxed);
        self.command(json!(["seek", seconds, "relative", "exact"]))
    }

    pub fn seek_absolute_ms(&self, position_ms: u64) -> Result<()> {
        self.position_ms.store(position_ms, Ordering::Relaxed);
        self.command(json!([
            "seek",
            position_ms as f64 / 1000.0,
            "absolute",
            "exact"
        ]))
    }

    pub fn set_volume(&self, volume: f64) -> Result<()> {
        self.command(json!([
            "set_property",
            "volume",
            volume.clamp(0.0, 1.0) * 100.0
        ]))
    }

    pub fn set_replay_gain(&self, gain_db: Option<f64>) -> Result<()> {
        let _ = self.command(json!(["af", "remove", "@muscli_replaygain"]));
        if let Some(gain_db) = gain_db {
            self.command(json!([
                "af",
                "add",
                format!("@muscli_replaygain:volume=volume={gain_db:.3}dB")
            ]))?;
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.position_ms.store(0, Ordering::Relaxed);
        self.command(json!(["stop"]))
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms.load(Ordering::Relaxed)
    }

    pub fn try_event(&mut self) -> Option<PlayerEvent> {
        self.events.try_recv().ok()
    }

    pub async fn recv_event(&mut self) -> Option<PlayerEvent> {
        self.events.recv().await
    }
}

fn mpv_binary() -> std::path::PathBuf {
    #[cfg(windows)]
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let bundled = directory.join("mpv.exe");
        if bundled.is_file() {
            return bundled;
        }
    }
    std::path::PathBuf::from("mpv")
}

#[cfg(unix)]
fn mpv_install_help() -> &'static str {
    "could not start mpv; install it with `omarchy pkg add mpv`"
}

#[cfg(windows)]
fn mpv_install_help() -> &'static str {
    "could not start mpv; reinstall muscli or add mpv.exe to PATH"
}

fn parse_event(value: &Value) -> Option<PlayerEvent> {
    match value.get("event")?.as_str()? {
        "property-change" => {
            let name = value.get("name")?.as_str()?;
            let data = value.get("data");
            match name {
                "time-pos" => data?
                    .as_f64()
                    .map(|v| PlayerEvent::Position((v.max(0.0) * 1000.0) as u64)),
                "duration" => data?
                    .as_f64()
                    .map(|v| PlayerEvent::Duration((v.max(0.0) * 1000.0) as u64)),
                "pause" => data?.as_bool().map(PlayerEvent::Paused),
                "volume" => data?
                    .as_f64()
                    .map(|v| PlayerEvent::Volume((v / 100.0).clamp(0.0, 1.0))),
                _ => None,
            }
        }
        "end-file" if value.get("reason").and_then(Value::as_str) == Some("eof") => {
            Some(PlayerEvent::EndOfFile)
        }
        "end-file" if value.get("reason").and_then(Value::as_str) == Some("error") => {
            Some(PlayerEvent::Error(
                value
                    .get("file_error")
                    .and_then(Value::as_str)
                    .unwrap_or("mpv playback error")
                    .to_owned(),
            ))
        }
        _ => None,
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = self.command(json!(["quit"]));
        if let Some(mut child) = self.child.take() {
            for _ in 0..10 {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        let _ = fs::remove_file(&self.socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_property_events() {
        let value = json!({"event":"property-change","name":"time-pos","data":12.5});
        assert!(matches!(
            parse_event(&value),
            Some(PlayerEvent::Position(12500))
        ));
        let value = json!({"event":"end-file","reason":"eof"});
        assert!(matches!(parse_event(&value), Some(PlayerEvent::EndOfFile)));
    }
}
