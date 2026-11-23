//! Corpus: every `testdata/**/*.pen` that has `script` nodes or `shader` paints/effects
//! renders without panics with both engines plugged in (skips when `testdata/` is missing;
//! the current corpus has none, so this mostly guards future files).

use std::path::{Path, PathBuf};
use std::sync::Arc;

fn pens(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            pens(&p, out);
        } else if p.extension().is_some_and(|x| x == "pen") {
            out.push(p);
        }
    }
}

#[test]
fn corpus_with_scripts_or_shaders_renders() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
    if !root.exists() {
        eprintln!("skip: testdata/ not found");
        return;
    }
    let mut files = Vec::new();
    pens(&root, &mut files);
    let mut checked = 0;
    for f in files {
        let bytes = std::fs::read(&f).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let uses = text.contains("\"script\"") || text.contains("\"shader\"");
        if !uses {
            continue;
        }
        let dir = f.parent().map(Path::to_path_buf);
        let (mut scene, _) =
            pen_doc::SceneDoc::load(&bytes, dir.as_deref(), &pen_doc::FsLibraries::default())
                .unwrap();
        let mut pipe = pen_layout::Pipeline::new(
            pen_layout::TextEngine::new(),
            Box::new(pen_layout::FsAssets::new(dir)),
        );
        pipe.set_script_runner(Box::new(pen_script::ScriptNodeRunner::new()));
        pipe.set_shader_renderer(Arc::new(pen_shader::ShaderEngine::new()));
        pipe.rebuild(&mut scene);
        for &r in scene.roots() {
            if let Some(rect) = pipe.node_rect(&scene, r) {
                let _ = pen_raster::render(&pipe.list, rect, 0.25, pen_render::Color::WHITE);
            }
        }
        checked += 1;
    }
    eprintln!("{checked} corpus files with scripts/shaders");
}
