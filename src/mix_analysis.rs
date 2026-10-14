//! Incremental, background, library-wide BPM and energy analysis.
//!
//! The decoder never runs on the playback callback. One worker processes
//! candidates sequentially so adding a large library cannot spawn a thread
//! per song or allocate buffers proportional to the whole music collection.
use std::{path::PathBuf, thread, time::Duration};

use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

use crate::audio::mix::{self, TrackAnalysis};

#[derive(Debug)]
pub enum MixAnalysisMessage {
    Result {
        track_id: String,
        analysis: std::result::Result<TrackAnalysis, String>,
        size: u64,
        modified: i64,
        completed: usize,
        total: usize,
    },
    Done,
}

pub fn start(
    candidates: Vec<(String, PathBuf, u64, i64)>,
    tx: UnboundedSender<MixAnalysisMessage>,
) -> Result<()> {
    thread::Builder::new()
        .name("muscli-mix-analysis".into())
        .spawn(move || {
            let total = candidates.len();
            for (index, (track_id, path, size, modified)) in candidates.into_iter().enumerate() {
                let analysis = mix::analyze_track(&path)
                    .map_err(|error| format!("{}: {error:#}", path.display()));
                if tx
                    .send(MixAnalysisMessage::Result {
                        track_id,
                        analysis,
                        size,
                        modified,
                        completed: index + 1,
                        total,
                    })
                    .is_err()
                {
                    return;
                }
                // Yield to playback and scanning, not a burst of decoders.
                thread::sleep(Duration::from_millis(25));
            }
            let _ = tx.send(MixAnalysisMessage::Done);
        })?;
    Ok(())
}
