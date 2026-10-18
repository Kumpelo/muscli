//! Original sine tones encoded with FFmpeg/libopus, with no music or artwork.
//! stereo: 440/880 Hz, 44.1 kHz input, 2.137 s, 96 kb/s; mono: 440 Hz,
//! 16 kHz input, 0.137 s, voip/12 kb/s; surround: 200/300/400/60/500/600 Hz,
//! 48 kHz input, 0.137 s, 256 kb/s. stereo.f32 is the first 50 ms decoded
//! independently by FFmpeg/libopus as little-endian f32 at 48 kHz. mono.f32
//! is the equivalent mono reference; mono-long is a 2 s/48 kHz tone at 48 kb/s.

mod common;

use std::path::{Path, PathBuf};

use muscli::audio::decode::Decoder;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/opus")
        .join(name)
}
fn decode(path: &Path) -> Vec<f32> {
    let mut decoder = Decoder::open(path).unwrap();
    let mut pcm = Vec::new();
    while let Some(block) = decoder.next_block().unwrap() {
        pcm.extend_from_slice(block);
    }
    pcm
}

#[test]
fn opus_uses_48khz_and_trims_encoder_delay_and_padding() {
    for (name, channels, frames) in [("mono.opus", 1, 6576), ("stereo.opus", 2, 102577)] {
        let mut decoder = Decoder::open(&fixture(name)).unwrap();
        assert_eq!(decoder.spec().sample_rate, 48000);
        assert_eq!(decoder.spec().channels, channels);
        assert_eq!(decoder.spec().bits_per_sample, None);
        assert_eq!(decoder.duration_ms(), Some(frames * 1000 / 48000));
        let mut pcm = Vec::new();
        while let Some(block) = decoder.next_block().unwrap() {
            pcm.extend_from_slice(block);
        }
        assert_eq!(pcm.len(), frames as usize * channels as usize);
        assert_eq!(decoder.position_frames(), frames);
        assert!(pcm.iter().all(|v| v.is_finite()));
        assert!(pcm.iter().any(|v| v.abs() > 0.05));
    }
}

#[test]
fn opus_is_detected_by_content() {
    let dir = tempfile::tempdir().unwrap();
    let expected = decode(&fixture("stereo.opus"));
    for name in ["track.ogg", "track.flac", "track"] {
        let path = dir.path().join(name);
        std::fs::copy(fixture("stereo.opus"), &path).unwrap();
        assert_eq!(decode(&path), expected);
    }
}

#[test]
fn seeking_resets_opus_and_matches_continuous_audio_after_preroll() {
    let path = fixture("stereo.opus");
    let expected = decode(&path);
    let mut decoder = Decoder::open(&path).unwrap();
    for position in [1500, 0, 750, 2000, 10] {
        decoder.seek_ms(position).unwrap();
        assert_eq!(decoder.position_ms(), position);
        let mut actual = Vec::new();
        while actual.len() < 4096 {
            let Some(block) = decoder.next_block().unwrap() else {
                break;
            };
            actual.extend_from_slice(block);
        }
        let start = position as usize * 48 * 2;
        let reference = &expected[start..start + actual.len()];
        let error = actual
            .iter()
            .zip(reference)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            / actual.len() as f32;
        assert!(
            error.sqrt() < 0.001,
            "seek {position}: RMS error {}",
            error.sqrt()
        );
    }
}

#[test]
fn rust_opus_matches_independent_libopus_reference() {
    for name in ["stereo", "mono"] {
        let pcm = decode(&fixture(&format!("{name}.opus")));
        let bytes = std::fs::read(fixture(&format!("{name}.f32"))).unwrap();
        let reference: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let error = pcm
            .iter()
            .zip(&reference)
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            / reference.len() as f32;
        assert!(
            error.sqrt() < 0.0001,
            "{name}: libopus RMS error {}",
            error.sqrt()
        );
    }
}

