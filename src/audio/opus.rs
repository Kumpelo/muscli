//! Packet decoding only: Ogg probing, metadata and end padding stay in Symphonia.

use ruopus::MultistreamDecoder;
use symphonia::core::{
    audio::{AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef},
    codecs::{
        CodecInfo,
        audio::{AudioCodecParameters, AudioDecoder, FinalizeResult},
    },
    errors::{Error, Result, decode_error, unsupported_error},
    packet::PacketRef,
};

const INFO: CodecInfo = CodecInfo {
    short_name: "opus",
    long_name: "Opus (Rust)",
    profiles: &[],
};
const RATE: u32 = 48_000;

pub(super) struct OpusDecoder {
    params: AudioCodecParameters,
    decoder: MultistreamDecoder,
    streams: usize,
    coupled: usize,
    mapping: Vec<u8>,
    gain: f32,
    pre_skip: u64,
    buf: AudioBuffer<f32>,
}

impl OpusDecoder {
    pub(super) fn new(params: &AudioCodecParameters) -> Result<Self> {
        let head = params
            .extra_data
            .as_deref()
            .ok_or(Error::DecodeError("opus: missing OpusHead"))?;
        if head.len() < 19 || &head[..8] != b"OpusHead" || head[8] > 15 {
            return decode_error("opus: invalid OpusHead");
        }
        let channels = usize::from(head[9]);
        if !(1..=8).contains(&channels) || params.sample_rate != Some(RATE) {
            return unsupported_error("opus: unsupported rate or channel count");
        }
        let positions = params
            .channels
            .clone()
            .ok_or(Error::DecodeError("opus: missing channel layout"))?;
        if positions.count() != channels {
            return decode_error("opus: inconsistent channel count");
        }
        let (streams, coupled, mut mapping) = match head[18] {
            0 if channels <= 2 => (1, channels - 1, (0..channels as u8).collect::<Vec<_>>()),
            1 if head.len() >= 21 + channels => {
                let streams = usize::from(head[19]);
                let coupled = usize::from(head[20]);
                let mapping = head[21..21 + channels].to_vec();
                // Limit internal decoding work as well as output channels.
                // Ogg can map hundreds of hidden streams to one output.
                if streams == 0
                    || coupled > streams
                    || streams + coupled > channels
                    || mapping
                        .iter()
                        .any(|&m| m != 255 && usize::from(m) >= streams + coupled)
                {
                    return decode_error("opus: invalid stream mapping");
                }
                (streams, coupled, mapping)
            }
            _ => return unsupported_error("opus: unsupported or truncated channel mapping"),
        };
        // Family 1 uses Vorbis order; the device and Symphonia use WAVE order.
        let order: &[usize] = match channels {
            3 => &[0, 2, 1],
            5 => &[0, 2, 1, 3, 4],
            6 => &[0, 2, 1, 5, 3, 4],
            7 => &[0, 2, 1, 6, 5, 3, 4],
            8 => &[0, 2, 1, 7, 5, 6, 3, 4],
            _ => &[],
        };
        if !order.is_empty() {
            mapping = order.iter().map(|&i| mapping[i]).collect();
        }
        let gain_db = f32::from(i16::from_le_bytes([head[16], head[17]])) / 256.0;
        Ok(Self {
            params: params.clone(),
            decoder: MultistreamDecoder::with_rate(RATE, streams, coupled, &mapping),
            streams,
            coupled,
            mapping,
            gain: 10.0_f32.powf(gain_db / 20.0),
            pre_skip: u64::from(u16::from_le_bytes([head[10], head[11]])),
            buf: AudioBuffer::new(AudioSpec::new(RATE, positions), 5760),
        })
    }

    pub(super) fn pre_skip(&self) -> u64 {
        self.pre_skip
    }
}

impl AudioDecoder for OpusDecoder {
    fn codec_info(&self) -> &CodecInfo {
        &INFO
    }
    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }
    fn reset(&mut self) {
        self.decoder =
            MultistreamDecoder::with_rate(RATE, self.streams, self.coupled, &self.mapping);
        self.buf.clear();
    }
    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        self.buf.clear();
        if packet.data.is_empty() {
            return decode_error("opus: empty packet");
        }
        let mut pcm = self
            .decoder
            .decode_packet(packet.data)
            .map_err(|_| Error::DecodeError("opus: invalid packet"))?;
        let frames = pcm.len() / self.mapping.len();
        if frames > 5760 || pcm.iter().any(|s| !s.is_finite()) {
            return decode_error("opus: invalid decoded audio");
        }
        for sample in pcm.iter_mut() {
            *sample *= self.gain;
        }
        self.buf.render_uninit(Some(frames));
        self.buf.copy_from_slice_interleaved(&pcm);
        // Symphonia 0.6 Ogg timestamps include pre-skip. Derive the remaining
        // delay from the packet timestamp so it spans packets and seeks safely.
        let delay = self.pre_skip.saturating_sub(packet.pts.get().max(0) as u64);
        self.buf.trim(
            packet.trim_start.get().max(delay) as usize,
            packet.trim_end.get() as usize,
        );
        Ok(self.buf.as_generic_audio_buffer_ref())
    }
    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }
    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}
