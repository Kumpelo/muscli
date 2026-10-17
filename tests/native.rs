//! The native playback path, end to end: a file on disk, decoded, processed,
//! through the ring, and out of a device that hands its samples to the test
//! instead of to a listener.

mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

use muscli::{
    audio::{
        decode::Decoder,
        dsp::Settings,
        native::{
            engine::{Command, Engine, Position},
            sink::{CaptureHandle, CaptureOutput, Volume},
        },
    },
    model::PlayerEvent,
};
use tempfile::TempDir;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const RATE: u32 = 44_100;
const BLOCK: usize = 1024;

struct Harness {
    engine: Engine,
    capture: CaptureHandle,
    events: UnboundedReceiver<PlayerEvent>,
    position: Arc<Position>,
    _directory: TempDir,
    path: PathBuf,
}

fn harness(millis: u32, rates: &[u32]) -> Harness {
    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("tone.flac");
    fs::write(
        &path,
        common::flac_bytes(RATE, &common::sine(RATE, 440.0, millis)),
    )
    .expect("write the fixture");

    let (output, capture) = CaptureOutput::new(rates);
    let (sender, events) = unbounded_channel();
    let position = Arc::new(Position::default());
    let engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        Arc::clone(&position),
        Arc::new(Volume::default()),
        Arc::new(AtomicBool::new(false)),
    );

    Harness {
        engine,
        capture,
        events,
        position,
        _directory: directory,
        path,
    }
}

fn decoded(path: &Path) -> Vec<f32> {
    let mut decoder = Decoder::open(path).expect("open the fixture");
    let mut samples = Vec::new();
    while let Some(block) = decoder.next_block().expect("decode") {
        samples.extend_from_slice(block);
    }
    samples
}

fn errors(events: &mut UnboundedReceiver<PlayerEvent>) -> Vec<String> {
    let mut found = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::Error(message) = event {
            found.push(message);
        }
    }
    found
}

