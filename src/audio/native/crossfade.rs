//! The native engine's optional, bounded live crossfade.
//!
//! This is driven on the decoding thread, not in the hardware callback.
//! It mixes the next decoder directly into the outgoing samples before
//! the existing DSP chain, then hands over the *advanced* next decoder
//! at the outgoing track's normal EOF. No intermediate WAV or full song
//! buffers are needed.

use crate::audio::{
    decode::Decoder,
    mix::{MixCurve, blend_gains},
};

/// Holds only one decoded packet of the next track.
#[derive(Default)]
pub(super) struct CrossfadeBuffer {
    pending: Vec<f32>,
    offset: usize,
    active: bool,
    abandoned: bool,
}

impl CrossfadeBuffer {
    pub fn reset(&mut self) {
        self.pending.clear();
        self.offset = 0;
        self.active = false;
        self.abandoned = false;
    }

    fn next_frame(&mut self, decoder: &mut Decoder, channels: usize) -> Option<&[f32]> {
        while self.offset + channels > self.pending.len() {
            let samples = match decoder.next_block() {
                Ok(Some(samples)) => samples,
                _ => {
                    self.abandoned = true;
                    return None;
                }
            };
            self.pending.clear();
            self.pending.extend_from_slice(samples);
            self.offset = 0;
            if self.pending.is_empty() {
                continue;
            }
        }
        let start = self.offset;
        self.offset += channels;
        Some(&self.pending[start..start + channels])
    }

    /// Overlay incoming audio only when the exact crossfade start is reached.
    /// A late prefetch never starts halfway through a fade.
    ///
    /// Return the number of frames that contained both tracks.
    pub fn apply(
        &mut self,
        outgoing: &mut [f32],
        first_source_frame: u64,
        source_duration_ms: Option<u64>,
        source_rate: u32,
        channels: usize,
        next: &mut Decoder,
        seconds: f64,
        curve: MixCurve,
    ) -> usize {
        if self.abandoned || seconds <= 0.0 || source_rate == 0 || channels == 0 {
            return 0;
        }
        let Some(duration_ms) = source_duration_ms else {
            return 0;
        };
        let total_frames = duration_ms.saturating_mul(u64::from(source_rate)) / 1000;
        let wanted_frames = (seconds * f64::from(source_rate)).round() as u64;
        if wanted_frames < 2 || wanted_frames >= total_frames {
            return 0;
        }
        let start = total_frames - wanted_frames;
        let block_end = first_source_frame + (outgoing.len() / channels) as u64;
        if block_end <= start || first_source_frame >= total_frames {
            return 0;
        }
        // If the next decoder was armed too late, or the track was seeked
        // straight into its fade window, prefer the established gapless path.
        if !self.active && first_source_frame > start {
            return 0;
        }

        let mut mixed = 0;
        for (i, frame) in outgoing.chunks_exact_mut(channels).enumerate() {
            let position = first_source_frame + i as u64;
            if position < start || position >= total_frames {
                continue;
            }
            if !self.active {
                if position != start {
                    break;
                }
                self.active = true;
            }
            let Some(incoming) = self.next_frame(next, channels) else {
                break;
            };
            let progress = (position - start) as f64 / (wanted_frames - 1) as f64;
            let (a, b) = blend_gains(curve, progress);
            for (sample, next_sample) in frame.iter_mut().zip(incoming.iter()) {
                *sample = *sample * a + *next_sample * b;
            }
            mixed += 1;
        }
        mixed
    }
}