fn rewrite_head(name: &str, change: impl FnOnce(&mut Vec<u8>)) -> (tempfile::TempDir, PathBuf) {
    use ruopus::ogg::{Page, PageWriter};
    let bytes = std::fs::read(fixture(name)).unwrap();
    let (page, used) = Page::parse(&bytes).unwrap();
    let mut head = page.body.to_vec();
    change(&mut head);
    let mut writer = PageWriter::new(page.serial);
    let mut output = Vec::new();
    writer.push(&mut output, &head, 0, false);
    writer.flush(&mut output);
    output.extend_from_slice(&bytes[used..]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.opus");
    std::fs::write(&path, output).unwrap();
    (dir, path)
}

#[test]
fn preskip_can_span_packets_and_gain_is_applied_once() {
    let expected = decode(&fixture("stereo.opus"));
    let (_dir, path) = rewrite_head("stereo.opus", |head| {
        head[10..12].copy_from_slice(&2232_u16.to_le_bytes());
        head[16..18].copy_from_slice(&(-1536_i16).to_le_bytes()); // -6 dB
        head[12..16].copy_from_slice(&0_u32.to_le_bytes()); // input rate is informational
    });
    let actual = decode(&path);
    let expected = &expected[1920 * 2..];
    assert_eq!(actual.len(), expected.len());
    let gain = 10.0_f32.powf(-6.0 / 20.0);
    for (a, b) in actual.iter().zip(expected) {
        assert!((a - b * gain).abs() < 1e-6);
    }
}

#[test]
fn unsupported_and_invalid_headers_are_errors() {
    for head in [
        vec![0; 19],
        b"OpusHead\x01\x00\x00\x00\x80\xbb\x00\x00\x00\x00\x00".to_vec(),
    ] {
        let (_dir, path) = rewrite_head("mono.opus", |h| *h = head);
        assert!(Decoder::open(&path).is_err());
    }
    let (_dir, path) = rewrite_head("stereo.opus", |h| h[18] = 255);
    assert!(Decoder::open(&path).is_err());
    let (_dir, path) = rewrite_head("stereo.opus", |h| {
        h[18] = 1;
        h.extend_from_slice(&[0, 0, 0, 0]);
    });
    assert!(Decoder::open(&path).is_err());
}

#[test]
fn surround_channels_follow_device_order() {
    let path = fixture("surround.opus");
    assert_eq!(Decoder::open(&path).unwrap().spec().channels, 6);
    let pcm = decode(&path);
    assert_eq!(pcm.len(), 6576 * 6);
    for (channel, frequency) in [200.0, 300.0, 400.0, 60.0, 500.0, 600.0]
        .into_iter()
        .enumerate()
    {
        let samples: Vec<f32> = pcm.as_chunks::<6>().0.iter().map(|f| f[channel]).collect();
        let spectrum = muscli::audio::measure::spectrum(&samples[..4096], 48000);
        let found = spectrum.frequency(spectrum.peak_bin());
        assert!(
            (found - frequency).abs() < 12.0,
            "channel {channel}: {found} != {frequency}"
        );
    }
}

#[test]
fn opus_tags_reach_the_library() {
    use muscli::{
        db::Database,
        library::{ScanOptions, scan_to_database},
    };
    let fixture_dir = common::Fixture::new();
    std::fs::copy(
        fixture("stereo.opus"),
        fixture_dir.source().join("tagged.opus"),
    )
    .unwrap();
    let mut db = Database::open(&fixture_dir.paths().database_file()).unwrap();
    let report = scan_to_database(
        &mut db,
        fixture_dir.paths(),
        &[fixture_dir.source()],
        &ScanOptions::default(),
    )
    .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let tracks = db.load_tracks().unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Opus fixture");
    assert_eq!(tracks[0].artist, "Test Artist");
    assert_eq!(tracks[0].album, "Native audio");
}

#[test]
fn opus_joins_native_tracks_without_losing_or_inserting_frames() {
    use muscli::{
        audio::{
            dsp::Settings,
            native::{
                engine::{Command, Engine, Position},
                sink::{CaptureOutput, Volume},
            },
        },
        model::PlayerEvent,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    use tokio::sync::mpsc::unbounded_channel;
    let dir = tempfile::tempdir().unwrap();
    let flac = dir.path().join("next.flac");
    std::fs::write(
        &flac,
        common::flac_bytes(48000, &common::sine(48000, 660.0, 137)),
    )
    .unwrap();
    for (first, second) in [
        (fixture("mono.opus"), flac.clone()),
        (flac.clone(), fixture("mono.opus")),
        (fixture("stereo.opus"), fixture("stereo.opus")),
    ] {
        let expected: Vec<f32> = decode(&first).into_iter().chain(decode(&second)).collect();
        let channels = Decoder::open(&first).unwrap().spec().channels as usize;
        let (output, capture) = CaptureOutput::new(&[48000]);
        let (sender, mut events) = unbounded_channel();
        let mut engine = Engine::new(
            Box::new(output),
            Settings {
                bit_perfect: true,
                ..Settings::default()
            },
            sender,
            Arc::new(Position::default()),
            Arc::new(Volume::default()),
            Arc::new(AtomicBool::new(false)),
        );
        engine.handle(Command::Load {
            path: first,
            position_ms: 0,
        });
        engine.handle(Command::Prefetch(Some(second.clone())));
        let mut actual = Vec::new();
        for _ in 0..(expected.len() / channels / 256 + 10) {
            engine.step();
            actual.extend(capture.pull(256));
            if actual.len() >= expected.len() {
                break;
            }
        }
        assert!(actual.len() >= expected.len());
        let mismatch = actual.iter().zip(&expected).position(|(a, b)| a != b);
        assert!(
            mismatch.is_none(),
            "first mismatch at sample {mismatch:?}, expected {} samples",
            expected.len()
        );
        assert_eq!(capture.starts(), 1);
        let mut transitioned = Vec::new();
        while let Ok(event) = events.try_recv() {
            match event {
                PlayerEvent::Error(message) => panic!("{message}"),
                PlayerEvent::PrefetchedTrack(path) => transitioned.push(path),
                _ => {}
            }
        }
        assert_eq!(transitioned, vec![second]);
    }
}

#[test]
fn opus_mix_carries_incoming_replaygain_into_the_next_track() {
    use muscli::{
        audio::{
            dsp::Settings,
            mix::{MixCurve, blend_gains},
            native::{
                engine::{Command, Engine, Position},
                sink::{CaptureOutput, Volume},
            },
        },
        model::PlayerEvent,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    let first = fixture("mono-long.opus");
    let dir = tempfile::tempdir().unwrap();
    let next = dir.path().join("incoming.flac");
    std::fs::write(
        &next,
        common::flac_bytes(48000, &common::sine(48000, 660.0, 500)),
    )
    .unwrap();
    let mut expected = decode(&first);
    let incoming = decode(&next);
    let overlap = 3840;
    let start = expected.len() - overlap;
    let gain = 10.0_f32.powf(-6.0 / 20.0);
    for i in 0..overlap {
        let (a, b) = blend_gains(MixCurve::Smooth, i as f64 / (overlap - 1) as f64);
        expected[start + i] = expected[start + i] * a + incoming[i] * b * gain;
    }
    expected.extend(incoming[overlap..].iter().map(|s| s * gain));
    let (output, capture) = CaptureOutput::new(&[48000]);
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        Arc::new(Position::default()),
        Arc::new(Volume::default()),
        Arc::new(AtomicBool::new(false)),
    );
    engine.handle(Command::Crossfade {
        seconds: 0.08,
        curve: MixCurve::Smooth,
    });
    engine.handle(Command::Load {
        path: first,
        position_ms: 0,
    });
    engine.handle(Command::PrefetchWithGain {
        path: Some(next.clone()),
        gain_db: Some(-6.0),
    });
    let mut actual = Vec::new();
    for _ in 0..(expected.len() / 256 + 10) {
        engine.step();
        actual.extend(capture.pull(256));
        if actual.len() >= expected.len() + 96 {
            break;
        }
    }
    // The limiter holds its final look-ahead at EOF; compare through the
    // handover and incoming body, excluding that existing terminal tail.
    let error = actual[96..]
        .iter()
        .zip(&expected[..expected.len() - 256])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(error < 0.005, "MIX/ReplayGain maximum error: {error}");
    assert_eq!(capture.starts(), 1);
    let mut handovers = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            PlayerEvent::Error(message) => panic!("{message}"),
            PlayerEvent::PrefetchedTrack(path) => handovers.push(path),
            _ => {}
        }
    }
    assert_eq!(handovers, vec![next]);
}

