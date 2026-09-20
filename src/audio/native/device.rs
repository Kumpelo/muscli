//! The cpal output.
//!
//! Untestable here, since a build container has no sound card, so it holds as
//! little logic as possible: what to do with a short ring lives in
//! [`super::sink::fill`], which the capture output shares.

use std::sync::{Arc, atomic::Ordering};

use anyhow::{Context, Result, anyhow, bail};
use cpal::{
    BufferSize, FromSample, I24, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedStreamConfigRange, U24,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::Consumer;
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    audio::{
        decode::StreamSpec,
        dsp::{dither::Dither, gain::Gain},
        native::sink::{Output, OutputFormat, SinkState, choose_channels, choose_rate, fill},
    },
    model::PlayerEvent,
};

/// The rates worth asking a device about. cpal advertises ranges, so these
/// are the points within them that music is actually distributed at.
const KNOWN_RATES: [u32; 18] = [
    8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 64_000, 88_200, 96_000,
    176_400, 192_000, 352_800, 384_000, 705_600, 768_000,
];

/// Sample formats worth taking, best first.
///
/// `f32` needs no conversion at all, since that is what the chain holds. After
/// it the widest integer, because the wider the destination the less the
/// rounding costs -- and 24 bits before 16 is most of the difference.
const FORMATS: [(SampleFormat, Option<u32>); 5] = [
    (SampleFormat::F32, None),
    (SampleFormat::I32, Some(32)),
    (SampleFormat::I24, Some(24)),
    (SampleFormat::U24, Some(24)),
    (SampleFormat::I16, Some(16)),
];

/// Scratch space for formats that are not `f32`, in frames. Sized once at
/// open, because the callback may not allocate; a device asking for more than
/// this gets silence rather than a reallocation.
const SCRATCH_FRAMES: usize = 48_000;

pub struct CpalOutput {
    device: cpal::Device,
    name: String,
    configs: Vec<SupportedStreamConfigRange>,
    stream: Option<Stream>,
    /// Whether dither on an integer output is shaped out of the band the ear
    /// is most sensitive in.
    noise_shaping: bool,
    events: UnboundedSender<PlayerEvent>,
}

impl CpalOutput {
    /// Open the named device, or the system default.
    pub fn open(
        preferred: Option<&str>,
        noise_shaping: bool,
        events: UnboundedSender<PlayerEvent>,
    ) -> Result<Self> {
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
            noise_shaping,
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

    /// The best format this device offers for a stream, and its depth.
    fn pick(&self, sample_rate: u32, channels: u16) -> Option<(SampleFormat, Option<u32>)> {
        FORMATS.into_iter().find(|(format, _)| {
            self.configs.iter().any(|config| {
                config.channels() == channels
                    && config.contains_rate(sample_rate)
                    && config.sample_format() == *format
            })
        })
    }

    /// Channel counts this device offers.
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

    /// Rates this device offers for a channel count, among the ones music is
    /// actually distributed at.
    fn rates(&self, channels: u16) -> Vec<u32> {
        KNOWN_RATES
            .iter()
            .copied()
            .filter(|rate| self.pick(*rate, channels).is_some())
            .collect()
    }

    fn build<T>(
        &self,
        config: StreamConfig,
        mut frames: Consumer<f32>,
        state: Arc<SinkState>,
        mut dither: Option<Dither>,
    ) -> Result<Stream>
    where
        T: SizedSample + FromSample<f32> + Send + 'static,
    {
        let channels = config.channels;
        let mut scratch = vec![0.0f32; SCRATCH_FRAMES * usize::from(channels.max(1))];
        let mut volume = Gain::new(config.sample_rate, state.volume.get());
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
                    fill(staging, &mut frames, &starved, channels, &mut volume);
                    // After the volume, because quantising and then scaling
                    // would put the samples back between the steps dither
                    // exists to land them on.
                    if let Some(dither) = &mut dither {
                        dither.process(staging, usize::from(channels.max(1)));
                    }
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
        let mut volume = Gain::new(config.sample_rate, state.volume.get());
        let complaints = self.events.clone();
        self.device
            .build_output_stream::<f32, _, _>(
                config,
                move |buffer: &mut [f32], _| {
                    fill(buffer, &mut frames, &state, channels, &mut volume)
                },
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

    fn negotiate(&self, source: StreamSpec) -> Option<OutputFormat> {
        let channels = choose_channels(source.channels.max(1), &self.channel_counts())?;
        let sample_rate = choose_rate(source.sample_rate, &self.rates(channels))?;
        let (_, bits) = self.pick(sample_rate, channels)?;
        Some(OutputFormat {
            sample_rate,
            channels,
            bits,
        })
    }

    fn start(
        &mut self,
        format: OutputFormat,
        frames: Consumer<f32>,
        state: Arc<SinkState>,
    ) -> Result<()> {
        // Dropping the old stream first closes the device; some drivers will
        // not hand out a second one while the first is open.
        self.stream = None;

        let OutputFormat {
            sample_rate,
            channels,
            bits,
        } = format;
        let Some((sample_format, _)) = self.pick(sample_rate, channels) else {
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
        let dither =
            || bits.map(|bits| Dither::new(bits, usize::from(channels.max(1)), self.noise_shaping));

        let stream = match sample_format {
            SampleFormat::F32 => self.build_float(config, frames, state)?,
            SampleFormat::I32 => self.build::<i32>(config, frames, state, dither())?,
            SampleFormat::I24 => self.build::<I24>(config, frames, state, dither())?,
            SampleFormat::U24 => self.build::<U24>(config, frames, state, dither())?,
            SampleFormat::I16 => self.build::<i16>(config, frames, state, dither())?,
            other => bail!("{} asked for {other}, which is not supported", self.name),
        };
        stream.play().context("the device would not start")?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) {
        self.stream = None;
    }
}
