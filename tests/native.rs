//! The native playback path, end to end: a file on disk, decoded, processed,
//! through the ring, and out of a device that hands its samples to the test
//! instead of to a listener.

mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use muscli::{
    audio::{
        decode::Decoder,
        dsp::Settings,
        native::{
            engine::{Command, Engine},
            sink::{CaptureHandle, CaptureOutput},
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
    position: Arc<AtomicU64>,
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
    let position = Arc::new(AtomicU64::new(0));
    let engine = Engine::new(
        Box::new(output),
        Settings::default(),
        sender,
        Arc::clone(&position),
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
    let position = Arc::new(AtomicU64::new(0));
    let mut engine = Engine::new(Box::new(output), Settings::default(), sender, position);

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
    for (frame, pair) in captured.chunks_exact(2).enumerate().skip(latency).take(256) {
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
        harness.position.load(Ordering::Relaxed),
        0,
        "the position moved before anything was played"
    );

    // Play a quarter of a second.
    for _ in 0..(RATE as usize / 4 / BLOCK) {
        harness.capture.pull(BLOCK);
        harness.engine.step();
    }
    let position = harness.position.load(Ordering::Relaxed);
    assert!(
        position.abs_diff(250) < 40,
        "a quarter of a second in, the position reads {position} ms"
    );
}

#[test]
fn a_seek_moves_both_the_position_and_the_audio() {
    let mut harness = harness(3_000, &[RATE]);
    harness.engine.handle(Command::Load {
        path: harness.path.clone(),
        position_ms: 0,
    });
    harness.engine.handle(Command::SeekAbsolute(1_500));
    assert!(errors(&mut harness.events).is_empty());

    harness.engine.step();
    let mut captured = Vec::new();
    for _ in 0..8 {
        captured.extend(harness.capture.pull(BLOCK));
        harness.engine.step();
    }

    // Eight blocks have been played since the seek, so that is where the
    // position should be -- the seek lands on the container's nearest frame
    // boundary, which is why this is a window rather than an equality.
    let played = 8 * BLOCK as u64 * 1_000 / u64::from(RATE);
    let position = harness.position.load(Ordering::Relaxed);
    assert!(
        position.abs_diff(1_500 + played) < 100,
        "after seeking to 1500 ms and playing {played} ms the position reads {position} ms"
    );

    // And the samples have to be the ones that live there, not merely a
    // plausible number of them.
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

    // And the position has to have followed the device rather than the
    // decoder, which by now is most of a second ahead.
    thread::sleep(Duration::from_millis(50));
    let position = player.position_ms();
    let played = 4 * BLOCK as u64 * 1_000 / u64::from(RATE);
    assert!(
        position.abs_diff(played) < 30,
        "after {played} ms of audio the position reads {position} ms"
    );
}
