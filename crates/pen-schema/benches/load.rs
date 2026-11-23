//! Parse + migrate + typed conversion benchmarks on the largest corpus file.

use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use pen_schema::{load_document, parse_json, save_document};

fn corpus(name: &str) -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(name);
    std::fs::read(path).ok()
}

fn bench_load(c: &mut Criterion) {
    let Some(big) = corpus("user/t-process-recorder.pen") else {
        eprintln!("testdata/user/t-process-recorder.pen not found; skipping");
        return;
    };
    let mut g = c.benchmark_group("t-process-recorder (1.4 MB, 2.22)");
    g.sample_size(30);
    g.bench_function("parse_json", |b| {
        b.iter(|| parse_json(std::hint::black_box(&big)).unwrap())
    });
    g.bench_function("load_document", |b| {
        b.iter(|| load_document(std::hint::black_box(&big)).unwrap())
    });
    let (doc, _) = load_document(&big).unwrap();
    g.bench_function("save_document", |b| {
        b.iter(|| save_document(std::hint::black_box(&doc)))
    });
    g.finish();

    if let Some(old) = corpus("legacy/pencil-demo.pen") {
        let mut g = c.benchmark_group("pencil-demo (1.3 MB, 2.6 -> 2.22)");
        g.sample_size(30);
        g.bench_function("load_document (migrate)", |b| {
            b.iter(|| load_document(std::hint::black_box(&old)).unwrap())
        });
        g.finish();
    }
}

criterion_group!(benches, bench_load);
criterion_main!(benches);
