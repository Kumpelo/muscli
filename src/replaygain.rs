use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

use anyhow::{Context, Result};

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
    Error(String),
    Done,
}

pub fn start(
    candidates: Vec<(String, PathBuf, u64, i64)>,
    target_lufs: f64,
) -> Receiver<GainMessage> {
    let (tx, rx) = mpsc::channel();
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
                        let _ =
                            tx.send(GainMessage::Error(format!("{}: {error:#}", path.display())));
                    }
                }
            }
            let _ = tx.send(GainMessage::Done);
        })
        .ok();
    rx
}

pub fn analyze(path: &Path, target_lufs: f64) -> Result<ReplayGainAnalysis> {
    let output = ffmpeg_command()
        .arg("-nostdin")
        .arg("-hide_banner")
        .arg("-i")
        .arg(path)
        .args(["-filter_complex", "ebur128=peak=true", "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .context("could not run ffmpeg loudness analysis")?;
    if !output.status.success() {
        anyhow::bail!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("unknown error")
        );
    }
    parse_summary(&String::from_utf8_lossy(&output.stderr), target_lufs)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ebur128_summary() {
        let output = "Summary:\n  Integrated loudness:\n    I:         -13.5 LUFS\n  True peak:\n    Peak:       -0.8 dBFS\n";
        let result = parse_summary(output, -18.0).unwrap();
        assert_eq!(result.gain_db, -4.5);
        assert_eq!(result.true_peak_db, -0.8);
    }
}
