use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;

use crate::model::ReplayGainAnalysis;

#[derive(Debug)]
pub enum GainMessage {
    Result {
        track_id: String,
        result: ReplayGainAnalysis,
        size: u64,
        modified: i64,
        completed: usize,
        total: usize,
    },
    Error {
        track_id: String,
        message: String,
        size: u64,
        modified: i64,
        completed: usize,
        total: usize,
    },
    Done,
}

pub fn start(
    candidates: Vec<(String, PathBuf, u64, i64)>,
    target_lufs: f64,
    tx: UnboundedSender<GainMessage>,
) {
    thread::Builder::new()
        .name("muscli-replaygain".into())
        .spawn(move || {
            let total = candidates.len();
            for (index, (track_id, path, size, modified)) in candidates.into_iter().enumerate() {
                match analyze(&path, target_lufs) {
                    Ok(result) => {
                        if tx
                            .send(GainMessage::Result {
                                track_id,
                                result,
                                size,
                                modified,
                                completed: index + 1,
                                total,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        if tx
                            .send(GainMessage::Error {
                                track_id,
                                message: format!("{}: {error:#}", path.display()),
                                size,
                                modified,
                                completed: index + 1,
                                total,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            let _ = tx.send(GainMessage::Done);
        })
        .ok();
}

const FFMPEG_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_FFMPEG_LOG: usize = 128 * 1024;

/// Keep only the end of stderr: EBU R128's summary is emitted at EOF.
/// Reading concurrently avoids blocking FFmpeg when its pipe fills.
fn collect_stderr_tail(mut stderr: impl Read) -> std::io::Result<Vec<u8>> {
    let mut tail = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let count = stderr.read(&mut buf)?;
        if count == 0 {
            return Ok(tail);
        }
        let excess = (tail.len() + count).saturating_sub(MAX_FFMPEG_LOG);
        if excess > 0 {
            tail.drain(..excess);
        }
        tail.extend_from_slice(&buf[..count]);
    }
}

fn run_ffmpeg(mut command: Command) -> Result<(ExitStatus, Vec<u8>)> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run ffmpeg loudness analysis")?;
    let stderr = child.stderr.take().context("ffmpeg stderr unavailable")?;
    let reader = thread::spawn(move || collect_stderr_tail(stderr));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= FFMPEG_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let stderr = reader
        .join()
        .map_err(|_| anyhow::anyhow!("ffmpeg stderr reader panicked"))??;
    let status = status.context("ffmpeg analysis timed out after 600 seconds")?;
    Ok((status, stderr))
}

pub fn analyze(path: &Path, target_lufs: f64) -> Result<ReplayGainAnalysis> {
    let mut command = ffmpeg_command();
    command
        .arg("-nostdin")
        .arg("-hide_banner")
        .arg("-nostats")
        // Do not let an audio container resolve remote protocols during analysis.
        .args(["-protocol_whitelist", "file,pipe"])
        .arg("-i")
        .arg(path)
        .args(["-filter_complex", "ebur128=peak=true", "-f", "null", "-"]);
    let (status, stderr) = run_ffmpeg(command)?;
    if !status.success() {
        anyhow::bail!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&stderr)
                .lines()
                .last()
                .unwrap_or("unknown error")
        );
    }
    parse_summary(&String::from_utf8_lossy(&stderr), target_lufs)
        .context("ffmpeg did not return an EBU R128 summary")
}

#[cfg(unix)]
fn ffmpeg_command() -> Command {
    let mut command = Command::new("ionice");
    command.args(["-c", "3", "nice", "-n", "10", "ffmpeg"]);
    command
}

#[cfg(windows)]
fn ffmpeg_command() -> Command {
    use std::os::windows::process::CommandExt;
    use windows::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;

    let bundled = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("ffmpeg.exe")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("ffmpeg"));
    let mut command = Command::new(bundled);
    command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS.0);
    command
}

fn parse_summary(output: &str, target_lufs: f64) -> Option<ReplayGainAnalysis> {
    let mut integrated = None;
    let mut peak = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("I:") {
            integrated = first_number(value);
        } else if let Some(value) = trimmed.strip_prefix("Peak:") {
            peak = first_number(value);
        }
    }
    let integrated = integrated?;
    let true_peak_db = peak.unwrap_or(0.0);
    Some(ReplayGainAnalysis {
        gain_db: target_lufs - integrated,
        true_peak_db,
    })
}

fn first_number(value: &str) -> Option<f64> {
    value
        .split_whitespace()
        .next()
        .and_then(|number| number.parse().ok())
}

/// Write the analysis into the file's own tags.
///
/// This is the only thing muscli does that modifies a user's audio files, so
/// it never happens as part of a scan or of analysis: it is a separate command
/// that has to be asked for. Existing tags with these names are replaced;
/// nothing else in the file is touched.
pub fn write_tags(path: &Path, analysis: ReplayGainAnalysis) -> Result<()> {
    use lofty::{
        config::WriteOptions,
        file::TaggedFileExt,
        prelude::{ItemKey, TagExt},
        probe::Probe,
        tag::Tag,
    };

    let tagged = Probe::open(path)
        .with_context(|| format!("could not open {}", path.display()))?
        .guess_file_type()?
        .read()?;
    // Prefer an existing tag that this container can actually write. A
    // tagless file gets the container's primary writable tag type (ID3v2 for
    // MP3/AAC, Vorbis comments for FLAC/Ogg/Opus, MP4 ilst for MP4, etc.).
    let primary_tag_type = tagged.primary_tag_type();
    let mut tag = tagged
        .tags()
        .iter()
        .find(|tag| tagged.tag_support(tag.tag_type()).is_writable())
        .cloned()
        .unwrap_or_else(|| Tag::new(primary_tag_type));

    // The ReplayGain 1.0 field names, in the units the specification defines.
    tag.insert_text(
        ItemKey::ReplayGainTrackGain,
        format!("{:.2} dB", analysis.gain_db),
    );
    tag.insert_text(
        ItemKey::ReplayGainTrackPeak,
        format!("{:.6}", 10f64.powf(analysis.true_peak_db / 20.0)),
    );

    tag.save_to_path(path, WriteOptions::default())
        .with_context(|| format!("could not write tags to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_capture_is_bounded_and_preserves_final_summary() {
        let data = format!("{}{}", "noise".repeat(50_000), "Summary:\nI: -13.5 LUFS\nPeak: -0.8 dBFS\n");
        let output = collect_stderr_tail(data.as_bytes()).unwrap();
        assert!(output.len() <= MAX_FFMPEG_LOG);
        assert!(String::from_utf8_lossy(&output).ends_with("Peak: -0.8 dBFS\n"));
    }

    #[test]
    fn parses_ebur128_summary() {
        let output = "Summary:\n  Integrated loudness:\n    I:         -13.5 LUFS\n  True peak:\n    Peak:       -0.8 dBFS\n";
        let result = parse_summary(output, -18.0).unwrap();
        assert_eq!(result.gain_db, -4.5);
        assert_eq!(result.true_peak_db, -0.8);
    }
}
