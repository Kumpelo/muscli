//! Decoding audio files to interleaved `f32`.
//!
//! Converting once here means nothing downstream has to know whether the file
//! was 16, 24 or 32 bit. The conversion is exact: `tests/audio.rs` decodes a
//! generated 16-bit stream and compares every sample.

use std::{fs::File, path::Path};

use anyhow::{Context, Result, anyhow};
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_OPUS},
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    units::Time,
};

/// What a decoded stream is, as the file declares it. `bits_per_sample`
/// survives the conversion to float because dither and bit-perfect need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: Option<u32>,
}

impl StreamSpec {
    pub(crate) fn same_rate_and_channels(self, other: Self) -> bool {
        self.sample_rate == other.sample_rate && self.channels == other.channels
    }
}

/// A file being decoded to interleaved `f32`.
pub struct Decoder {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    spec: StreamSpec,
    /// Numerator and denominator of the track's timebase, in seconds per tick.
    /// Positions are kept in frames: a seek lands on a frame, and rounding to
    /// the nearest millisecond loses up to 44 samples.
    time_base: (u64, u64),
    block: Vec<f32>,
    duration_ms: Option<u64>,
    /// Where the last seek landed, in frames from the start of the track.
    base_frames: u64,
    frames_since_seek: u64,
    opus_pre_skip: Option<u64>,
    seek_target: Option<u64>,
}

impl Decoder {
    /// Open a file and prepare its first audio track for decoding.
    pub fn open(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("could not open {}", path.display()))?;
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

        // The extension is a hint, not a decision: the probe still reads the
        // markers, so a mislabelled file is opened by what it actually is.
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }

        let reader = symphonia::default::get_probe()
            .probe(
                &hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .with_context(|| format!("no decoder for {}", path.display()))?;

        Self::from_reader(reader, path)
    }

    /// How long the track is, when the container says.
    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    fn from_reader(reader: Box<dyn FormatReader>, path: &Path) -> Result<Self> {
        let track = reader
            .first_track_known_codec(TrackType::Audio)
            .ok_or_else(|| anyhow!("{} has no audio track", path.display()))?;
        let track_id = track.id;
        let time_base = track.time_base.unwrap_or_default();
        let time_base = (
            u64::from(time_base.numer.get()),
            u64::from(time_base.denom.get()),
        );
        let parameters = track
            .codec_params
            .as_ref()
            .and_then(|parameters| parameters.audio())
            .ok_or_else(|| anyhow!("{} has no codec parameters", path.display()))?
            .clone();

        let mut duration_ms = track
            .duration
            .map(|duration| duration.get() as u128 * u128::from(time_base.0) * 1_000)
            .map(|scaled| (scaled / u128::from(time_base.1)) as u64);

        let spec = StreamSpec {
            sample_rate: parameters
                .sample_rate
                .ok_or_else(|| anyhow!("{} does not declare a sample rate", path.display()))?,
            channels: parameters
                .channels
                .as_ref()
                .map(|channels| channels.count() as u16)
                .ok_or_else(|| anyhow!("{} does not declare its channels", path.display()))?,
            bits_per_sample: parameters.bits_per_sample,
        };

        let (decoder, opus_pre_skip): (Box<dyn AudioDecoder>, Option<u64>) = if parameters.codec
            == CODEC_ID_OPUS
        {
            let decoder = super::opus::OpusDecoder::new(&parameters)?;
            let pre_skip = decoder.pre_skip();
            duration_ms = track.duration.map(|duration| {
                (u128::from(duration.get().saturating_sub(pre_skip)) * 1_000 / 48_000) as u64
            });
            (Box::new(decoder), Some(pre_skip))
        } else {
            (
                symphonia::default::get_codecs()
                    .make_audio_decoder(&parameters, &AudioDecoderOptions::default())
                    .with_context(|| format!("no decoder for the codec in {}", path.display()))?,
                None,
            )
        };

        Ok(Self {
            reader,
            decoder,
            track_id,
            spec,
            time_base,
            duration_ms,
            block: Vec::new(),
            base_frames: 0,
            frames_since_seek: 0,
            opus_pre_skip,
            seek_target: None,
        })
    }

    /// What the stream currently is. Read it after every block, not once:
    /// some containers change rate or channel count mid-stream, and the
    /// engine renegotiates the device when they do.
    pub fn spec(&self) -> StreamSpec {
        self.spec
    }

    /// How far into the track the next block starts, in frames.
    pub fn position_frames(&self) -> u64 {
        self.base_frames + self.frames_since_seek
    }

    /// The same position, rounded to a millisecond for display.
    pub fn position_ms(&self) -> u64 {
        self.position_frames() * 1_000 / u64::from(self.spec.sample_rate.max(1))
    }

    /// Decode the next block of interleaved samples, or `None` at the end.
    /// A packet that fails to decode is skipped, not fatal.
    pub fn next_block(&mut self) -> Result<Option<&[f32]>> {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(SymphoniaError::IoError(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            if packet.track_id != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let mut frames = decoded.frames();
                    if frames == 0 {
                        continue;
                    }
                    // Fields are borrowed apart here on purpose: `decoded`
                    // borrows the decoder, the block it fills is a different
                    // field, and going through a method would borrow both.
                    self.spec.sample_rate = decoded.spec().rate();
                    self.spec.channels = decoded.num_planes() as u16;
                    decoded.copy_to_vec_interleaved(&mut self.block);
                    if let Some(target) = self.seek_target.take() {
                        let raw_start = packet.pts.get().max(0) as u64 + packet.trim_start.get();
                        let start = raw_start.saturating_sub(self.opus_pre_skip.unwrap_or(0));
                        let discard = target.saturating_sub(start).min(frames as u64) as usize;
                        self.block
                            .drain(..discard * usize::from(self.spec.channels));
                        frames -= discard;
                        if frames == 0 {
                            self.seek_target = Some(target);
                            continue;
                        }
                        self.base_frames = start + discard as u64;
                        self.frames_since_seek = 0;
                    }
                    self.frames_since_seek += frames as u64;
                    return Ok(Some(&self.block));
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Seek so that the next block starts at or just before `position_ms`.
    pub fn seek_ms(&mut self, position_ms: u64) -> Result<()> {
        // Warm up Opus for 250 ms after random access (RFC 7845 requires at least 80 ms).
        // Container time includes pre-skip; the public position does not.
        let seek_ms = self.opus_pre_skip.map_or(position_ms, |pre_skip| {
            (position_ms.saturating_mul(48).saturating_add(pre_skip)).saturating_sub(12_000) / 48
        });
        let landed = self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from_millis_u64(seek_ms),
                track_id: Some(self.track_id),
            },
        )?;
        // A seek leaves the decoder holding state from somewhere else in the
        // stream; carrying it over is what produces the click after a scrub.
        self.decoder.reset();
        self.block.clear();
        let ticks = landed.actual_ts.get().max(0) as u128;
        let (numerator, denominator) = self.time_base;
        self.base_frames = (ticks * u128::from(numerator) * u128::from(self.spec.sample_rate)
            / u128::from(denominator)) as u64;
        self.frames_since_seek = 0;
        if self.opus_pre_skip.is_some() {
            let target = position_ms.saturating_mul(48);
            self.seek_target = Some(target);
            self.base_frames = target;
        }
        Ok(())
    }
}
