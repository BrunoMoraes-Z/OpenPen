//! Corpus smoke test: every top-level frame of `testdata/user/*.pen` and `testdata/legacy/*.pen`
//! is exported to HTML + CSS and HTML + Tailwind without panics; the output must be well formed
//! enough (balanced tags, one rule per class). Outputs go to `target/pen-codegen-corpus/` only.
//! Skips when the corpus folders are absent.

use pen_codegen::{CodeFormat, CodegenOptions};
use pen_doc::{FsLibraries, Kind, SceneDoc};
use pen_layout::{FsAssets, Pipeline, TextEngine};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["testdata/user", "testdata/legacy"] {
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

/// Opening and closing tags of the elements we emit must balance.
fn balanced(html: &str) -> bool {
    let mut depth = 0i64;
    for tag in ["div", "p", "svg", "iframe"] {
        let open =
            html.matches(&format!("<{tag} ")).count() + html.matches(&format!("<{tag}>")).count();
        let close = html.matches(&format!("</{tag}>")).count();
        depth += open as i64 - close as i64;
    }
    depth == 0
}

#[test]
fn corpus_exports_html() {
    let files = files();
    if files.is_empty() {
        eprintln!("corpus not found; skipping");
        return;
    }
    let out_root = root().join("target/pen-codegen-corpus");
    let mut text = Some(TextEngine::new());
    let mut frames = 0;
    let started = std::time::Instant::now();
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let name = path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .replace('.', "_");
        let dir = path.parent().unwrap();
        let (mut scene, _) = SceneDoc::load(&bytes, Some(dir), &FsLibraries::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut pipe = Pipeline::new(
            text.take().unwrap(),
            Box::new(FsAssets::new(Some(dir.to_path_buf()))),
        );
        pipe.rebuild(&mut scene);
        let roots: Vec<_> = scene
            .roots()
            .iter()
            .copied()
            .filter(|&k| scene.get(k).is_some_and(|n| n.kind == Kind::Frame))
            .collect();
        for (i, &k) in roots.iter().enumerate() {
            for format in [CodeFormat::HtmlCss, CodeFormat::HtmlTailwind] {
                let opts = CodegenOptions::with_format(format);
                let out = pen_codegen::generate(&scene, &pipe.layout, &[k], &opts);
                assert!(out.html.contains("<body>"), "{name}#{i}");
                assert!(
                    balanced(&out.html),
                    "{name}#{i} {}: unbalanced tags",
                    format.as_str()
                );
                if let Some(css) = &out.css {
                    assert_eq!(
                        css.matches('{').count(),
                        css.matches('}').count(),
                        "{name}#{i}: unbalanced css"
                    );
                }
                // Keep a few per file to look at; never the assets (not ours to copy around).
                if i < 4 {
                    let d = out_root.join(format!("{name}-{i}.{}", format.as_str()));
                    std::fs::create_dir_all(&d).unwrap();
                    std::fs::write(d.join("index.html"), &out.html).unwrap();
                    if let Some(css) = &out.css {
                        std::fs::write(d.join("styles.css"), css).unwrap();
                    }
                }
            }
            frames += 1;
        }
        text = Some(std::mem::replace(&mut pipe.text, TextEngine::new()));
    }
    eprintln!(
        "exported {frames} frames x 2 formats in {} ms",
        started.elapsed().as_millis()
    );
}