#[test]
#[ignore = "requires FFmpeg with the standard audio encoders"]
fn existing_native_codecs_still_decode_without_mpv() {
    for (extension, codec, format) in [
        ("flac", "flac", "flac"),
        ("mp3", "libmp3lame", "mp3"),
        ("aac", "aac", "adts"),
        ("m4a", "alac", "ipod"),
        ("m4a", "aac", "ipod"),
        ("ogg", "libvorbis", "ogg"),
        ("wav", "pcm_s16le", "wav"),
        ("aiff", "pcm_s16be", "aiff"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("tone.{extension}"));
        let result = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=0.137",
                "-c:a",
                codec,
                "-f",
                format,
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{extension}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let pcm = decode(&path);
        assert!(pcm.len() >= 6000, "{extension}: too few samples");
        assert!(pcm.iter().all(|s| s.is_finite()));
        let spectrum = muscli::audio::measure::spectrum(&pcm[..4096], 48000);
        let found = spectrum.frequency(spectrum.peak_bin());
        assert!((found - 440.0).abs() < 12.0, "{extension}: {found} Hz");
    }
}

#[test]
fn malformed_audio_packets_are_skipped_without_panicking() {
    use ruopus::ogg::{PacketReader, Page, PageWriter};
    let bytes = std::fs::read(fixture("mono.opus")).unwrap();
    let (page, _) = Page::parse(&bytes).unwrap();
    let mut packets = PacketReader::new(&bytes, page.serial);
    let mut writer = PageWriter::new(page.serial);
    let mut output = Vec::new();
    for _ in 0..2 {
        writer.push(&mut output, &packets.next().unwrap().data, 0, false);
        writer.flush(&mut output);
    }
    // Code 3 claims 63 frames, beyond the allowed 120 ms packet duration.
    writer.push(&mut output, &[0xff, 0x3f], 960, true);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.opus");
    std::fs::write(&path, output).unwrap();
    let mut decoder = Decoder::open(&path).unwrap();
    assert!(decoder.next_block().unwrap().is_none());
}

