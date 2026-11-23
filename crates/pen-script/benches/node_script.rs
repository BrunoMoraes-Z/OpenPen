//! `script` node benchmarks: a generator of ~120 nodes (a bar chart with labels).
//!
//! - `node_script/resize_step`: width patch + `Pipeline::update` with a new width every
//!   iteration (the script runs: compiled function call, output migration, scene build,
//!   relayout of the root, chunk rebuild). This is the cost of one resize drag step.
//! - `node_script/resize_cached`: alternates between two widths (outputs re-attached
//!   from the cache, no JavaScript).
//! - `node_script/cold`: new runner each time (header parse + QuickJS context + compile + run).

use criterion::{Criterion, criterion_group, criterion_main};
use pen_doc::{DocOp, SceneDoc};
use pen_layout::{NoAssets, Pipeline, TextEngine};
use pen_script::ScriptNodeRunner;
use std::path::PathBuf;

const CHART: &str = r##"/**
 * @schema 2.11
 * @input bars: number(min=1, max=200) = 40
 * @input color: color = #3B82F6
 */
const n = Math.floor(pencil.input.bars);
const bw = pencil.width / n;
const out = [];
for (let i = 0; i < n; i++) {
  const h = (pencil.height - 20) * (0.2 + 0.8 * Math.random());
  out.push({ type: "rectangle", x: i * bw + 1, y: pencil.height - 20 - h, width: bw - 2, height: h,
    fill: pencil.input.color, cornerRadius: [2, 2, 0, 0] });
  if (i % 2 == 0) out.push({ type: "text", x: i * bw, y: pencil.height - 16, content: String(i),
    fontSize: 9, fill: "#555555" });
}
out.push({ type: "line", x: 0, y: pencil.height - 20, width: pencil.width, height: 0,
  stroke: { fill: "#999999", thickness: 1 } });
return out;
"##;

fn setup() -> (SceneDoc, Pipeline, PathBuf) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/node-scripts/bench");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("chart.js"), CHART).unwrap();
    let json = r#"{ "version": "2.22", "children": [
      { "type": "frame", "id": "page", "x": 0, "y": 0, "width": 800, "height": 400, "layout": "none",
        "children": [ { "type": "script", "id": "s", "x": 10, "y": 10, "width": 600, "height": 300,
          "scriptUri": "chart.js", "inputs": { "bars": 60 } } ] } ] }"#;
    let (mut scene, _) =
        SceneDoc::load(json.as_bytes(), Some(&dir), &pen_doc::NoLibraries).unwrap();
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.set_script_runner(Box::new(ScriptNodeRunner::new()));
    pipe.rebuild(&mut scene);
    (scene, pipe, dir)
}

fn set_width(scene: &mut SceneDoc, w: f64) {
    let k = scene.find("s").unwrap();
    let (node, _) = pen_schema::node_from_value(serde_json::json!({ "width": w }), "bench");
    scene
        .apply(DocOp::set(scene.node_ref(k).unwrap(), node))
        .unwrap();
}

fn benches(c: &mut Criterion) {
    let (mut scene, mut pipe, dir) = setup();
    let k = scene.find("s").unwrap();
    assert!(scene.generated(k).len() > 80, "{:?}", scene.script_error(k));
    let mut w = 600.0;
    c.bench_function("node_script/resize_step", |b| {
        b.iter(|| {
            w += 0.5;
            if w > 700.0 {
                w = 600.0;
            }
            set_width(&mut scene, w);
            pipe.update(&mut scene);
        })
    });
    let mut flip = false;
    c.bench_function("node_script/resize_cached", |b| {
        b.iter(|| {
            flip = !flip;
            set_width(&mut scene, if flip { 400.0 } else { 401.0 });
            pipe.update(&mut scene);
        })
    });
    let src = std::fs::read_to_string(dir.join("chart.js")).unwrap();
    c.bench_function("node_script/cold", |b| {
        b.iter(|| {
            let mut r = ScriptNodeRunner::new();
            r.run_source(&src, &[], 600.0, 300.0).unwrap()
        })
    });
}

criterion_group!(group, benches);
criterion_main!(group);
