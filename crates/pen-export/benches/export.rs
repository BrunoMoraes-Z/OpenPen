//! SVG export of the largest corpus document (skipped when `testdata/` is absent).
//!
//! `cargo bench -p pen-export`

use criterion::{Criterion, criterion_group, criterion_main};
use pen_doc::{FsLibraries, SceneDoc};
use pen_export::{ExportOptions, TextMode};
use pen_layout::{FsAssets, Pipeline, TextEngine};
use pen_raster::RasterContext;
use std::hint::black_box;
use std::path::{Path, PathBuf};

fn largest_corpus_file() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    ["testdata/user", "testdata/legacy"]
        .iter()
        .filter_map(|d| std::fs::read_dir(root.join(d)).ok())
        .flat_map(|rd| rd.flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pen"))
        .max_by_key(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
}

fn bench(c: &mut Criterion) {
    let Some(path) = largest_corpus_file() else {
        eprintln!("corpus not found; skipping pen-export bench");
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let dir = path.parent().unwrap();
    let (mut scene, _) = SceneDoc::load(&bytes, Some(dir), &FsLibraries::default()).unwrap();
    let mut pipe = Pipeline::new(
        TextEngine::new(),
        Box::new(FsAssets::new(Some(dir.to_path_buf()))),
    );
    pipe.rebuild(&mut scene);
    let list = std::mem::take(&mut pipe.list);
    let name = path.file_stem().unwrap().to_string_lossy().to_string();
    let mut ctx = RasterContext::new();
    let text = ExportOptions::default();
    let outlines = ExportOptions {
        text: TextMode::Outlines,
        ..ExportOptions::default()
    };
    // Warm caches (system fonts, shaping) outside the measurement.
    let _ = pen_export::svg::export_with(&mut ctx, &list, &outlines);
    c.bench_function(&format!("svg_export_text/{name}"), |b| {
        b.iter(|| {
            black_box(pen_export::svg::export_with(
                &mut ctx,
                black_box(&list),
                &text,
            ))
        })
    });
    c.bench_function(&format!("svg_export_outlines/{name}"), |b| {
        b.iter(|| {
            black_box(pen_export::svg::export_with(
                &mut ctx,
                black_box(&list),
                &outlines,
            ))
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench
}
criterion_main!(benches);
