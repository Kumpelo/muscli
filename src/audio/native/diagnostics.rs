//! Opt-in observation. File I/O runs on its own thread, never in a callback.

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use super::sink::SinkState;
use serde_json::{Value, json};

static RECORDER: OnceLock<Option<Arc<Recorder>>> = OnceLock::new();

pub(super) struct Recorder {
    sender: SyncSender<Event>,
    next_stream: AtomicU64,
    dropped: Arc<AtomicU64>,
}

enum Event {
    Start(u64, Weak<SinkState>, Value),
    Data(Value),
    End(u64, Value),
    Flush(mpsc::Sender<()>),
}

#[derive(Clone)]
pub(super) struct StreamLog {
    id: u64,
    recorder: Arc<Recorder>,
    state: Weak<SinkState>,
}

fn stamp(mut value: Value) -> Value {
    value["unix_ms"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    value["pid"] = json!(std::process::id());
    value
}

fn snapshot(id: u64, state: &SinkState) -> Value {
    stamp(json!({"event": "ring_snapshot", "stream": id,
        "delivered_frames": state.played.load(Ordering::Relaxed),
        "ring_missing_frames": state.starved.load(Ordering::Relaxed),
        "paused": state.paused.load(Ordering::Relaxed),
        "flush_generation": state.flushed.load(Ordering::Relaxed)}))
}

fn write_record(file: &mut File, value: Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *file, &value)?;
    file.write_all(b"\n")
}

impl Recorder {
    fn open(path: &Path) -> std::io::Result<Arc<Self>> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        write_record(
            &mut file,
            stamp(json!({"event": "diagnostics_started", "version": env!("CARGO_PKG_VERSION")})),
        )?;
        let (sender, receiver) = mpsc::sync_channel(128);
        let dropped = Arc::new(AtomicU64::new(0));
        let losses = Arc::clone(&dropped);
        thread::Builder::new()
            .name("muscli-audio-log".into())
            .spawn(move || {
                let mut active: HashMap<u64, Weak<SinkState>> = HashMap::new();
                let mut tick = Instant::now() + Duration::from_secs(1);
                let mut lost_before = 0;
                loop {
                    let mut barrier = None;
                    let value = match receiver
                        .recv_timeout(tick.saturating_duration_since(Instant::now()))
                    {
                        Ok(Event::Start(id, state, value)) => {
                            active.insert(id, state);
                            Some(value)
                        }
                        Ok(Event::Data(value)) => Some(value),
                        Ok(Event::End(id, value)) => {
                            active.remove(&id);
                            Some(value)
                        }
                        Ok(Event::Flush(reply)) => {
                            barrier = Some(reply);
                            None
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if let Some(value) = value
                        && write_record(&mut file, value).is_err()
                    {
                        break;
                    }
                    if Instant::now() >= tick || barrier.is_some() {
                        let mut failed = false;
                        active.retain(|id, weak| {
                            let Some(state) = weak.upgrade() else {
                                return false;
                            };
                            failed |= write_record(&mut file, snapshot(*id, &state)).is_err();
                            true
                        });
                        let lost = losses.load(Ordering::Relaxed);
                        if lost != lost_before {
                            failed |= write_record(
                                &mut file,
                                stamp(json!({"event":"diagnostics_dropped", "count":lost})),
                            )
                            .is_err();
                            lost_before = lost;
                        }
                        if failed {
                            break;
                        }
                        tick = Instant::now() + Duration::from_secs(1);
                    }
                    if let Some(reply) = barrier {
                        let _ = reply.send(());
                    }
                }
            })?;
        Ok(Arc::new(Self {
            sender,
            next_stream: AtomicU64::new(1),
            dropped,
        }))
    }

    fn send(&self, event: Event) {
        if self.sender.try_send(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn record(&self, value: Value) {
        self.send(Event::Data(stamp(value)));
    }

    pub(super) fn stream(
        self: &Arc<Self>,
        state: &Arc<SinkState>,
        mut metadata: Value,
    ) -> StreamLog {
        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
        metadata["event"] = json!("stream_open");
        metadata["stream"] = json!(id);
        self.send(Event::Start(id, Arc::downgrade(state), stamp(metadata)));
        StreamLog {
            id,
            recorder: Arc::clone(self),
            state: Arc::downgrade(state),
        }
    }

    fn flush(&self) {
        let (sender, receiver) = mpsc::channel();
        if self.sender.try_send(Event::Flush(sender)).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_millis(500));
        }
    }
}

pub(super) fn recorder() -> Option<&'static Arc<Recorder>> {
    RECORDER
        .get_or_init(|| {
            let path = std::env::var_os("MUSCLI_AUDIO_LOG").filter(|path| !path.is_empty())?;
            match Recorder::open(Path::new(&path)) {
                Ok(recorder) => Some(recorder),
                Err(error) => {
                    eprintln!("Audio diagnostics unavailable: {error}");
                    None
                }
            }
        })
        .as_ref()
}

