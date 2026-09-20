//! What the native path costs in time. The quality side is covered by the
//! tests in `src/audio`.

use criterion::{Criterion, criterion_group, criterion_main};
use muscli::{
    audio::{
        dsp::{Chain, Settings, resample::Resampler},
        measure::tone,
    },
    config::EQUALIZER_BANDS,
};
use std::hint::black_box;

/// One second of stereo at 44.1 kHz, which is the unit that matters: a figure
/// under a millisecond means the work is a thousandth of real time.
const RATE: u32 = 44_100;

fn stereo_second() -> Vec<f32> {
    tone(RATE as usize, 440, 0.5)
        .iter()
        .flat_map(|sample| [*sample, *sample])
        .collect()
}

fn bands(gain: f32) -> Vec<(u32, f32)> {
    EQUALIZER_BANDS.iter().map(|hz| (*hz, gain)).collect()
}

fn chain(criterion: &mut Criterion) {
    let audio = stereo_second();
    let mut group = criterion.benchmark_group("chain");

    group.bench_function("neutral", |bencher| {
        let mut chain = Chain::new(RATE, 2, &Settings::default());
        bencher.iter(|| {
            let mut block = audio.clone();
            chain.process(black_box(&mut block));
            black_box(block);
        });
    });

    group.bench_function("eight bands", |bencher| {
        let settings = Settings {
            volume: 0.7,
            replay_gain_db: Some(-4.0),
            equalizer: bands(3.0),
            ..Settings::default()
        };
        let mut chain = Chain::new(RATE, 2, &settings);
        bencher.iter(|| {
            let mut block = audio.clone();
            chain.process(black_box(&mut block));
            black_box(block);
        });
    });

    group.finish();
}

fn resample(criterion: &mut Criterion) {
    let audio = stereo_second();
    // Built once: the filter tables are a setup cost paid per track, not per
    // second, and folding them in here would overstate what playing costs.
    let mut resampler = Resampler::new(RATE, 48_000, 2).expect("build the converter");
    criterion.bench_function("resample 44.1 to 48", |bencher| {
        bencher.iter(|| {
            let mut output = Vec::new();
            resampler
                .process(black_box(&audio), &mut output)
                .expect("convert");
            black_box(output);
        });
    });
}

criterion_group!(benches, chain, resample);
criterion_main!(benches);
