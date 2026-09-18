//! Library scan benchmarks.
//!
//! The scan is the slowest operation muscli performs and the one under active
//! optimisation, so it needs a repeatable measurement. Two cases matter and
//! behave very differently:
//!
//! * a cold scan, where every file has its tags parsed, and
//! * a warm rescan, where nearly every file should be dismissed by the
//!   fingerprint fast path.

// The fixture generator lives with the integration tests; share it rather than
// keeping a second copy in sync.
#[path = "../tests/common/mod.rs"]
mod common;

use std::path::Path;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use muscli::{
    db::Database,
    library::{ScanOptions, scan_to_database},
};

use common::{Fixture, TrackSpec, png_bytes, write_track};

const CACHE_LIMIT: u64 = 64 * 1024 * 1024;
const TRACKS: usize = 300;

fn options(threads: usize) -> ScanOptions {
    ScanOptions {
        threads,
        cover_cache_bytes: CACHE_LIMIT,
    }
}

/// Build a library tree once; every iteration reuses these files.
fn populate(fixture: &Fixture, tracks: usize, with_art: bool) {
    for index in 0..tracks {
        let mut spec = TrackSpec::new(&format!("Track {index:04}"))
            .artist(&format!("Artist {:02}", index % 25))
            .album(&format!("Album {:03}", index % 60))
            .genre(if index % 3 == 0 { "Electronic" } else { "Rock" })
            .track_number((index % 12) as u32 + 1)
            // Short fixtures keep the tree small; tag parsing cost dominates
            // either way because it does not depend on stream length.
            .millis(64);
        if with_art {
            // One distinct cover per album, so the artwork cache is exercised
            // with both hits and misses the way a real library would.
            let shade = (index % 60) as u8;
            spec = spec.cover(png_bytes(160, 160, [shade, 90, 200 - shade]));
        }
        write_track(
            &fixture.source().join(format!(
                "Artist {:02}/Album {:03}/{index:04}.flac",
                index % 25,
                index % 60
            )),
            &spec,
        );
    }
}

fn fresh_database(path: &Path) -> Database {
    // Start every cold-scan iteration from an empty index.
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
    Database::open(path).expect("opening the benchmark database")
}

fn bench_cold(criterion: &mut Criterion, name: &str, with_art: bool) {
    let fixture = Fixture::new();
    populate(&fixture, TRACKS, with_art);
    let roots = vec![fixture.source()];
    let database_file = fixture.paths().database_file();

    let mut group = criterion.benchmark_group("scan");
    group.sample_size(10);
    // Thread counts rather than a single figure: the speedup is the point, and
    // it is very different for tag parsing than for artwork decoding.
    for threads in [1usize, 2, 4] {
        group.bench_with_input(
            BenchmarkId::new(name, threads),
            &threads,
            |bencher, &threads| {
                bencher.iter(|| {
                    if with_art {
                        // Artwork decoding and thumbnailing is the CPU-bound
                        // half of a cold scan, so clear the cover cache too.
                        for cover in fixture.cover_cache_files() {
                            let _ = std::fs::remove_file(cover);
                        }
                    }
                    let mut db = fresh_database(&database_file);
                    scan_to_database(&mut db, fixture.paths(), &roots, options(threads))
                        .expect("scanning the benchmark library")
                })
            },
        );
    }
    group.finish();
}

fn cold_scan(criterion: &mut Criterion) {
    bench_cold(criterion, "cold_no_artwork", false);
}

fn cold_scan_with_artwork(criterion: &mut Criterion) {
    bench_cold(criterion, "cold_with_artwork", true);
}

fn warm_rescan(criterion: &mut Criterion) {
    let fixture = Fixture::new();
    populate(&fixture, TRACKS, false);
    let roots = vec![fixture.source()];
    let mut db =
        Database::open(&fixture.paths().database_file()).expect("opening the benchmark database");
    scan_to_database(&mut db, fixture.paths(), &roots, options(1)).expect("priming the index");

    let mut group = criterion.benchmark_group("scan");
    group.sample_size(20);
    for threads in [1usize, 4] {
        group.bench_with_input(
            BenchmarkId::new("warm_rescan", threads),
            &threads,
            |bencher, &threads| {
                bencher.iter(|| {
                    scan_to_database(&mut db, fixture.paths(), &roots, options(threads))
                        .expect("rescanning the benchmark library")
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, cold_scan, cold_scan_with_artwork, warm_rescan);
criterion_main!(benches);
