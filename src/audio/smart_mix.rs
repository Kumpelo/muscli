//! Conservative live transition policy. A tempo match is only one signal;
//! missing or conflicting evidence selects the ordinary gapless path.
use serde::{Deserialize, Serialize};

use super::mix::TrackAnalysis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveMixMode {
    Off,
    Classic,
    Smart,
    AutoDj,
}

impl LiveMixMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Classic,
            Self::Classic => Self::Smart,
            Self::Smart => Self::AutoDj,
            Self::AutoDj => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Classic => "CLASSIC",
            Self::Smart => "SMART",
            Self::AutoDj => "AUTO_DJ",
        }
    }
}

/// The result is a heuristic overlap duration, never a claim of harmonic or
/// beat-phase compatibility. No pitch, tempo, EQ or source offset is changed.
pub fn transition_seconds(first: TrackAnalysis, second: TrackAnalysis, maximum: f64) -> f64 {
    let (Some(a), Some(b), Some(outgoing), Some(incoming)) =
        (first.bpm, second.bpm, first.boundaries, second.boundaries)
    else {
        return 0.0;
    };
    if ![
        a,
        b,
        first.confidence,
        second.confidence,
        first.energy_rms,
        second.energy_rms,
        outgoing.outro_rms,
        incoming.intro_rms,
        maximum,
    ]
    .iter()
    .all(|v| v.is_finite())
        || !(50.0..=240.0).contains(&a)
        || !(50.0..=240.0).contains(&b)
        || first.confidence < 0.65
        || second.confidence < 0.65
        || (a - b).abs() / a.max(b) > 0.035
        || outgoing.trailing_silence_ms > 100
        || incoming.leading_silence_ms > 100
        || outgoing.outro_rms < 0.005
        || incoming.intro_rms < 0.005
        || first.energy_rms <= 0.0
        || second.energy_rms <= 0.0
    {
        return 0.0;
    }
    let level_difference = (20.0 * (outgoing.outro_rms / incoming.intro_rms).log10()).abs();
    let energy_difference = (20.0 * (first.energy_rms / second.energy_rms).log10()).abs();
    if level_difference > 4.0 || energy_difference > 5.0 {
        return 0.0;
    }
    // Four approximate beats bound a short fade; this does not align beats.
    (240.0 / ((a + b) / 2.0)).min(maximum.clamp(0.0, 8.0))
}

/// Caller supplies only available files. Queued IDs include manual Play Next
/// entries and session history, so recommendations neither replace nor repeat them.
pub fn recommend<'a>(
    first: TrackAnalysis,
    queued: &[String],
    candidates: impl Iterator<Item = (&'a str, TrackAnalysis)>,
    maximum: f64,
) -> Option<String> {
    let queued = queued
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    candidates
        .filter(|(id, second)| {
            !queued.contains(id) && transition_seconds(first, *second, maximum) > 0.0
        })
        .min_by(|(a, a_info), (b, b_info)| {
            let distance = |info: &TrackAnalysis| {
                (first.bpm.unwrap_or(0.0) - info.bpm.unwrap_or(0.0)).abs()
                    + (first.energy_rms - info.energy_rms).abs() * 100.0
            };
            distance(a_info)
                .total_cmp(&distance(b_info))
                .then_with(|| a.cmp(b))
        })
        .map(|(id, _)| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mix::BoundaryAnalysis;

    fn analysis() -> TrackAnalysis {
        TrackAnalysis {
            bpm: Some(120.0),
            confidence: 0.9,
            energy_rms: 0.1,
            boundaries: Some(BoundaryAnalysis {
                intro_rms: 0.1,
                outro_rms: 0.1,
                leading_silence_ms: 0,
                trailing_silence_ms: 0,
            }),
        }
    }

    #[test]
    fn recommendations_preserve_manual_queue_and_do_not_repeat_it() {
        let queue = vec!["current".into(), "manual-next".into()];
        let before = queue.clone();
        let candidates = [
            ("current", analysis()),
            ("manual-next", analysis()),
            ("compatible", analysis()),
        ];
        assert_eq!(
            recommend(analysis(), &queue, candidates.into_iter(), 6.0),
            Some("compatible".into())
        );
        assert_eq!(queue, before);
        assert_eq!(
            recommend(analysis(), &queue, candidates[..2].iter().copied(), 6.0),
            None
        );
    }

    #[test]
    fn matching_tempos_alone_do_not_justify_a_fade() {
        let first = analysis();
        for second in [
            TrackAnalysis {
                boundaries: None,
                ..first
            },
            TrackAnalysis {
                confidence: 0.2,
                ..first
            },
            TrackAnalysis {
                energy_rms: 0.8,
                ..first
            },
            TrackAnalysis {
                boundaries: Some(BoundaryAnalysis {
                    leading_silence_ms: 500,
                    ..first.boundaries.unwrap()
                }),
                ..first
            },
            TrackAnalysis {
                bpm: Some(f64::NAN),
                ..first
            },
        ] {
            assert_eq!(transition_seconds(first, second, 6.0), 0.0);
        }
        assert_eq!(transition_seconds(first, first, 6.0), 2.0);
        assert_eq!(transition_seconds(first, first, 1.0), 1.0);
    }
}