pub(super) fn flush() {
    if let Some(Some(recorder)) = RECORDER.get() {
        recorder.flush();
    }
}

impl StreamLog {
    pub(super) fn record(&self, mut value: Value) {
        value["stream"] = json!(self.id);
        if let Some(state) = self.state.upgrade() {
            value["ring_missing_frames"] = json!(state.starved.load(Ordering::Relaxed));
            value["delivered_frames"] = json!(state.played.load(Ordering::Relaxed));
        }
        self.recorder.record(value);
    }

    pub(super) fn end(&self) {
        let mut value = self
            .state
            .upgrade()
            .map(|state| snapshot(self.id, &state))
            .unwrap_or_else(|| stamp(json!({"stream":self.id})));
        value["event"] = json!("stream_closed");
        self.recorder.send(Event::End(self.id, value));
    }
}

#[cfg(test)]
mod tests {
    use super::super::sink::Volume;
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn records_negotiation_faults_ring_counts_and_stream_closure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.jsonl");
        let recorder = Recorder::open(&path).unwrap();
        let state = Arc::new(SinkState::new(
            Arc::new(Volume::default()),
            Arc::new(AtomicBool::new(false)),
        ));
        let stream = recorder.stream(&state, json!({"rate":44100,"requested_frames":2048}));
        stream.record(json!({"event":"stream_started","negotiated_frames":2048}));
        state.played.store(4096, Ordering::Relaxed);
        state.starved.store(128, Ordering::Relaxed);
        stream.record(json!({"event":"device_fault","kind":"Xrun"}));
        recorder.flush();
        stream.end();
        recorder.flush();
        let rows: Vec<Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            rows.iter()
                .all(|row| row["pid"].is_number() && row["unix_ms"].is_number())
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row["event"] == "stream_open")
                .count(),
            1
        );
        assert!(rows.iter().any(|row| row["negotiated_frames"] == 2048));
        assert!(
            rows.iter()
                .any(|row| row["event"] == "device_fault" && row["ring_missing_frames"] == 128)
        );
        assert!(
            rows.iter()
                .any(|row| row["event"] == "ring_snapshot" && row["delivered_frames"] == 4096)
        );
        assert_eq!(rows.last().unwrap()["event"], "stream_closed");
    }
    #[test]
    fn a_full_log_queue_counts_loss_without_waiting_for_the_writer() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let recorder = Recorder {
            sender,
            next_stream: AtomicU64::new(1),
            dropped: Arc::new(AtomicU64::new(0)),
        };
        recorder.record(json!({"event":"first"}));
        recorder.record(json!({"event":"second"}));
        assert_eq!(recorder.dropped.load(Ordering::Relaxed), 1);
        assert!(matches!(receiver.try_recv(), Ok(Event::Data(_))));
        assert!(receiver.try_recv().is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "opens a silent default ALSA stream for two seconds; no injected delays"]
    fn live_alsa_observes_negotiated_period_without_injected_delays() {
        use crate::audio::{
            decode::StreamSpec,
            native::{device::CpalOutput, sink::Output},
        };
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let mut output = CpalOutput::open(None, false, events).unwrap();
        let format = output
            .negotiate(StreamSpec {
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: Some(24),
            })
            .unwrap();
        assert_eq!(format.sample_rate, 44100);
        let state = Arc::new(SinkState::new(
            Arc::new(Volume::default()),
            Arc::new(AtomicBool::new(false)),
        ));
        let samples = 44100 * usize::from(format.channels) * 4;
        let (mut producer, consumer) = rtrb::RingBuffer::new(samples);
        assert_eq!(producer.push_partial_slice(&vec![0.0; samples]).1.len(), 0);
        output.start(format, consumer, Arc::clone(&state)).unwrap();
        thread::sleep(Duration::from_secs(2));
        output.stop();
        drop(output);
        assert_eq!(state.starved.load(Ordering::Relaxed), 0);
        assert!(state.played.load(Ordering::Relaxed) > 44100);
        assert!(received.try_recv().is_err(), "unexpected device fault");
    }
}
