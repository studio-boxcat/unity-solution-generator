#[path = "common.rs"]
mod common;

use criterion::{Criterion, criterion_group, criterion_main};
use unity_solution_generator::ProjectScanner;

fn bench_scan_cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("project_scanner.scan");
    for (assemblies, files) in [(13, 50), (50, 100), (100, 200)] {
        let id = format!("{}asm_x_{}cs", assemblies, files);
        group.bench_function(&id, |b| {
            b.iter_batched(
                || common::make_project(assemblies, files),
                |fix| {
                    // Cold: scan-cache absent on first call. Subsequent iter() calls inside
                    // a single fixture would hit the cache; iter_batched gives us a fresh
                    // fixture per iteration so we always measure the cold path.
                    ProjectScanner::scan(&fix.root_str(), unity_solution_generator::DEFAULT_GENERATOR_ROOT).unwrap();
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_scan_warm(c: &mut Criterion) {
    let mut group = c.benchmark_group("project_scanner.scan_warm");
    for (assemblies, files) in [(13, 50), (50, 100), (100, 200)] {
        let id = format!("{}asm_x_{}cs", assemblies, files);
        let fix = common::make_project(assemblies, files);
        // Prime the scan-cache.
        ProjectScanner::scan(&fix.root_str(), unity_solution_generator::DEFAULT_GENERATOR_ROOT).unwrap();
        group.bench_function(&id, |b| {
            b.iter(|| {
                ProjectScanner::scan(&fix.root_str(), unity_solution_generator::DEFAULT_GENERATOR_ROOT).unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scan_cold, bench_scan_warm);
criterion_main!(benches);
