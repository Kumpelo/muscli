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
use tokio::sync::mpsc::UnboundedSender;

use crate::model::PlayerEvent;

use super::{AudioBackend, Capabilities};

pub struct MpvPlayer {
    child: Option<Child>,
    writer: Arc<Mutex<Stream>>,
    position_ms: Arc<AtomicU64>,
    socket: std::path::PathBuf,
}

impl MpvPlayer {
    pub fn start(socket: &Path, events: UnboundedSender<PlayerEvent>) -> Result<Self> {
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
            // Let mpv open the next playlist entry before the current one ends;
            // without this, gapless only applies to files already demuxed.
            .arg("--prefetch-playlist=yes")
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
        let tx = events;
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
            position_ms,
            socket: socket.to_path_buf(),
        };
        for (id, property) in [
            (1, "time-pos"),
            (2, "duration"),
            (3, "pause"),
            (4, "volume"),
            (5, "playlist-pos"),
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

    /// Queue `path` to play straight after the current file. Appended rather
    /// than loaded, so mpv opens it early; loading on end-of-file cannot be
    /// seamless, because the round trip through this process is the gap.
    pub fn set_prefetch(&self, path: &Path) -> Result<()> {
        self.clear_prefetch()?;
        self.command(json!(["loadfile", path.to_string_lossy(), "append"]))
    }

    /// Drop anything queued after the current file. Failure means there was
    /// nothing queued, which is the desired state.
    pub fn clear_prefetch(&self) -> Result<()> {
        let _ = self.command(json!(["playlist-remove", 1]));
        Ok(())
    }

    /// Discard the entry that just finished, making the playing file entry 0
    /// again so a new one can be queued behind it.
    pub fn drop_finished_entry(&self) -> Result<()> {
        let _ = self.command(json!(["playlist-remove", 0]));
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
        self.position_ms.store(
            (current + delta).max(0).min(u64::MAX as i128) as u64,
            Ordering::Relaxed,
        );
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

    /// Apply a graphic equaliser, on the same labelled-filter mechanism as
    /// ReplayGain so the two stack. An empty set removes the filter rather
    /// than installing a flat one.
    pub fn set_equalizer(&self, bands: &[(u32, f32)]) -> Result<()> {
        let _ = self.command(json!(["af", "remove", "@muscli_eq"]));
        let active: Vec<String> = bands
            .iter()
            .filter(|(_, gain)| gain.abs() >= 0.1)
            .map(|(frequency, gain)| {
                // width_type=o means the width is in octaves, which is what
                // makes a fixed set of bands sound even across the spectrum.
                format!("equalizer=f={frequency}:width_type=o:width=1:gain={gain:.1}")
            })
            .collect();
        if active.is_empty() {
            return Ok(());
        }
        self.command(json!([
            "af",
            "add",
            format!("@muscli_eq:lavfi=[{}]", active.join(","))
        ]))
    }

    pub fn stop(&self) -> Result<()> {
        self.position_ms.store(0, Ordering::Relaxed);
        self.command(json!(["stop"]))
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms.load(Ordering::Relaxed)
    }
}

impl AudioBackend for MpvPlayer {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "mpv",
            replay_gain: true,
            equalizer: true,
            volume: true,
            gapless: true,
            // mpv decodes and converts on its own terms; there is no way to
            // ask it for the file's samples and nothing else.
            bit_perfect: false,
            rolls_into_prefetch: true,
        }
    }

    fn load(&mut self, path: &Path, position_ms: u64) -> Result<()> {
        MpvPlayer::load(self, path, position_ms)
    }

    fn set_prefetch(&mut self, path: Option<&Path>) -> Result<()> {
        match path {
            Some(path) => MpvPlayer::set_prefetch(self, path),
            None => MpvPlayer::clear_prefetch(self),
        }
    }

    fn adopt_prefetch(&mut self) -> Result<()> {
        // mpv is playing the second playlist entry; dropping the first makes
        // the playing one entry zero again, so the playlist never grows.
        MpvPlayer::drop_finished_entry(self)
    }

    fn pause(&mut self, paused: bool) -> Result<()> {
        MpvPlayer::pause(self, paused)
    }

    fn toggle(&mut self) -> Result<()> {
        MpvPlayer::toggle(self)
    }

    fn seek_relative(&mut self, seconds: f64) -> Result<()> {
        MpvPlayer::seek_relative(self, seconds)
    }

    fn seek_absolute_ms(&mut self, position_ms: u64) -> Result<()> {
        MpvPlayer::seek_absolute_ms(self, position_ms)
    }

    fn set_volume(&mut self, volume: f64) -> Result<()> {
        MpvPlayer::set_volume(self, volume)
    }

    fn set_replay_gain(&mut self, gain_db: Option<f64>) -> Result<()> {
        MpvPlayer::set_replay_gain(self, gain_db)
    }

    fn set_equalizer(&mut self, bands: &[(u32, f32)]) -> Result<()> {
        MpvPlayer::set_equalizer(self, bands)
    }

    fn stop(&mut self) -> Result<()> {
        MpvPlayer::stop(self)
    }

    fn position_ms(&self) -> u64 {
        MpvPlayer::position_ms(self)
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
                "playlist-pos" => data?.as_i64().map(PlayerEvent::PlaylistPosition),
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
