//! The real device.
//!
//! Nothing in here can be exercised by a test in this repository -- there is
//! no sound card in a build container -- which is exactly why it contains as
//! little logic as possible. Deciding what to do with a short ring lives in
//! [`super::sink::fill`], which the capture output uses too; what is left here
//! is opening a device and copying.

use std::sync::{Arc, atomic::Ordering};

use anyhow::{Context, Result, anyhow, bail};
use cpal::{
    BufferSize, FromSample, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedStreamConfigRange,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::Consumer;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    audio::native::sink::{Output, SinkState, fill},
    model::PlayerEvent,
};

/// The rates worth asking a device about.
///
/// A device advertises ranges, not a list, and a range says nothing about
/// what is actually useful. These are the rates music is distributed at,
/// which is the only list that matters here.
const KNOWN_RATES: [u32; 12] = [
    8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
    384_000,
];

/// Scratch space for formats that are not `f32`, in frames.
///
/// Sized once, when the stream opens, because the callback may not allocate.
/// A device asking for more than a second in one call does not exist; if one
/// did, it would get silence and a starvation count rather than a malloc.
const SCRATCH_FRAMES: usize = 48_000;

pub struct CpalOutput {
    device: cpal::Device,
    name: String,
    configs: Vec<SupportedStreamConfigRange>,
    stream: Option<Stream>,
    events: UnboundedSender<PlayerEvent>,
}

impl CpalOutput {
    /// Open the named device, or the system default.
    pub fn open(preferred: Option<&str>, events: UnboundedSender<PlayerEvent>) -> Result<Self> {
        let host = cpal::default_host();
        let device = match preferred {
            Some(wanted) => host
                .output_devices()
                .context("could not list the output devices")?
                .find(|device| device.to_string() == wanted)
                .ok_or_else(|| anyhow!("no output device called {wanted}"))?,
            None => host
                .default_output_device()
                .ok_or_else(|| anyhow!("there is no default output device"))?,
        };

        let name = device.to_string();
        let configs = device
            .supported_output_configs()
            .with_context(|| format!("could not ask {name} what it supports"))?
            .collect();

        Ok(Self {
            device,
            name,
            configs,
            stream: None,
            events,
        })
    }

    /// Every output device, by name, for `muscli devices`.
    pub fn devices() -> Result<Vec<String>> {
        Ok(cpal::default_host()
            .output_devices()
            .context("could not list the output devices")?
            .map(|device| device.to_string())
            .collect())
    }

    /// The best format this device offers for a stream.
    ///
    /// `f32` first: it is what the chain already holds, so taking it means no
    /// conversion and no rounding at all. Failing that, the widest integer,
    /// because the wider the destination the less the rounding costs.
    fn pick(&self, sample_rate: u32, channels: u16) -> Option<SampleFormat> {
        let mut best: Option<SampleFormat> = None;
        for config in &self.configs {
            if config.channels() != channels || !config.contains_rate(sample_rate) {
                continue;
            }
            let format = config.sample_format();
            let rank = |format: SampleFormat| match format {
                SampleFormat::F32 => 3,
                SampleFormat::I32 => 2,
                SampleFormat::I16 => 1,
                _ => 0,
            };
            if rank(format) > 0 && best.is_none_or(|current| rank(format) > rank(current)) {
                best = Some(format);
            }
        }
        best
    }

    fn build<T>(
        &self,
        config: StreamConfig,
        mut frames: Consumer<f32>,
        state: Arc<SinkState>,
    ) -> Result<Stream>
    where
        T: SizedSample + FromSample<f32> + Send + 'static,
    {
        let channels = config.channels;
        let mut scratch = vec![0.0f32; SCRATCH_FRAMES * usize::from(channels.max(1))];
        let starved = Arc::clone(&state);
        let complaints = self.events.clone();

        self.device
            .build_output_stream::<T, _, _>(
                config,
                move |buffer: &mut [T], _| {
                    if buffer.len() > scratch.len() {
                        // Never grow here. A device this greedy is broken,
                        // and a reallocation in the callback would be worse.
                        buffer.fill(T::from_sample(0.0));
                        starved.starved.fetch_add(
                            buffer.len() as u64 / u64::from(channels.max(1)),
                            Ordering::Relaxed,
                        );
                        return;
                    }
                    let staging = &mut scratch[..buffer.len()];
                    fill(staging, &mut frames, &starved, channels);
                    for (out, sample) in buffer.iter_mut().zip(staging.iter()) {
                        *out = T::from_sample(*sample);
                    }
                },
                // A notice, not an error: the application answers an error by
                // skipping the track, and a device that hiccups has not said
                // anything about the file being played.
                move |error| {
                    let _ = complaints.send(PlayerEvent::Notice(error.to_string()));
                },
                None,
            )
            .context("the device refused the stream")
    }

    fn build_float(
        &self,
        config: StreamConfig,
        mut frames: Consumer<f32>,
        state: Arc<SinkState>,
    ) -> Result<Stream> {
        let channels = config.channels;
        let complaints = self.events.clone();
        self.device
            .build_output_stream::<f32, _, _>(
                config,
                move |buffer: &mut [f32], _| fill(buffer, &mut frames, &state, channels),
                move |error| {
                    let _ = complaints.send(PlayerEvent::Notice(error.to_string()));
                },
                None,
            )
            .context("the device refused the stream")
    }
}

impl Output for CpalOutput {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn supports(&self, sample_rate: u32, channels: u16) -> bool {
        self.pick(sample_rate, channels).is_some()
    }

    fn rates(&self, channels: u16) -> Vec<u32> {
        KNOWN_RATES
            .iter()
            .copied()
            .filter(|rate| self.pick(*rate, channels).is_some())
            .collect()
    }

    fn channel_counts(&self) -> Vec<u16> {
        let mut counts: Vec<u16> = self
            .configs
            .iter()
            .map(|config| config.channels())
            .collect();
        counts.sort_unstable();
        counts.dedup();
        counts
    }

    fn start(
        &mut self,
        sample_rate: u32,
        channels: u16,
        frames: Consumer<f32>,
        state: Arc<SinkState>,
    ) -> Result<()> {
        // Dropping the old stream first closes the device; some drivers will
        // not hand out a second one while the first is open.
        self.stream = None;

        let Some(format) = self.pick(sample_rate, channels) else {
            bail!(
                "{} cannot play {sample_rate} Hz in {channels} channels",
                self.name
            );
        };
        let config = StreamConfig {
            channels,
            sample_rate,
            buffer_size: BufferSize::Default,
        };

        let stream = match format {
            SampleFormat::F32 => self.build_float(config, frames, state)?,
            SampleFormat::I32 => self.build::<i32>(config, frames, state)?,
            SampleFormat::I16 => self.build::<i16>(config, frames, state)?,
            other => bail!("{} asked for {other}, which is not supported", self.name),
        };
        stream.play().context("the device would not start")?;
        self.stream = Some(stream);
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) -> Result<()> {
        let Some(stream) = &self.stream else {
            return Ok(());
        };
        if paused {
            stream.pause().context("the device would not pause")
        } else {
            stream.play().context("the device would not resume")
        }
    }

    fn stop(&mut self) {
        self.stream = None;
    }
}
