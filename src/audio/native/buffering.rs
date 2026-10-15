//! Device periods are independent of the decoder's half-second ring. The
//! output callback must still meet ALSA's deadline even when that ring is full.

use cpal::{BufferSize, SupportedBufferSize};

/// Prefer a roughly 40–80 ms period on ALSA. Its default PipeWire period can
/// be 512 frames (11.6 ms at 44.1 kHz), too short for ordinary scheduling jitter.
/// Leave other hosts and unknown device limits to their existing defaults.
pub(super) fn playback_buffer_size(rate: u32, supported: &SupportedBufferSize) -> BufferSize {
    if cfg!(target_os = "linux")
        && rate > 0
        && let SupportedBufferSize::Range { min, max } = *supported
        && min <= max
        && max > 0
    {
        let frames = rate.div_ceil(25).next_power_of_two();
        return BufferSize::Fixed(frames.clamp(min.max(1), max));
    }
    BufferSize::Default
}

/// Keep driver errors distinct from an empty decoder ring. ALSA recovers Xrun
/// internally; reopening here would add another discontinuity and discard audio.
pub(super) fn device_error_callback(
    events: tokio::sync::mpsc::UnboundedSender<crate::model::PlayerEvent>,
    state: std::sync::Arc<super::sink::SinkState>,
    device: String,
    config: cpal::StreamConfig,
    diagnostics: Option<super::diagnostics::StreamLog>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    use std::sync::atomic::Ordering;
    let mut xruns = 0u64;
    move |error| {
        if error.kind() == cpal::ErrorKind::Xrun {
            xruns = xruns.saturating_add(1);
        }
        if let Some(log) = &diagnostics {
            log.record(serde_json::json!({"event":"device_fault", "kind":format!("{:?}", error.kind()), "message":error.to_string(), "device_xruns":xruns}));
        }
        let missing = state.starved.load(Ordering::Relaxed);
        let _ = events.send(crate::model::PlayerEvent::Notice(format!(
            "Audio device {:?}: {error} [{device}; {} Hz; {} ch; requested period {:?}; ring missing {missing} frames; device xruns {xruns}]",
            error.kind(), config.sample_rate, config.channels, config.buffer_size,
        )));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn alsa_period_has_room_for_a_35ms_callback_delay_at_music_rates() {
        let supported = SupportedBufferSize::Range {
            min: 64,
            max: 65_536,
        };
        for rate in [8_000, 44_100, 48_000, 96_000, 192_000, 384_000, 768_000] {
            let BufferSize::Fixed(frames) = playback_buffer_size(rate, &supported) else {
                panic!("no bounded period selected for {rate} Hz");
            };
            assert!(u64::from(frames) * 1_000 >= u64::from(rate) * 40);
            assert!(u64::from(frames) * 1_000 < u64::from(rate) * 80);
        }
    }

    #[test]
    fn period_selection_respects_device_limits_and_unknown_ranges() {
        assert_eq!(
            playback_buffer_size(44_100, &SupportedBufferSize::Range { min: 32, max: 1024 }),
            BufferSize::Fixed(1024)
        );
        assert_eq!(
            playback_buffer_size(
                44_100,
                &SupportedBufferSize::Range {
                    min: 4096,
                    max: 8192
                }
            ),
            BufferSize::Fixed(4096)
        );
        assert_eq!(
            playback_buffer_size(44_100, &SupportedBufferSize::Unknown),
            BufferSize::Default
        );
        assert_eq!(
            playback_buffer_size(
                44_100,
                &SupportedBufferSize::Range {
                    min: 4096,
                    max: 1024
                }
            ),
            BufferSize::Default
        );
    }

    #[test]
    fn driver_xruns_are_reported_separately_from_ring_starvation() {
        use crate::{
            audio::native::sink::{SinkState, Volume},
            model::PlayerEvent,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let state = Arc::new(SinkState::new(
            Arc::new(Volume::default()),
            Arc::new(AtomicBool::new(false)),
        ));
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let mut report = device_error_callback(
            events,
            Arc::clone(&state),
            "test output".into(),
            cpal::StreamConfig {
                channels: 2,
                sample_rate: 44_100,
                buffer_size: BufferSize::Fixed(2048),
            },
            None,
        );
        report(cpal::ErrorKind::Xrun.into());
        let PlayerEvent::Notice(message) = received.try_recv().unwrap() else {
            panic!("driver errors must not skip a song")
        };
        assert!(message.contains("A buffer underrun or overrun occurred."));
        assert!(message.contains("ring missing 0 frames"));
        assert!(message.contains("device xruns 1"));
        state.starved.store(128, Ordering::Relaxed);
        report(cpal::ErrorKind::Xrun.into());
        let PlayerEvent::Notice(message) = received.try_recv().unwrap() else {
            panic!("missing notice")
        };
        assert!(message.contains("ring missing 128 frames"));
        assert!(message.contains("device xruns 2"));
        report(cpal::ErrorKind::DeviceNotAvailable.into());
        let PlayerEvent::Notice(message) = received.try_recv().unwrap() else {
            panic!("missing disconnect notice")
        };
        assert!(message.contains("DeviceNotAvailable"));
        assert!(message.contains("device xruns 2"));
    }

    /// Opens an additional silent stream. Never change the user's audio route.
    #[test]
    #[ignore = "requires Linux ALSA/PipeWire output; injects three 35 ms callback delays"]
    fn live_alsa_callback_jitter_recovers_without_xruns_with_the_selected_period() {
        use cpal::{
            ErrorKind, StreamConfig,
            traits::{DeviceTrait, HostTrait, StreamTrait},
        };
        use std::{
            sync::{
                Arc, Mutex,
                atomic::{AtomicUsize, Ordering},
            },
            thread,
            time::{Duration, Instant},
        };

        let device = cpal::default_host()
            .default_output_device()
            .expect("default output");
        let supported = device
            .supported_output_configs()
            .unwrap()
            .find(|config| {
                config.channels() == 2
                    && config.contains_rate(44_100)
                    && config.sample_format() == cpal::SampleFormat::F32
            })
            .expect("44.1 kHz stereo float output");
        let selected = playback_buffer_size(44_100, supported.buffer_size());
        let mut results = Vec::new();
        for size in [BufferSize::Default, selected] {
            let errors = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&errors);
            let injected = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&injected);
            let calls = Arc::new(AtomicUsize::new(0));
            let callbacks = Arc::clone(&calls);
            let last_delay = Arc::new(AtomicUsize::new(0));
            let delayed_at = Arc::clone(&last_delay);
            let started = Instant::now();
            let mut delay = 0;
            let stream = device
                .build_output_stream::<f32, _, _>(
                    StreamConfig {
                        channels: 2,
                        sample_rate: 44_100,
                        buffer_size: size,
                    },
                    move |buffer, _| {
                        buffer.fill(0.0);
                        let call = callbacks.fetch_add(1, Ordering::Relaxed) + 1;
                        if delay < 3
                            && started.elapsed() >= Duration::from_millis(500 + delay * 700)
                        {
                            delay += 1;
                            counted.fetch_add(1, Ordering::Relaxed);
                            delayed_at.store(call, Ordering::Relaxed);
                            thread::sleep(Duration::from_millis(35));
                        }
                    },
                    move |error| recorded.lock().unwrap().push(error.kind()),
                    None,
                )
                .expect("build silent probe");
            let period = stream.buffer_size().expect("actual ALSA period");
            stream.play().expect("start silent probe");
            thread::sleep(Duration::from_secs(3));
            drop(stream);
            let kinds = errors.lock().unwrap().clone();
            eprintln!(
                "requested={size:?}, actual_period={period}, injections={}, callbacks={}, errors={kinds:?}",
                injected.load(Ordering::Relaxed),
                calls.load(Ordering::Relaxed)
            );
            assert_eq!(injected.load(Ordering::Relaxed), 3);
            assert!(
                calls.load(Ordering::Relaxed) > last_delay.load(Ordering::Relaxed) + 5,
                "stream did not continue after the final delayed callback"
            );
            results.push(kinds);
        }
        assert!(
            results[0].contains(&ErrorKind::Xrun),
            "default-period reproduction did not trigger; environment differs"
        );
        assert!(
            results[1].is_empty(),
            "selected-period stream still failed: {:?}",
            results[1]
        );
    }
}
