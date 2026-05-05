#[path = "common.rs"]
mod common;

use criterion::{Criterion, criterion_group, criterion_main};
use usg_core::{BuildConfig, BuildPlatform, GenerateOptions, SolutionGenerator};

fn bench_generate(c: &mut Criterion) {
    let mut group = c.benchmark_group("generate.from_lockfile");
    let lockfile = common::minimal_lockfile();
    for (assemblies, files) in [(13, 50), (50, 100), (100, 200)] {
        let id = format!("{}asm_x_{}cs", assemblies, files);
        let fix = common::make_project(assemblies, files);
        let options = GenerateOptions::new(fix.root_str(), BuildPlatform::Ios)
            .with_generator_root("tpl")
            .with_build_config(BuildConfig::Editor);
        // Warm up scan-cache + write a first variant so on-disk byte-equality short-circuits
        // `write_file_if_changed` for csprojs (steady-state behaviour).
        SolutionGenerator::new()
            .generate_from_lockfile(&options, &lockfile)
            .unwrap();
        group.bench_function(&id, |b| {
            b.iter(|| {
                SolutionGenerator::new()
                    .generate_from_lockfile(&options, &lockfile)
                    .unwrap();
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_generate);
criterion_main!(benches);
