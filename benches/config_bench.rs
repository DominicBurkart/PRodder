//! Measure the cold parse cost of `Config::load_from` against a
//! freshly-written copy of `prodder.example.toml`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use prodder::config::Config;

fn bench_cold_parse(c: &mut Criterion) {
    let raw = std::fs::read_to_string("prodder.example.toml").expect(
        "run `cargo bench` from the repository root so \
             prodder.example.toml is reachable",
    );
    let dir = std::env::temp_dir();
    let path = dir.join("prodder-bench.toml");
    std::fs::write(&path, &raw).unwrap();

    c.bench_function("Config::load_from(example)", |b| {
        b.iter(|| {
            let cfg =
                Config::load_from(black_box(&path)).expect("load");
            black_box(cfg);
        })
    });
}

criterion_group!(benches, bench_cold_parse);
criterion_main!(benches);
