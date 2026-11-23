//! Corpus: every `.pen` in testdata/legacy and testdata/user builds a scene,
//! lays out and produces a DisplayList without panics or NaN bounds. A few
//! top-level frames are rendered to `target/render-check/` for visual checks.
//! Skips when the corpus folders are absent.

use pen_doc::{FsLibraries, SceneDoc};
use pen_layout::{FsAssets, Pipeline, TextEngine};
use pen_render::{Color, DisplayItem, Rect};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["testdata/legacy", "testdata/user"] {
        let Ok(rd) = std::fs::read_dir(root().join(dir)) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "pen") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn finite(r: &Rect) -> bool {
    r.x.is_finite()
        && r.y.is_finite()
        && r.w.is_finite()
        && r.h.is_finite()
        && r.w >= 0.0
        && r.h >= 0.0
}

#[test]
fn corpus_builds_lays_out_and_renders() {
    let files = files();
    if files.is_empty() {
        eprintln!("corpus not found; skipping");
        return;
    }
    let mut text = Some(TextEngine::new());
    let out_dir = root().join("target/render-check");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut raster = pen_raster::RasterContext::new();
    // (file stem, how many top-level frames to render)
    let render: &[(&str, usize)] = &[
        ("pencil-welcome", 1),
        ("pencil-shadcn", 2),
        ("pencil-heroui", 2),
        ("pencil-demo", 2),
        ("t-process-recorder", 3),
        ("azure_management", 2),
        ("genie", 1),
        ("maark", 1),
        ("my_torrent", 1),
        ("pencil-lunaris", 1),
    ];
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let name = path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .replace(".lib", "_lib");
        let dir = path.parent().unwrap();
        let (mut scene, _) = match SceneDoc::load(&bytes, Some(dir), &FsLibraries::default()) {
            Ok(s) => s,
            Err(e) => panic!("{name}: {e}"),
        };
        let t0 = std::time::Instant::now();
        let mut pipe = Pipeline::new(
            text.take().unwrap(),
            Box::new(FsAssets::new(Some(dir.to_path_buf()))),
        );
        pipe.rebuild(&mut scene);
        let dt = t0.elapsed();
        for c in &pipe.list.chunks {
            assert!(finite(&c.bounds), "{name}: bad chunk bounds {:?}", c.bounds);
            for it in &c.items {
                if let DisplayItem::PushLayer {
                    bounds, opacity, ..
                } = it
                {
                    assert!(
                        finite(bounds) && opacity.is_finite(),
                        "{name}: bad layer {bounds:?}"
                    );
                }
                if let DisplayItem::Shape { transform, .. } | DisplayItem::Text { transform, .. } =
                    it
                {
                    assert!(transform.is_finite(), "{name}: bad transform");
                }
            }
        }
        let warn = scene.warnings().len();
        eprintln!(
            "{name}: {} nodes, {} roots, {} chunks, {} items, {} warnings, layout+dl {:?}",
            scene.node_count(),
            scene.roots().len(),
            pipe.list.chunks.len(),
            pipe.list.item_count(),
            warn,
            dt
        );
        if let Some((_, n)) = render.iter().find(|(s, _)| *s == name) {
            let mut done = 0;
            for &r in scene.roots() {
                if done >= *n {
                    break;
                }
                let Some(rect) = pipe.node_rect(&scene, r) else {
                    continue;
                };
                if rect.w < 50.0 || rect.h < 50.0 {
                    continue;
                }
                let list = pipe.node_list(&scene, r);
                let vp = Rect::new(rect.x, rect.y, rect.w.min(3000.0), rect.h.min(3000.0));
                let png = raster.render_to_png(&list, vp, 1.0, Color::WHITE).unwrap();
                let id = scene.get(r).unwrap().id.to_string();
                std::fs::write(out_dir.join(format!("{name}-{id}.png")), png).unwrap();
                done += 1;
            }
        }
        text = Some(pipe.text);
    }
}
