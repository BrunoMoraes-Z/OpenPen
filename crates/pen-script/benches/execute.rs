//! `execute` benchmarks: a snippet that inserts ~200 nodes (a list of 50 rows
//! with 3 children each, built with a loop), measured end to end (context,
//! prelude, evaluation, layout, commit) and undone after each iteration.
//! Only `execute` is timed (not the undo). `execute/noop` is the fixed cost of one call. The `corpus/*` variants run the
//! same snippet against `testdata/user/t-process-recorder.pen` when present.

use criterion::{Criterion, criterion_group, criterion_main};
use pen_doc::{FsLibraries, SceneDoc, UndoStack};
use pen_layout::{NoAssets, Pipeline, TextEngine};
use pen_script::{ExecuteRequest, HeadlessHost, ScriptEngine};
use std::path::Path;

const INSERT_200: &str = r##"
const list = Insert(document, {type: "frame", name: "List", x: 0, y: -2000, width: 480, layout: "vertical", gap: 4, padding: 16})
const row = {type: "frame", layout: "horizontal", gap: 8, padding: 6, width: "fill_container", alignItems: "center"}
for (let i = 0; i < 50; i++) {
  const r = Insert(list, {...row, name: "Row " + i})
  Insert(r, {type: "ellipse", name: "Dot", width: 8, height: 8, fill: i % 2 ? "#22aa55" : "#aa2255"})
  Insert(r, {type: "text", name: "Label", content: "Item number " + i, fontFamily: "Inter", fontSize: 14, fill: "#111111"})
  Insert(r, {type: "text", name: "Value", content: String(i * 37), fontFamily: "Inter", fontSize: 14, fill: "#666666"})
}
Print(Get(list, {depth: 0}).name)
"##;

fn req(src: &str) -> ExecuteRequest {
    ExecuteRequest {
        agent_id: "bench".into(),
        description: "bench".into(),
        input: Some(src.into()),
        ..Default::default()
    }
}

fn run(c: &mut Criterion, name: &str, mut scene: SceneDoc, src: &'static str) {
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.rebuild(&mut scene);
    let mut undo = UndoStack::new();
    let mut eng = ScriptEngine::new();
    // Only the `execute` call is timed; the undo that resets the document is not.
    c.bench_function(name, |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for _ in 0..iters {
                let t = std::time::Instant::now();
                let r = eng.execute(
                    &mut scene,
                    &mut pipe,
                    &mut undo,
                    &mut HeadlessHost,
                    req(src),
                );
                total += t.elapsed();
                assert!(r.ok, "{}", r.text);
                if r.ops > 0 {
                    undo.undo(&mut scene).unwrap();
                    pipe.update(&mut scene);
                }
            }
            total
        })
    });
}

fn empty() -> SceneDoc {
    SceneDoc::load(
        br#"{"version": "2.22", "children": []}"#,
        None,
        &FsLibraries::default(),
    )
    .unwrap()
    .0
}

fn bench(c: &mut Criterion) {
    run(c, "execute/noop", empty(), "Print(1)");
    run(c, "execute/insert_200", empty(), INSERT_200);
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/user/t-process-recorder.pen");
    if let Ok(bytes) = std::fs::read(&path) {
        let (scene, _) = SceneDoc::load(&bytes, path.parent(), &FsLibraries::default()).unwrap();
        run(c, "corpus/insert_200", scene, INSERT_200);
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
