//! Scene build, layout and display-list benchmarks on the largest corpus file.
//! Skips when `testdata/user/t-process-recorder.pen` is absent.
//!
//! `open/*` measure what the app does when it opens a file: a fresh `TextEngine`
//! (bundled fonts, system families copied on demand), `FsAssets::deferred`, parse +
//! migrate + scene + layout + display list. `open/system_font_scan` is the one-time
//! per-process scan of the installed fonts (a cold process pays scan + open; the app
//! starts the scan at launch). `edit/drag_step` is one drag step end to end: op +
//! incremental update + the `Arc<DisplayList>` handed to the canvas.

use criterion::{Criterion, criterion_group, criterion_main};
use pen_doc::{DocOp, FsLibraries, Kind, NodeRef, SceneDoc};
use pen_layout::{FsAssets, NoAssets, Pipeline, TextEngine};
use pen_schema::{ColorOrVar, Fill, Node, OneOrMany};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn corpus_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/user/t-process-recorder.pen")
}

fn fill(c: &str) -> Node {
    Node {
        fill: Some(OneOrMany::One(Fill::Solid(ColorOrVar(c.into())))),
        ..Node::default()
    }
}

/// What `SceneDocument::load` does in the app.
fn open(path: &Path) -> (SceneDoc, Pipeline) {
    let bytes = std::fs::read(path).unwrap();
    let dir = path.parent();
    let (mut scene, _) = SceneDoc::load(&bytes, dir, &FsLibraries::default()).unwrap();
    let mut pipe = Pipeline::new(
        TextEngine::new(),
        Box::new(FsAssets::deferred(dir.map(Path::to_path_buf))),
    );
    pipe.rebuild(&mut scene);
    (scene, pipe)
}

fn bench(c: &mut Criterion) {
    let path = corpus_path();
    if !path.exists() {
        eprintln!("t-process-recorder.pen not found; skipping");
        return;
    }

    let mut g = c.benchmark_group("open");
    g.sample_size(20);
    // First iteration in this process: includes the system font scan.
    let t = std::time::Instant::now();
    black_box(open(&path));
    eprintln!(
        "cold open (first in process, scan included): {:?}",
        t.elapsed()
    );
    g.bench_function("system_font_scan", |b| {
        b.iter(|| black_box(pen_fonts::system::SystemFonts::scan().len()))
    });
    g.bench_function("t-process-recorder", |b| b.iter(|| black_box(open(&path))));
    g.finish();

    let bytes = std::fs::read(&path).unwrap();
    let (doc, _) = pen_schema::load_document(&bytes).unwrap();
    let mut scene = SceneDoc::new(doc);
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.rebuild(&mut scene);
    eprintln!(
        "nodes: {}, chunks: {}, items: {}",
        scene.node_count(),
        pipe.list.chunks.len(),
        pipe.list.item_count()
    );

    let mut g = c.benchmark_group("t-process-recorder");
    g.sample_size(20);
    g.bench_function("scene_build", |b| b.iter(|| scene.rebuild_all()));
    pipe.rebuild(&mut scene);
    g.bench_function("layout_full_warm_text", |b| {
        b.iter(|| pipe.layout.layout_all(black_box(&scene), &mut pipe.text))
    });
    g.bench_function("layout_full_cold_text", |b| {
        b.iter(|| {
            pipe.text.clear_caches();
            pipe.layout.layout_all(black_box(&scene), &mut pipe.text)
        })
    });
    g.bench_function("display_list_full", |b| {
        b.iter(|| black_box(pipe.display.build(&scene, &pipe.layout)))
    });
    g.bench_function("full_pipeline_warm_text", |b| {
        b.iter(|| {
            scene.rebuild_all();
            pipe.rebuild(&mut scene);
            let _ = scene.take_changes();
        })
    });

    // Incremental: change one node's fill and update.
    scene.rebuild_all();
    pipe.rebuild(&mut scene);
    let _ = scene.take_changes();
    let target = scene
        .descendants(scene.roots()[0])
        .find(|k| {
            scene
                .get(*k)
                .is_some_and(|n| n.kind == Kind::Frame && !n.fills.is_empty())
        })
        .and_then(|k| scene.node_ref(k))
        .unwrap_or(NodeRef::Id(scene.get(scene.roots()[0]).unwrap().id.clone()));
    let mut flip = false;
    g.bench_function("incremental_fill_edit", |b| {
        b.iter(|| {
            flip = !flip;
            let c = if flip { "#ff0000" } else { "#00ff00" };
            scene.apply(DocOp::set(target.clone(), fill(c))).unwrap();
            black_box(pipe.update(&mut scene));
        })
    });
    let width = Node {
        width: Some(pen_schema::Sizing::Fixed(123.0)),
        ..Node::default()
    };
    let wtarget = target.clone();
    g.bench_function("incremental_size_edit", |b| {
        b.iter(|| {
            let inv = scene
                .apply(DocOp::set(wtarget.clone(), width.clone()))
                .unwrap();
            black_box(pipe.update(&mut scene));
            scene.apply(inv).unwrap();
            black_box(pipe.update(&mut scene));
        })
    });
    g.finish();

    // Drag: move the largest top-level frame by 1 px per step, as the app does
    // (x/y patch, incremental update, a new Arc<DisplayList> for the canvas).
    let mut g = c.benchmark_group("edit");
    g.sample_size(50);
    let root = *scene
        .roots()
        .iter()
        .max_by_key(|&&r| scene.descendants(r).count())
        .unwrap();
    let rref = scene.node_ref(root).unwrap();
    let x0 = scene.get(root).unwrap().x;
    let mut step = 0u32;
    let mut canvas_list: Arc<pen_render::DisplayList> = Arc::new(pipe.list.clone());
    g.bench_function("drag_step", |b| {
        b.iter(|| {
            step += 1;
            let patch = Node {
                x: Some((x0 + (step % 64) as f32) as f64),
                ..Node::default()
            };
            scene.apply(DocOp::set(rref.clone(), patch)).unwrap();
            pipe.update(&mut scene);
            canvas_list = Arc::new(pipe.list.clone());
            black_box(&canvas_list);
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
