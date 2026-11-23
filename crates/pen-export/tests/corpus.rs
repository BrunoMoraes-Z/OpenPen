//! Corpus: every `.pen` in testdata/legacy and testdata/user is exported to SVG (real text and
//! outlines) and PDF (whole document, plus one page per top-level frame) without panics; the
//! SVG must parse back. Outputs go to `target/pen-export-corpus/` only. Skips when the corpus
//! folders are absent.

use pen_doc::{FsLibraries, SceneDoc};
use pen_export::{ExportOptions, TextMode};
use pen_layout::{FsAssets, Pipeline, TextEngine};
use pen_raster::RasterContext;
use pen_render::{Color, NodeKey};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use svg2pdf::usvg;

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

#[test]
fn corpus_exports_svg_and_pdf() {
    let files = files();
    if files.is_empty() {
        eprintln!("corpus not found; skipping");
        return;
    }
    let out_dir = root().join("target/pen-export-corpus");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut text = Some(TextEngine::new());
    let mut ctx = RasterContext::new();
    let usvg_opts = usvg::Options {
        fontdb: Arc::new(pen_fonts::system::bundled_db()),
        ..usvg::Options::default()
    };
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let name = path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .replace(".lib", "_lib");
        let dir = path.parent().unwrap();
        let (mut scene, _) = SceneDoc::load(&bytes, Some(dir), &FsLibraries::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut pipe = Pipeline::new(
            text.take().unwrap(),
            Box::new(FsAssets::new(Some(dir.to_path_buf()))),
        );
        pipe.rebuild(&mut scene);
        let list = std::mem::take(&mut pipe.list);

        // Whole document.
        let t0 = std::time::Instant::now();
        let svg = pen_export::svg::export_with(&mut ctx, &list, &ExportOptions::default());
        let t_svg = t0.elapsed();
        usvg::Tree::from_str(&svg, &usvg_opts)
            .unwrap_or_else(|e| panic!("{name}: exported SVG does not parse: {e}"));
        std::fs::write(out_dir.join(format!("{name}.svg")), &svg).unwrap();

        let outlined = ExportOptions {
            text: TextMode::Outlines,
            background: Some(Color::WHITE),
            ..ExportOptions::default()
        };
        let t0 = std::time::Instant::now();
        let svg_o = pen_export::svg::export_with(&mut ctx, &list, &outlined);
        let t_svg_o = t0.elapsed();
        usvg::Tree::from_str(&svg_o, &usvg_opts)
            .unwrap_or_else(|e| panic!("{name}: outlined SVG does not parse: {e}"));

        let t0 = std::time::Instant::now();
        let pdf = pen_export::pdf::export_pages_with(&mut ctx, &list, &[ExportOptions::default()])
            .unwrap_or_else(|e| panic!("{name}: PDF export failed: {e}"));
        let t_pdf = t0.elapsed();
        assert!(pdf.starts_with(b"%PDF-"), "{name}: not a PDF");
        std::fs::write(out_dir.join(format!("{name}.pdf")), &pdf).unwrap();

        // One page per top-level frame (first few), like an "export frames" action.
        let roots: Vec<NodeKey> = scene
            .roots()
            .iter()
            .copied()
            .filter(|r| list.chunks.iter().any(|c| c.root == *r))
            .take(4)
            .collect();
        if !roots.is_empty() {
            let pages: Vec<ExportOptions> = roots
                .iter()
                .map(|r| ExportOptions {
                    region: pipe.node_rect(&scene, *r),
                    ..ExportOptions::root(*r)
                })
                .collect();
            let pdf = pen_export::pdf::export_pages_with(&mut ctx, &list, &pages)
                .unwrap_or_else(|e| panic!("{name}: frames PDF failed: {e}"));
            std::fs::write(out_dir.join(format!("{name}-frames.pdf")), &pdf).unwrap();
        }
        eprintln!(
            "{name}: {} items, svg {} KB in {t_svg:?}, outlined {} KB in {t_svg_o:?}, pdf {} KB in {t_pdf:?}",
            list.item_count(),
            svg.len() / 1024,
            svg_o.len() / 1024,
            pdf.len() / 1024,
        );
        text = Some(pipe.text);
    }
}
