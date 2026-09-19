//! Decoding audio files to floating point.
//!
//! Everything downstream of here works in `f32`, which is the only sane common
//! ground: the library holds 16-, 24- and 32-bit material at several sample
//! rates, and a chain that had to know which was which would branch in every
//! stage. Converting once, at the source, costs nothing measurable and makes
//! the rest of the path exact — see the round-trip test, which decodes a
//! generated 16-bit stream and gets every sample back unchanged.

use std::{fs::File, path::Path};

use anyhow::{Context, Result, anyhow};
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::{MediaSourceStream, MediaSourceStreamOptions},
    meta::MetadataOptions,
    units::Time,
};

/// What a decoded stream is, as the file itself declares it.
///
/// `bits_per_sample` is carried even though the samples are floats by this
/// point: dither has to know how many bits the output will be squeezed into,
/// and a bit-perfect path has to know what it is claiming to be perfect about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: Option<u32>,
}

/// A file being decoded to interleaved `f32`.
pub struct Decoder {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    spec: StreamSpec,
    /// The track's timebase, as a fraction of a second per tick. Positions are
    /// kept in frames rather than milliseconds because a seek lands on a frame
    /// and rounding it to the nearest millisecond loses up to 44 samples --
    /// enough to make "carry on from here" read the wrong ones.
    time_base: (u64, u64),
    block: Vec<f32>,
    /// Where the last seek landed, in frames from the start of the track.
    base_frames: u64,
    frames_since_seek: u64,
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

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&parameters, &AudioDecoderOptions::default())
            .with_context(|| format!("no decoder for the codec in {}", path.display()))?;

        Ok(Self {
            reader,
            decoder,
            track_id,
            spec,
            time_base,
            block: Vec::new(),
            base_frames: 0,
            frames_since_seek: 0,
        })
    }

    /// What the stream currently is.
    ///
    /// Read this after every block rather than once: a few containers change
    /// rate or channel count mid-stream, and the chain has to follow.
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

    /// Decode the next block, or `None` at the end of the stream.
    ///
    /// The samples are interleaved by channel. Packets that fail to decode are
    /// skipped rather than fatal: a single corrupt frame in the middle of an
    /// album should cost a click, not the rest of the track.
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
                    let frames = decoded.frames();
                    if frames == 0 {
                        continue;
                    }
                    // Fields are borrowed apart here on purpose: `decoded`
                    // borrows the decoder, the block it fills is a different
                    // field, and going through a method would borrow both.
                    self.spec.sample_rate = decoded.spec().rate();
                    self.spec.channels = decoded.num_planes() as u16;
                    decoded.copy_to_vec_interleaved(&mut self.block);
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
        let landed = self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from_millis_u64(position_ms),
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
        Ok(())
    }
}
