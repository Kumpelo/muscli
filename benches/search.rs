//! Search benchmarks: index construction and query latency.
//!
//! `SearchIndex` is rebuilt from scratch on every library reload, so its build
//! cost scales with library size and is paid far more often than a user might
//! expect. Query latency matters because search runs on every keystroke.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use muscli::{features::SearchIndex, model::Track};

fn synthetic_tracks(count: usize) -> Vec<Track> {
    (0..count)
        .map(|index| Track {
            id: format!("track-{index:06}"),
            source_id: "bench".into(),
            relative_path: format!("{index}.flac"),
            path: format!("/music/{index}.flac").into(),
            title: format!("Sonata número {index} en re menor"),
            artist: format!("Orquesta Sinfónica {:03}", index % 200),
            album_artist: format!("Orquesta Sinfónica {:03}", index % 200),
            album: format!("Colección {:03}", index % 400),
            genre: match index % 5 {
                0 => "Clásica",
                1 => "Electrónica",
                2 => "Jazz",
                3 => "Rock",
                _ => "Ambient",
            }
            .into(),
            year: Some(1960 + (index % 60) as i32),
            disc_number: 1,
            track_number: (index % 15) as u32 + 1,
            duration_ms: 180_000 + (index as u64 % 120) * 1_000,
            cover_path: None,
            available: true,
            favorite: index % 17 == 0,
        })
        .collect()
}

fn index_build(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("search_index_build");
    for size in [1_000usize, 10_000, 50_000] {
        let tracks = synthetic_tracks(size);
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &tracks,
            |bencher, tracks| bencher.iter(|| SearchIndex::build(black_box(tracks))),
        );
    }
    group.finish();
}

fn query(criterion: &mut Criterion) {
    let tracks = synthetic_tracks(50_000);
    let index = SearchIndex::build(&tracks);

    let mut group = criterion.benchmark_group("search_query");
    // An exact prefix short-circuits early; a typo forces the bounded edit
    // distance path over every field; a miss is the worst case.
    for (label, needle) in [
        ("prefix_hit", "sonata numero 1234"),
        ("typo", "orquestra sinfonica"),
        ("miss", "zzzzzzzz no existe"),
        ("short", "jazz"),
    ] {
        group.bench_function(label, |bencher| {
            bencher.iter(|| index.search(black_box(needle), 100))
        });
    }
    group.finish();
}

criterion_group!(benches, index_build, query);
criterion_main!(benches);