fn silent_multistream_opus(path: &Path, streams: u8, granule: u64) {
    use ruopus::ogg::PageWriter;
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, 1, 0, 0]);
    head.extend_from_slice(&48000_u32.to_le_bytes());
    head.extend_from_slice(&[0, 0, 1, streams, 0, 0]);
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&[0; 8]);
    let mut packet = Vec::new();
    for _ in 1..streams {
        packet.extend_from_slice(&[0xfb, 6, 0]);
    }
    packet.extend_from_slice(&[0xfb, 6]);
    let mut writer = PageWriter::new(1);
    let mut bytes = Vec::new();
    writer.push(&mut bytes, &head, 0, false);
    writer.flush(&mut bytes);
    writer.push(&mut bytes, &tags, 0, false);
    writer.flush(&mut bytes);
    writer.push(&mut bytes, &packet, granule, true);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn hidden_streams_cannot_exceed_the_native_decoding_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many-streams.opus");
    for streams in [2, 8, 32, 255] {
        silent_multistream_opus(&path, streams, 5760);
        assert!(
            Decoder::open(&path).is_err(),
            "accepted {streams} internal streams"
        );
    }
    silent_multistream_opus(&path, 1, 5760);
    assert_eq!(decode(&path).len(), 5760);
}

#[test]
fn extreme_granule_positions_do_not_overflow_duration_scaling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large-granule.opus");
    let granule = i64::MAX as u64;
    silent_multistream_opus(&path, 1, granule);
    let decoder = Decoder::open(&path).unwrap();
    assert_eq!(
        decoder.duration_ms(),
        Some((u128::from(granule) * 1000 / 48000) as u64)
    );
}
