//! Derived-index benchmarks. Every library reload regroups the whole track
//! list into albums, artists and genres.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use muscli::{
    db::{group_albums, group_artists},
    features::group_genres,
    model::Track,
};

fn synthetic_tracks(count: usize) -> Vec<Track> {
    (0..count)
        .map(|index| Track {
            id: format!("track-{index:06}"),
            source_id: "bench".into(),
            relative_path: format!("{index}.flac"),
            path: format!("/music/{index}.flac").into(),
            title: format!("Track {index:06}"),
            artist: format!("Artist {:04}", index % 900),
            album_artist: format!("Artist {:04}", index % 900),
            album: format!("Album {:04}", index % 3_000),
            genre: match index % 8 {
                0 => "Classical",
                1 => "Electronic",
                2 => "Jazz",
                3 => "Rock",
                4 => "Ambient",
                5 => "Metal",
                6 => "Hip Hop",
                _ => "",
            }
            .into(),
            year: Some(1970 + (index % 50) as i32),
            disc_number: (index % 2) as u32 + 1,
            track_number: (index % 18) as u32 + 1,
            duration_ms: 200_000,
            cover_path: None,
            available: true,
            favorite: false,
        })
        .collect()
}

fn grouping(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("grouping");
    for size in [5_000usize, 50_000] {
        let tracks = synthetic_tracks(size);
        group.bench_with_input(
            BenchmarkId::new("albums", size),
            &tracks,
            |bencher, tracks| bencher.iter(|| group_albums(black_box(tracks))),
        );
        group.bench_with_input(
            BenchmarkId::new("artists", size),
            &tracks,
            |bencher, tracks| bencher.iter(|| group_artists(black_box(tracks))),
        );
        group.bench_with_input(
            BenchmarkId::new("genres", size),
            &tracks,
            |bencher, tracks| bencher.iter(|| group_genres(black_box(tracks))),
        );
    }
    group.finish();
}

criterion_group!(benches, grouping);
criterion_main!(benches);