#[test]
fn unavailable_prefetch_reports_a_notice_without_aborting_current_audio() {
    let mut harness = harness(2_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    let missing = harness.path.with_file_name("next-not-found.flac");
    harness.engine.handle(Command::Prefetch(Some(missing)));

    let mut noticed = false;
    while let Ok(event) = harness.events.try_recv() {
        match event {
            PlayerEvent::Notice(message) => {
                noticed |= message.contains("Gapless prefetch unavailable");
            }
            PlayerEvent::Error(message) => {
                panic!("prefetch must not emit a fatal playback error: {message}");
            }
            _ => {}
        }
    }
    assert!(noticed, "the failure must still be reported to the user");
    harness.engine.step();
    let samples = harness.capture.pull(BLOCK);
    assert!(
        samples.iter().any(|sample| *sample != 0.0),
        "the active track must keep playing when the next one cannot be decoded"
    );
}

#[test]
fn what_reaches_the_device_is_what_was_in_the_file() {
    // Neutral settings, so anything that differs is the path itself losing or
    // altering samples rather than the processing doing its job.
    let mut harness = harness(500, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    assert!(errors(&mut harness.events).is_empty());

    let source = decoded(&harness.path);
    let mut captured = Vec::new();
    while captured.len() < source.len() {
        harness.engine.step();
        captured.extend(harness.capture.pull(BLOCK));
    }

    // The limiter's look-ahead delays everything by a fixed couple of
    // milliseconds; past that the two have to agree exactly.
    let latency = (RATE as usize * 2) / 1_000;
    let compared = source.len() - latency - BLOCK;
    assert_eq!(&captured[latency..latency + compared], &source[..compared]);
}

#[test]
fn a_device_at_another_rate_gets_the_file_converted_to_it() {
    // The device is opened at the file's rate wherever it can be; when it
    // cannot, the conversion happens here rather than being left to whatever
    // sound server would otherwise do it out of sight.
    let mut harness = harness(1_000, &[48_000]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    assert!(errors(&mut harness.events).is_empty());

    let mut captured = Vec::new();
    while captured.len() < 3 * 32_768 {
        harness.engine.step();
        captured.extend(harness.capture.pull(BLOCK));
    }

    // A 440 Hz tone is still a 440 Hz tone at the other rate. Getting this
    // wrong is the failure that sounds like the record being played fast.
    let window = &captured[32_768..2 * 32_768];
    let spectrum = muscli::audio::measure::spectrum(window, 48_000);
    let found = spectrum.frequency(spectrum.peak_bin());
    assert!(
        (found - 440.0).abs() < 2.0,
        "a 440 Hz tone came out at {found:.1} Hz"
    );
}

#[test]
fn a_mono_file_reaches_both_channels_of_a_stereo_device() {
    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("tone.flac");
    fs::write(
        &path,
        common::flac_bytes(RATE, &common::sine(RATE, 440.0, 500)),
    )
    .expect("write the fixture");

    let (output, capture) = CaptureOutput::with_channels(&[RATE], &[2]);
    let (sender, mut events) = unbounded_channel();
    let position = Arc::new(Position::default());
    let mut engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        position,
        Arc::new(Volume::default()),
        Arc::new(AtomicBool::new(false)),
    );

    engine.handle(Command::Load {
        path: path.clone(),
        position_ms: 0,
    });
    assert!(errors(&mut events).is_empty());
    engine.step();

    let captured = capture.pull(BLOCK);
    assert_eq!(captured.len(), BLOCK * 2, "the device was not given stereo");

    let source = decoded(&path);
    let latency = (RATE as usize * 2) / 1_000;
    for (frame, pair) in captured
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .skip(latency)
        .take(256)
    {
        assert_eq!(pair[0], pair[1], "the two channels differ at frame {frame}");
        assert_eq!(pair[0], source[frame - latency]);
    }
}

#[test]
fn the_position_follows_the_device_and_not_the_decoder() {
    // The decoder runs half a second ahead of what is being heard. A position
    // taken from it would show the track finishing before it had.
    let mut harness = harness(2_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });

    harness.engine.step();
    assert_eq!(
        harness.position.ms(),
        0,
        "the position moved before anything was played"
    );

    // Play a quarter of a second.
    for _ in 0..(RATE as usize / 4 / BLOCK) {
        harness.capture.pull(BLOCK);
        harness.engine.step();
    }
    let position = harness.position.ms();
    assert!(
        position.abs_diff(250) < 40,
        "a quarter of a second in, the position reads {position} ms"
    );
}

#[test]
fn a_seek_keeps_the_device_and_costs_one_buffer() {
    // The output discards what it holds instead of the stream being rebuilt
    // around a fresh ring, so the seek costs the one buffer it was filling.
    let mut harness = harness(3_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    assert_eq!(harness.capture.starts(), 1);

    harness.engine.handle(Command::SeekAbsolute(1_500));
    harness.engine.step();
    assert_eq!(
        harness.capture.starts(),
        1,
        "the seek reopened the device instead of flushing it"
    );
    assert!(errors(&mut harness.events).is_empty());

    // The buffer that carries out the flush comes out silent, and only that
    // one: the engine decoded ahead while it waited.
    let landing = harness.capture.pull(BLOCK);
    assert!(
        landing.iter().all(|sample| *sample == 0.0),
        "the old position was played after the seek"
    );
    harness.engine.step();

    let mut captured = Vec::new();
    for _ in 0..8 {
        captured.extend(harness.capture.pull(BLOCK));
        harness.engine.step();
    }
    assert!(
        captured[..BLOCK].iter().any(|sample| *sample != 0.0),
        "the seek cost more than the one buffer"
    );

    // And the samples are the ones that live there, not merely a plausible
    // number of them.
    let mut decoder = Decoder::open(&harness.path).expect("open the fixture");
    decoder.seek_ms(1_500).expect("seek");
    let mut expected = Vec::new();
    while expected.len() < captured.len() {
        let block = decoder.next_block().expect("decode").expect("more audio");
        expected.extend_from_slice(block);
    }

    let latency = (RATE as usize * 2) / 1_000;
    let compared = captured.len() - latency - BLOCK;
    assert_eq!(
        &captured[latency..latency + compared],
        &expected[..compared]
    );

    // A seek is not the decoder falling behind, so it is not reported as one.
    let notices: Vec<String> = std::iter::from_fn(|| harness.events.try_recv().ok())
        .filter_map(|event| match event {
            PlayerEvent::Notice(message) => Some(message),
            _ => None,
        })
        .collect();
    assert!(notices.is_empty(), "the seek complained: {notices:?}");
}

#[test]
fn rapid_consecutive_seeks_land_on_the_latest_requested_position() {
    let mut harness = harness(8_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });

    // Several 5-second and shorter scrubs can arrive before the output has
    // acknowledged the previous flush. Only the final position may be heard.
    for wanted in [5_000, 0, 5_000, 1_000, 6_500, 2_000, 5_500] {
        harness.engine.handle(Command::SeekAbsolute(wanted));
    }
    assert_eq!(harness.capture.starts(), 1, "seek reopened the device");
    assert!(harness.position.ms().abs_diff(5_500) < 100);

    harness.engine.step();
    assert!(
        harness
            .capture
            .pull(BLOCK)
            .iter()
            .all(|sample| *sample == 0.0),
        "stale audio escaped during the final seek"
    );
    harness.engine.step();

    let mut captured = Vec::new();
    for _ in 0..8 {
        captured.extend(harness.capture.pull(BLOCK));
        harness.engine.step();
    }
    let mut decoder = Decoder::open(&harness.path).expect("open fixture");
    decoder.seek_ms(5_500).expect("seek fixture");
    let mut expected = Vec::new();
    while expected.len() < captured.len() {
        expected.extend_from_slice(
            decoder
                .next_block()
                .expect("decode fixture")
                .expect("more audio"),
        );
    }
    let latency = (RATE as usize * 2) / 1_000;
    let compared = captured.len() - latency - BLOCK;
    assert_eq!(
        &captured[latency..latency + compared],
        &expected[..compared],
        "the output came from an earlier seek"
    );
    assert!(errors(&mut harness.events).is_empty());
}

#[test]
fn a_seek_moves_the_reported_position() {
    let mut harness = harness(3_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    harness.engine.handle(Command::SeekAbsolute(1_500));

    // Reported straight away, before a single frame has been played: the
    // interface should not show the old position while the seek lands.
    let landed = harness.position.ms();
    assert!(
        landed.abs_diff(1_500) < 100,
        "a seek to 1500 ms reported {landed} ms"
    );

    harness.engine.step();
    let mut played = 0u64;
    for _ in 0..8 {
        harness.capture.pull(BLOCK);
        harness.engine.step();
        played += BLOCK as u64;
    }

    // One buffer of that went on the flush itself.
    let elapsed = (played - BLOCK as u64) * 1_000 / u64::from(RATE);
    let position = harness.position.ms();
    assert!(
        position.abs_diff(1_500 + elapsed) < 60,
        "after seeking to 1500 ms and playing {elapsed} ms the position reads {position} ms"
    );
}

#[test]
fn the_end_is_announced_when_it_is_heard_and_not_when_it_is_decoded() {
    let mut harness = harness(200, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });

    // Decode the whole track without playing any of it.
    for _ in 0..8 {
        harness.engine.step();
    }
    assert!(
        !harness
            .events
            .try_recv()
            .is_ok_and(|event| matches!(event, PlayerEvent::EndOfFile)),
        "the track ended before a single frame had been played"
    );

    let frames = (RATE as u64 * 200 / 1_000) as usize;
    let mut ends = 0;
    for _ in 0..(frames / BLOCK + 4) {
        harness.capture.pull(BLOCK);
        harness.engine.step();
    }
    while let Ok(event) = harness.events.try_recv() {
        if matches!(event, PlayerEvent::EndOfFile) {
            ends += 1;
        }
    }
    assert_eq!(ends, 1, "the end of the track was announced {ends} times");
}

#[test]
fn a_device_asking_for_more_than_there_is_gets_silence_and_it_is_counted() {
    // The one thing a callback must never do is wait. Running dry has to
    // cost a gap and a number, not a stall.
    let mut harness = harness(50, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    harness.engine.step();

    let mut pulled = Vec::new();
    for _ in 0..200 {
        pulled.extend(harness.capture.pull(BLOCK));
    }

    assert!(harness.engine.starved_frames() > 0, "nothing was reported");
    assert_eq!(
        pulled[pulled.len() - BLOCK..],
        vec![0.0; BLOCK],
        "an empty ring produced something other than silence"
    );
}

#[test]
fn the_backend_plays_a_file_through_its_own_thread() {
    use muscli::audio::{AudioBackend, native::player::NativePlayer};
    use std::{thread, time::Duration};

    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("tone.flac");
    fs::write(
        &path,
        common::flac_bytes(RATE, &common::sine(RATE, 440.0, 1_000)),
    )
    .expect("write the fixture");

    let (output, capture) = CaptureOutput::new(&[RATE]);
    let (sender, mut events) = unbounded_channel();
    let mut player = NativePlayer::start_with(
        move |_| Ok(Box::new(output) as Box<dyn muscli::audio::native::sink::Output>),
        Settings::default(),
        sender,
    )
    .expect("start the backend");
    assert_eq!(player.device(), "capture");

    player.load(&path, 0).expect("load the fixture");
    // The engine tops the ring up every few milliseconds; a quarter of a
    // second is a hundred times what decoding this takes.
    thread::sleep(Duration::from_millis(250));
    assert!(errors(&mut events).is_empty());

    let captured = capture.pull(4 * BLOCK);
    let source = decoded(&path);
    let latency = (RATE as usize * 2) / 1_000;
    assert_eq!(
        &captured[latency..],
        &source[..captured.len() - latency],
        "what the device was handed is not what is in the file"
    );

    // The engine refreshes the position at most every 50 ms, and Windows
    // scheduler jitter may delay that wakeup. Poll with a bounded deadline
    // instead of assuming that exactly one 50 ms sleep reaches the next tick.
    use std::time::Instant;
    let played = 4 * BLOCK as u64 * 1_000 / u64::from(RATE);
    let deadline = Instant::now() + Duration::from_millis(250);
    let position = loop {
        let position = player.position_ms();
        if position.abs_diff(played) < 30 || Instant::now() >= deadline {
            break position;
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(
        position.abs_diff(played) < 30,
        "after {played} ms of audio the position reads {position} ms"
    );
}

#[test]
fn live_mix_with_replaygain_and_eq_stays_safe_through_integer_conversion() {
    use cpal::{I24, Sample};
    use muscli::audio::{dsp::dither::Dither, mix::MixCurve};

    for curve in [MixCurve::Linear, MixCurve::Smooth, MixCurve::EqualPower] {
        let directory = TempDir::new().expect("a temporary directory");
        let first = directory.path().join("first.flac");
        let second = directory.path().join("second.flac");
        let source = common::flac_bytes(RATE, &common::sine(RATE, 1_000.0, 1_000));
        fs::write(&first, &source).expect("write outgoing track");
        fs::write(&second, &source).expect("write incoming track");
        let settings = Settings {
            replay_gain_db: Some(18.0),
            equalizer: vec![(1_000, 6.0)],
            ..Settings::default()
        };
        let (output, capture) = CaptureOutput::new(&[RATE]);
        let (sender, mut events) = unbounded_channel();
        let mut engine = Engine::new(
            Box::new(output),
            settings,
            sender,
            Arc::new(Position::default()),
            Arc::new(Volume::default()),
            Arc::new(AtomicBool::new(false)),
        );
        engine.handle(Command::Crossfade {
            seconds: 0.2,
            curve,
        });
        engine.handle(Command::Load {
            path: first,
            position_ms: 0,
        });
        engine.handle(Command::Prefetch(Some(second)));
        let mut dither = Dither::new(24, 1, true);
        let mut peak = 0.0f32;
        // Includes the fade, the handover and the incoming packet remainder.
        for _ in 0..(RATE as usize * 17 / 10).div_ceil(BLOCK) {
            engine.step();
            let mut block = capture.pull(BLOCK);
            for &sample in &block {
                assert!(sample.is_finite() && sample.abs() <= 1.0 + 1e-6);
                peak = peak.max(sample.abs());
            }
            dither.process(&mut block, 1);
            for sample in block {
                let pcm = I24::from_sample(sample).inner();
                assert!((-8_388_608..=8_388_607).contains(&pcm), "invalid PCM {pcm}");
            }
        }
        assert!(peak > 0.99, "the fixture must exercise the limiter ceiling");
        assert!(errors(&mut events).is_empty());
    }
}

#[test]
fn live_mix_blends_the_last_200ms_into_the_prefetched_song() {
    use muscli::audio::mix::MixCurve;

    let directory = TempDir::new().expect("a temporary directory");
    let first = directory.path().join("first.flac");
    let second = directory.path().join("second.flac");
    fs::write(
        &first,
        common::flac_bytes(RATE, &common::sine(RATE, 220.0, 1_000)),
    )
    .expect("write outgoing track");
    fs::write(
        &second,
        common::flac_bytes(RATE, &common::sine(RATE, 880.0, 1_000)),
    )
    .expect("write incoming track");
    let source_a = decoded(&first);
    let source_b = decoded(&second);

    let (output, capture) = CaptureOutput::new(&[RATE]);
    let (sender, mut events) = unbounded_channel();
    let mut engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        Arc::new(Position::default()),
        Arc::new(Volume::default()),
        Arc::new(AtomicBool::new(false)),
    );

    engine.handle(Command::Crossfade {
        seconds: 0.2,
        curve: MixCurve::Smooth,
    });
    engine.handle(Command::Load {
        path: first.clone(),
        position_ms: 0,
    });
    engine.handle(Command::Prefetch(Some(second.clone())));

    let overlap = (RATE as usize * 200) / 1_000;
    let total = source_a.len() + source_b.len() - overlap;
    let mut heard = Vec::new();
    while heard.len() < total + BLOCK {
        engine.step();
        heard.extend(capture.pull(BLOCK));
    }
    let latency = (RATE as usize * 2) / 1_000;
    let outgoing_before_mix = (RATE as usize * 600) / 1_000;
    assert!(
        (heard[latency + outgoing_before_mix] - source_a[outgoing_before_mix]).abs() < 0.002,
        "the unaffected outgoing section should remain bit-transparent"
    );
    // The half-way mix combines the outgoing 220 Hz tone with the incoming
    // 880 Hz tone starting at its own zero, rather than waiting for EOF.
    let middle = (RATE as usize * 900) / 1_000;
    let incoming_middle = middle - (RATE as usize * 800) / 1_000;
    let t = (middle - (RATE as usize * 800) / 1_000) as f64 / (overlap.saturating_sub(1)) as f64;
    let (a, b) = muscli::audio::mix::blend_gains(MixCurve::Smooth, t);
    let expected = source_a[middle] * a + source_b[incoming_middle] * b;
    assert!(
        (heard[latency + middle] - expected).abs() < 0.03,
        "heard {:.3}, expected mixed {:.3} at mid-fade",
        heard[latency + middle],
        expected
    );
    // From the old track's EOF the new one is already 200 ms in.
    let after_handover = source_a.len() + (RATE as usize * 150) / 1_000;
    assert!(
        (heard[latency + after_handover] - source_b[overlap + (RATE as usize * 150) / 1_000]).abs()
            < 0.005,
        "the next decoder must carry on after the overlap, without replaying it"
    );
    assert!(errors(&mut events).is_empty());
}

#[test]
fn a_buffered_handover_keeps_its_file_identity_after_prefetch_changes() {
    let mut harness = harness(50, &[RATE]);
    let first_next = harness.path.with_file_name("already-buffered.flac");
    let new_next = harness.path.with_file_name("new-next.flac");
    for path in [&first_next, &new_next] {
        fs::write(
            path,
            common::flac_bytes(RATE, &common::sine(RATE, 880.0, 1_000)),
        )
        .expect("write next track");
    }
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    harness
        .engine
        .handle(Command::Prefetch(Some(first_next.clone())));
    // The short outgoing file and the start of its successor fit in the ring.
    // No samples have reached the device, so the handover event is still pending.
    harness.engine.step();
    harness.engine.handle(Command::Prefetch(Some(new_next)));
    let mut started = Vec::new();
    for _ in 0..15 {
        harness.capture.pull(BLOCK);
        harness.engine.step();
        while let Ok(event) = harness.events.try_recv() {
            match event {
                PlayerEvent::PrefetchedTrack(path) => started.push(path),
                PlayerEvent::Error(message) => panic!("{message}"),
                _ => {}
            }
        }
    }
    assert_eq!(started, vec![first_next]);
}

#[test]
fn replacing_prefetch_with_mix_enabled_plays_the_selected_file() {
    use muscli::audio::mix::MixCurve;

    for seconds in [0.0, 0.2] {
        let mut harness = harness(1_000, &[RATE]);
        let stale = harness.path.with_file_name("radiohead.flac");
        let selected = harness.path.with_file_name("marea.flac");
        for (path, frequency) in [(&stale, 220.0), (&selected, 880.0)] {
            fs::write(
                path,
                common::flac_bytes(RATE, &common::sine(RATE, frequency, 1_000)),
            )
            .expect("write next track");
        }
        harness.engine.handle(Command::Crossfade {
            seconds,
            curve: MixCurve::Smooth,
        });
        harness.engine.handle(Command::Load {
            path: harness.path.clone(),
            position_ms: 0,
        });
        harness.engine.handle(Command::Prefetch(Some(stale)));
        // Decode some outgoing audio before Play Next replaces the armed file.
        harness.engine.step();
        let mut heard = harness.capture.pull(BLOCK);
        harness
            .engine
            .handle(Command::Prefetch(Some(selected.clone())));
        while heard.len() < RATE as usize * 2 {
            harness.engine.step();
            heard.extend(harness.capture.pull(BLOCK));
        }
        let latency = RATE as usize * 2 / 1_000;
        let offset = RATE as usize * 150 / 1_000;
        let overlap = (RATE as f64 * seconds) as usize;
        let expected = decoded(&selected);
        let actual = &heard[latency + RATE as usize + offset..][..BLOCK];
        let expected = &expected[overlap + offset..][..BLOCK];
        let maximum_error = actual
            .iter()
            .zip(expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(
            maximum_error < 0.005,
            "wrong incoming audio with MIX={seconds}: {maximum_error}"
        );
        let mut transitioned = Vec::new();
        while let Ok(event) = harness.events.try_recv() {
            match event {
                PlayerEvent::PrefetchedTrack(path) => transitioned.push(path),
                PlayerEvent::Error(message) => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(transitioned, vec![selected]);
    }
}

#[test]
fn an_album_with_global_mix_keeps_every_sample_at_the_join() {
    // The whole point of prefetching: an album side that was mastered to run
    // together has to run together, with no silence and no restart of the
    // device at the join.
    let directory = TempDir::new().expect("a temporary directory");
    let first = directory.path().join("one.flac");
    let second = directory.path().join("two.flac");
    fs::write(
        &first,
        common::flac_bytes(RATE, &common::sine(RATE, 440.0, 2_000)),
    )
    .expect("write the first fixture");
    fs::write(
        &second,
        common::flac_bytes(RATE, &common::sine(RATE, 660.0, 2_000)),
    )
    .expect("write the second fixture");

    let (output, capture) = CaptureOutput::new(&[RATE]);
    let (sender, mut events) = unbounded_channel();
    let position = Arc::new(Position::default());
    let mut engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        Arc::clone(&position),
        Arc::new(Volume::default()),
        Arc::new(AtomicBool::new(false)),
    );

    let context = muscli::model::PlaybackContext::Album {
        key: "continuous".into(),
    };
    engine.handle(Command::Crossfade {
        seconds: context.crossfade_seconds(0.6, false, true),
        curve: muscli::audio::mix::MixCurve::Smooth,
    });
    engine.handle(Command::Load {
        path: first.clone(),
        position_ms: 0,
    });
    engine.handle(Command::Prefetch(Some(second.clone())));
    assert!(errors(&mut events).is_empty());

    let expected: Vec<f32> = decoded(&first)
        .into_iter()
        .chain(decoded(&second))
        .collect();
    let mut captured = Vec::new();
    while captured.len() < expected.len() {
        engine.step();
        captured.extend(capture.pull(BLOCK));
    }

    // One continuous stream: the second file's samples follow the first's
    // with nothing inserted between them.
    let latency = (RATE as usize * 2) / 1_000;
    let compared = expected.len() - latency;
    assert_eq!(
        &captured[latency..latency + compared],
        &expected[..compared]
    );

    // And the application is told exactly once that it has moved on, so its
    // queue index and its history follow the audio.
    let mut handovers = 0;
    let mut ends = 0;
    while let Ok(event) = events.try_recv() {
        match event {
            PlayerEvent::PrefetchedTrack(_) => handovers += 1,
            PlayerEvent::EndOfFile => ends += 1,
            _ => {}
        }
    }
    assert_eq!(handovers, 1, "the handover was announced {handovers} times");
    assert_eq!(ends, 0, "the stream ended when it should have carried on");
}

#[test]
fn songs_at_different_rates_join_without_restarting_the_device() {
    // Exercise both directions, as found in the Windows user's audio log.
    for (first_rate, next_rate) in [(44_100, 48_000), (48_000, 44_100)] {
        let directory = TempDir::new().expect("a temporary directory");
        let first = directory.path().join("one.flac");
        let second = directory.path().join("two.flac");
        fs::write(
            &first,
            common::flac_bytes(first_rate, &common::sine(first_rate, 440.0, 1_000)),
        )
        .expect("write outgoing file");
        fs::write(
            &second,
            common::flac_bytes(next_rate, &common::sine(next_rate, 660.0, 1_000)),
        )
        .expect("write incoming file");

        let (output, capture) = CaptureOutput::new(&[44_100, 48_000]);
        let (sender, mut events) = unbounded_channel();
        let position = Arc::new(Position::default());
        let mut engine = Engine::new(
            Box::new(output),
            Settings::default(),
            sender,
            position,
            Arc::new(Volume::default()),
            Arc::new(AtomicBool::new(false)),
        );
        engine.handle(Command::Load {
            path: first,
            position_ms: 0,
        });
        engine.handle(Command::Prefetch(Some(second.clone())));

        let boundary = first_rate as usize;
        let mut heard = Vec::new();
        while heard.len() < boundary * 2 {
            engine.step();
            heard.extend(capture.pull(BLOCK));
        }

        assert_eq!(
            capture.starts(),
            1,
            "{first_rate} -> {next_rate} reopened the output device"
        );
        assert!(
            errors(&mut events).is_empty(),
            "the transition raised an audio error"
        );
        let mut handovers = 0;
        let mut ends = 0;
        while let Ok(event) = events.try_recv() {
            match event {
                PlayerEvent::PrefetchedTrack(path) if path == second => handovers += 1,
                PlayerEvent::EndOfFile => ends += 1,
                PlayerEvent::Error(message) => panic!("transition error: {message}"),
                _ => {}
            }
        }
        assert_eq!(handovers, 1, "the next song was not adopted");
        assert_eq!(ends, 0, "the old song emitted a premature end");

        // A missing packet or device reopen leaves a near-silent window.
        for (start, finish) in [(boundary - 2_048, boundary), (boundary, boundary + 2_048)] {
            let level = muscli::audio::measure::rms(&heard[start..finish]);
            assert!(
                level > 0.08,
                "{first_rate} -> {next_rate} went silent near the join: RMS {level:.3}"
            );
        }

        // Resampling preserves musical pitch; it must not interpret incoming
        // 48 kHz samples as 44.1 kHz (or the reverse).
        let sample =
            &heard[boundary + first_rate as usize / 4..boundary + first_rate as usize / 4 + 8_192];
        let spectrum = muscli::audio::measure::spectrum(sample, first_rate);
        let found = spectrum.frequency(spectrum.peak_bin());
        assert!(
            (found - 660.0).abs() < 8.0,
            "{first_rate} -> {next_rate} changed pitch: {found:.1} Hz"
        );
    }
}

#[test]
fn bit_perfect_refuses_cross_rate_handover() {
    let directory = TempDir::new().expect("a temporary directory");
    let first = directory.path().join("one.flac");
    let second = directory.path().join("two.flac");
    fs::write(
        &first,
        common::flac_bytes(44_100, &common::sine(44_100, 440.0, 250)),
    )
    .expect("write outgoing file");
    fs::write(
        &second,
        common::flac_bytes(48_000, &common::sine(48_000, 440.0, 250)),
    )
    .expect("write incoming file");

    let (output, capture) = CaptureOutput::new(&[44_100, 48_000]);
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
    engine.handle(Command::Prefetch(Some(second)));

    for _ in 0..30 {
        engine.step();
        capture.pull(BLOCK);
    }
    let mut ends = 0;
    let mut handovers = 0;
    while let Ok(event) = events.try_recv() {
        match event {
            PlayerEvent::EndOfFile => ends += 1,
            PlayerEvent::PrefetchedTrack(_) => handovers += 1,
            PlayerEvent::Error(message) => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(ends, 1, "bit-perfect should finish the current song");
    assert_eq!(
        handovers, 0,
        "bit-perfect unexpectedly resampled the next song"
    );
}

#[test]
fn the_device_is_not_started_before_there_is_anything_to_play() {
    // The device begins asking for samples the moment it is opened. If the
    // ring is still empty then, the first thing every track produces is a
    // dropout -- on every play, and again on every seek.
    let mut harness = harness(500, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });

    // No step: this is the device asking first, which is what really happens.
    let first = harness.capture.pull(BLOCK);
    assert_eq!(
        harness.engine.starved_frames(),
        0,
        "the device was left with nothing to play"
    );
    assert!(
        first.iter().any(|sample| *sample != 0.0),
        "the track began with silence"
    );
}

#[test]
fn a_track_queued_late_still_joins() {
    // The buffer holds half a second, so a short track is decoded to its end
    // long before it is played. Latching "finished" at that moment would lose
    // the join to whatever the application queued a moment later.
    let directory = TempDir::new().expect("a temporary directory");
    let first = directory.path().join("one.flac");
    let second = directory.path().join("two.flac");
    fs::write(
        &first,
        common::flac_bytes(RATE, &common::sine(RATE, 440.0, 150)),
    )
    .expect("write the first fixture");
    fs::write(
        &second,
        common::flac_bytes(RATE, &common::sine(RATE, 660.0, 150)),
    )
    .expect("write the second fixture");

    let (output, capture) = CaptureOutput::new(&[RATE]);
    let (sender, mut events) = unbounded_channel();
    let position = Arc::new(Position::default());
    let mut engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        position,
        Arc::new(Volume::default()),
        Arc::new(AtomicBool::new(false)),
    );

    engine.handle(Command::Load {
        path: first.clone(),
        position_ms: 0,
    });
    // By now the whole of the first track is decoded and waiting.
    engine.step();
    engine.handle(Command::Prefetch(Some(second.clone())));

    let expected: Vec<f32> = decoded(&first)
        .into_iter()
        .chain(decoded(&second))
        .collect();
    let mut captured = Vec::new();
    while captured.len() < expected.len() {
        engine.step();
        captured.extend(capture.pull(BLOCK));
    }

    let latency = (RATE as usize * 2) / 1_000;
    let compared = expected.len() - latency;
    assert_eq!(
        &captured[latency..latency + compared],
        &expected[..compared]
    );

    let mut ends = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, PlayerEvent::EndOfFile) {
            ends += 1;
        }
    }
    assert_eq!(ends, 0, "the first track ended instead of joining");
}

#[test]
fn continuous_playback_keeps_samples_through_producer_jitter_without_mix() {
    // Drive time by consuming frames: no sleeping or sound card is needed.
    // Every burst stays within the half-second ring, even if decoding does
    // not get scheduled for 350 ms. Filling a full ring must never overwrite it.
    for gap_ms in [5, 50, 150, 350] {
        let mut harness = harness(3_000, &[RATE]);
        harness.engine.handle(Command::Load {
            path: harness.path.clone(),
            position_ms: 0,
        });
        let expected = decoded(&harness.path);
        let mut heard = Vec::new();
        let burst = RATE as usize * gap_ms / 1_000;
        while heard.len() < RATE as usize * 2 {
            for _ in 0..8 {
                harness.engine.step();
            }
            heard.extend(harness.capture.pull(burst));
        }
        assert_eq!(
            harness.engine.starved_frames(),
            0,
            "ring ran dry at {gap_ms} ms"
        );
        let latency = RATE as usize * 2 / 1_000;
        assert_eq!(
            &heard[latency..],
            &expected[..heard.len() - latency],
            "samples lost or overwritten at {gap_ms} ms"
        );
        assert!(errors(&mut harness.events).is_empty());
    }
}

#[test]
fn running_dry_in_the_middle_of_a_track_is_reported() {
    // A dropout is audible, and a listener who hears one deserves to be told
    // why rather than left wondering whether the file is damaged.
    let mut harness = harness(3_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });

    // Drain far past what the buffer holds without letting the engine refill.
    for _ in 0..40 {
        harness.capture.pull(BLOCK);
    }
    harness.engine.step();

    let mut notices = Vec::new();
    while let Ok(event) = harness.events.try_recv() {
        if let PlayerEvent::Notice(message) = event {
            notices.push(message);
        }
    }
    assert_eq!(notices.len(), 1, "expected one complaint, got {notices:?}");
    assert!(harness.engine.starved_frames() > 0);
}

#[test]
fn running_dry_after_the_last_note_is_not_reported() {
    // The ring empties at the end of every track while the application loads
    // the next one. Calling that a fault would cry wolf once per track.
    let mut harness = harness(100, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });

    for _ in 0..60 {
        harness.engine.step();
        harness.capture.pull(BLOCK);
    }

    let notices: Vec<String> = std::iter::from_fn(|| harness.events.try_recv().ok())
        .filter_map(|event| match event {
            PlayerEvent::Notice(message) => Some(message),
            _ => None,
        })
        .collect();
    assert!(
        notices.is_empty(),
        "the end of a track complained: {notices:?}"
    );
}

#[test]
fn an_idle_engine_asks_to_be_left_alone() {
    let mut harness = harness(500, &[RATE]);
    assert_eq!(
        harness.engine.idle_timeout(),
        None,
        "an engine with nothing to play still wanted waking"
    );

    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    assert!(harness.engine.idle_timeout().is_some());

    harness.engine.handle(Command::Pause(true));
    let resting = harness.engine.idle_timeout().expect("still has a stream");
    assert!(
        resting >= std::time::Duration::from_millis(50),
        "a paused engine still wanted waking every {resting:?}"
    );
}

#[test]
fn a_full_ring_is_left_alone_longer_than_a_drained_one() {
    // The engine wakes to top the ring up. How soon it has to is how long the
    // ring will last, so a full one means a long wait and a drained one a
    // short one.
    let mut harness = harness(5_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    let full = harness.engine.idle_timeout().expect("has a stream");

    // Take almost everything the ring holds, without letting the engine
    // refill in between.
    harness.capture.pull(RATE as usize * 450 / 1_000);
    let drained = harness.engine.idle_timeout().expect("still has a stream");

    assert!(
        drained < full,
        "a drained ring asked to be left {drained:?}, a full one {full:?}"
    );
}

#[test]
fn the_position_does_not_lurch_while_a_seek_lands() {
    // Between asking for the flush and the output carrying it out, the marks
    // are counted from a frame the device has not reached. Measuring against
    // them there would put the position wherever the track happened to be.
    let mut harness = harness(5_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });

    // Play a while, so the device's frame counter is well past zero.
    for _ in 0..40 {
        harness.capture.pull(BLOCK);
        harness.engine.step();
    }

    harness.engine.handle(Command::SeekAbsolute(1_500));
    harness.engine.step();

    let position = harness.position.ms();
    assert!(
        position.abs_diff(1_500) < 60,
        "while the seek was landing the position read {position} ms"
    );
}

#[test]
fn a_seek_while_paused_lands_when_playing_resumes() {
    // Nothing carries out the flush while the device is stopped, so the
    // engine waits rather than pushing audio that would be thrown away. The
    // seek has to take effect the moment playing resumes.
    let mut harness = harness(3_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    harness.engine.handle(Command::Pause(true));
    harness.engine.handle(Command::SeekAbsolute(1_500));
    for _ in 0..4 {
        harness.engine.step();
    }

    // A paused device is asked for nothing, so nothing comes out.
    assert!(harness.capture.pull(BLOCK).iter().all(|s| *s == 0.0));

    harness.engine.handle(Command::Pause(false));
    let landing = harness.capture.pull(BLOCK);
    assert!(
        landing.iter().all(|sample| *sample == 0.0),
        "the old position was played after the seek"
    );
    harness.engine.step();

    let mut captured = Vec::new();
    for _ in 0..4 {
        captured.extend(harness.capture.pull(BLOCK));
        harness.engine.step();
    }

    let mut decoder = Decoder::open(&harness.path).expect("open the fixture");
    decoder.seek_ms(1_500).expect("seek");
    let mut expected = Vec::new();
    while expected.len() < captured.len() {
        let block = decoder.next_block().expect("decode").expect("more audio");
        expected.extend_from_slice(block);
    }

    let latency = (RATE as usize * 2) / 1_000;
    let compared = captured.len() - latency - BLOCK;
    assert_eq!(
        &captured[latency..latency + compared],
        &expected[..compared]
    );
    assert_eq!(harness.capture.starts(), 1, "the device was reopened");
}

#[test]
fn loading_a_track_does_not_report_the_last_one_s_position() {
    // The application reads the position back inside the same tick it asked
    // for a track. If the answer is still the previous track's, it stores it
    // as this one's resume point and the next play starts a minute in.
    use muscli::audio::{AudioBackend, native::player::NativePlayer};
    use std::{thread, time::Duration};

    let directory = TempDir::new().expect("a temporary directory");
    let first = directory.path().join("one.flac");
    let second = directory.path().join("two.flac");
    for (path, hz) in [(&first, 440.0), (&second, 660.0)] {
        fs::write(
            path,
            common::flac_bytes(RATE, &common::sine(RATE, hz, 4_000)),
        )
        .expect("write a fixture");
    }

    let (output, capture) = CaptureOutput::new(&[RATE]);
    let (sender, _events) = unbounded_channel();
    let mut player = NativePlayer::start_with(
        move |_| Ok(Box::new(output) as Box<dyn muscli::audio::native::sink::Output>),
        Settings::default(),
        sender,
    )
    .expect("start the backend");

    player.load(&first, 0).expect("load the first");
    thread::sleep(Duration::from_millis(100));
    for _ in 0..40 {
        capture.pull(BLOCK);
    }
    thread::sleep(Duration::from_millis(50));
    assert!(
        player.position_ms() > 500,
        "the first track never got going"
    );

    player.load(&second, 0).expect("load the second");
    let straight_away = player.position_ms();
    assert_eq!(
        straight_away, 0,
        "a track just loaded reported {straight_away} ms, which belongs to the one before it"
    );
}

#[test]
fn a_configured_equaliser_reaches_the_device() {
    // Measured at the far end of the whole path rather than on the filter in
    // isolation, so a band that is configured but never wired up cannot pass.
    use muscli::config::EQUALIZER_BANDS;

    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("tone.flac");
    // A tone sitting on the 1 kHz band, which is EQUALIZER_BANDS[3].
    let band = EQUALIZER_BANDS[3] as f64;
    fs::write(
        &path,
        common::flac_bytes(RATE, &common::sine(RATE, band, 4_000)),
    )
    .expect("write the fixture");

    let level = |gain_db: f32| {
        let settings = Settings {
            equalizer: EQUALIZER_BANDS
                .iter()
                .enumerate()
                .map(|(index, hz)| (*hz, if index == 3 { gain_db } else { 0.0 }))
                .collect(),
            ..Settings::default()
        };
        let (output, capture) = CaptureOutput::new(&[RATE]);
        let (sender, _events) = unbounded_channel();
        let mut engine = Engine::new(
            Box::new(output),
            settings,
            sender,
            Arc::new(Position::default()),
            Arc::new(Volume::default()),
            Arc::new(AtomicBool::new(false)),
        );
        engine.handle(Command::Load {
            path: path.clone(),
            position_ms: 0,
        });

        let mut captured = Vec::new();
        while captured.len() < 3 * 32_768 {
            engine.step();
            captured.extend(capture.pull(BLOCK));
        }
        // Past the filter's start-up, and measured through a window because
        // the tone is not coherent with the analysis length.
        muscli::audio::measure::windowed_amplitude(
            &captured[32_768..2 * 32_768],
            RATE,
            band - 100.0,
            band + 100.0,
        )
    };

    let flat = level(0.0);
    let lifted = level(6.0);
    let moved = muscli::audio::measure::db(lifted / flat);
    assert!(
        (moved - 6.0).abs() < 0.5,
        "a band set to +6 dB moved the device's audio by {moved:.2} dB"
    );
}

#[test]
fn pausing_silences_the_device_and_stops_the_clock() {
    let mut harness = harness(3_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    for _ in 0..8 {
        harness.capture.pull(BLOCK);
        harness.engine.step();
    }
    let before = harness.position.ms();
    assert!(before > 0, "nothing played");

    harness.engine.handle(Command::Pause(true));
    let quiet = harness.capture.pull(BLOCK);
    harness.engine.step();

    assert!(
        quiet.iter().all(|sample| *sample == 0.0),
        "the device kept playing after being paused"
    );
    assert_eq!(
        harness.position.ms(),
        before,
        "the position moved while paused"
    );
    assert!(errors(&mut harness.events).is_empty(), "pausing failed");

    // And resuming picks up where it stopped rather than skipping ahead.
    harness.engine.handle(Command::Pause(false));
    let resumed = harness.capture.pull(BLOCK);
    assert!(resumed.iter().any(|sample| *sample != 0.0));
}

#[test]
fn a_pause_already_in_force_is_respected_by_a_device_just_started() {
    // Restoring a session loads the track, which starts the device, and pauses
    // it on top of that. If the pause is not already in force when the device
    // opens, the music begins on its own every time muscli starts.
    let directory = TempDir::new().expect("a temporary directory");
    let path = directory.path().join("tone.flac");
    fs::write(
        &path,
        common::flac_bytes(RATE, &common::sine(RATE, 440.0, 2_000)),
    )
    .expect("write the fixture");

    let (output, capture) = CaptureOutput::new(&[RATE]);
    let (sender, mut events) = unbounded_channel();
    let paused = Arc::new(AtomicBool::new(true));
    let mut engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        Arc::new(Position::default()),
        Arc::new(Volume::default()),
        Arc::clone(&paused),
    );

    engine.handle(Command::Load {
        path,
        position_ms: 0,
    });

    // The very first thing the device asks for, with no step in between.
    let first = capture.pull(BLOCK);
    assert!(
        first.iter().all(|sample| *sample == 0.0),
        "the music started on its own"
    );
    assert!(errors(&mut events).is_empty());
}

#[test]
fn mix_events_follow_audible_output_and_precede_the_single_queue_handover() {
    let mut h = harness(2_000, &[RATE]);
    let incoming = h.path.with_file_name("incoming.flac");
    fs::write(
        &incoming,
        common::flac_bytes(RATE, &common::sine(RATE, 660.0, 2_000)),
    )
    .unwrap();
    h.engine.handle(Command::Crossfade {
        seconds: 0.4,
        curve: muscli::audio::mix::MixCurve::Smooth,
    });
    h.engine.handle(Command::Load {
        path: h.path.clone(),
        position_ms: 0,
    });
    h.engine.handle(Command::Prefetch(Some(incoming.clone())));
    let mut consumed = 0usize;
    let mut stages = Vec::new();
    let mut last_position = 0;
    for _ in 0..(RATE as usize * 3).div_ceil(BLOCK) {
        h.engine.step();
        while let Ok(event) = h.events.try_recv() {
            match event {
                PlayerEvent::MixStarted { incoming: path, .. } => {
                    assert!(
                        consumed >= RATE as usize * 1600 / 1000,
                        "announced while still decoding ahead"
                    );
                    assert_eq!(path, incoming);
                    stages.push("start");
                }
                PlayerEvent::MixProgress {
                    incoming: path,
                    position_ms,
                    progress,
                } => {
                    assert_eq!(path, incoming);
                    assert!(!stages.is_empty());
                    assert!(position_ms >= last_position);
                    assert!((0.0..=1.0).contains(&progress));
                    last_position = position_ms;
                }
                PlayerEvent::MixFinished { .. } => stages.push("finish"),
                PlayerEvent::PrefetchedTrack(_) => stages.push("handover"),
                PlayerEvent::Error(message) => panic!("{message}"),
                _ => {}
            }
        }
        h.capture.pull(BLOCK);
        consumed += BLOCK;
    }
    assert_eq!(stages, ["start", "finish", "handover"]);
    assert_eq!(last_position, 400);
}

#[test]
fn incoming_replaygain_is_applied_during_overlap_and_at_the_decode_boundary() {
    let mut h = harness(2_000, &[RATE]);
    let incoming = h.path.with_file_name("incoming-gain.flac");
    fs::write(
        &incoming,
        common::flac_bytes(RATE, &common::sine(RATE, 660.0, 2_000)),
    )
    .unwrap();
    h.engine.handle(Command::ReplayGain(Some(-6.0)));
    h.engine.handle(Command::Crossfade {
        seconds: 0.4,
        curve: muscli::audio::mix::MixCurve::Smooth,
    });
    h.engine.handle(Command::Load {
        path: h.path.clone(),
        position_ms: 0,
    });
    h.engine.handle(Command::PrefetchWithGain {
        path: Some(incoming.clone()),
        gain_db: Some(-12.0),
    });
    let mut heard = Vec::new();
    while heard.len() < RATE as usize * 3 {
        h.engine.step();
        heard.extend(h.capture.pull(BLOCK));
    }
    let first = decoded(&h.path);
    let second = decoded(&incoming);
    let latency = RATE as usize * 2 / 1_000;
    for position in [RATE as usize * 18 / 10, RATE as usize * 23 / 10] {
        let expected = if position < first.len() {
            let next_position = position - RATE as usize * 16 / 10;
            let progress = next_position as f64 / (RATE as f64 * 0.4 - 1.0);
            let (a, b) =
                muscli::audio::mix::blend_gains(muscli::audio::mix::MixCurve::Smooth, progress);
            first[position] * a * 10.0f32.powf(-6.0 / 20.0)
                + second[next_position] * b * 10.0f32.powf(-12.0 / 20.0)
        } else {
            second[position - RATE as usize * 16 / 10] * 10.0f32.powf(-12.0 / 20.0)
        };
        assert!((heard[position + latency] - expected).abs() < 0.0001);
    }
}
