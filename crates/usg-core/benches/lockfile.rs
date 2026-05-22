#[path = "common.rs"]
mod common;

use criterion::{Criterion, criterion_group, criterion_main};
use unity_solution_generator::lockfile;

fn bench_lockfile_io(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfile_io");
    let lockfile = common::minimal_lockfile();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("csproj.lock").to_string_lossy().into_owned();

    group.bench_function("write_initial", |b| {
        b.iter(|| {
            lockfile::write(&lockfile, &path).unwrap();
        });
    });

    lockfile::write(&lockfile, &path).unwrap();
    group.bench_function("read", |b| {
        b.iter(|| {
            let _ = lockfile::read(&path).unwrap();
        });
    });

    group.finish();
}

criterion_group!(benches, bench_lockfile_io);
criterion_main!(benches);
