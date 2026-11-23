//! Golden outputs: the fixtures in `tests/fixtures/*.pen` are exported to HTML + CSS and
//! HTML + Tailwind and compared (exact text) with `tests/golden/<fixture>.<format>/`.
//!
//! A missing reference is written (bootstrap); `PEN_CODEGEN_UPDATE_GOLDEN=1` rewrites them; a
//! divergent output is written next to it as `*.actual`. The full exports (with assets) also
//! go to `target/pen-codegen-golden/` for a look in a browser.

use pen_codegen::{CodeFormat, CodeOutput, CodegenOptions};
use pen_doc::{FsLibraries, SceneDoc};
use pen_layout::{FsAssets, Pipeline, TextEngine};
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn export(name: &str, format: CodeFormat) -> CodeOutput {
    let dir = crate_dir().join("tests/fixtures");
    let bytes = std::fs::read(dir.join(format!("{name}.pen"))).unwrap();
    let (mut scene, _) = SceneDoc::load(&bytes, Some(&dir), &FsLibraries::default()).unwrap();
    let mut pipe = Pipeline::new(
        TextEngine::new(),
        Box::new(FsAssets::new(Some(dir.clone()))),
    );
    pipe.rebuild(&mut scene);
    pen_codegen::generate(
        &scene,
        &pipe.layout,
        &[],
        &CodegenOptions::with_format(format),
    )
}

fn check(path: &Path, actual: &str, failures: &mut Vec<String>) {
    let update = std::env::var_os("PEN_CODEGEN_UPDATE_GOLDEN").is_some();
    let actual_path = PathBuf::from(format!("{}.actual", path.display()));
    match std::fs::read_to_string(path) {
        Ok(expected) if !update => {
            // Git on Windows may check the references out with CRLF.
            if expected.replace("\r\n", "\n") == actual {
                let _ = std::fs::remove_file(&actual_path);
            } else {
                std::fs::write(&actual_path, actual).unwrap();
                failures.push(format!(
                    "{} differs (see {})",
                    path.display(),
                    actual_path.display()
                ));
            }
        }
        _ => {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, actual).unwrap();
            let _ = std::fs::remove_file(&actual_path);
        }
    }
}

#[test]
fn golden_outputs() {
    let mut failures = Vec::new();
    let out_root = crate_dir().join("../../target/pen-codegen-golden");
    for name in ["card", "shapes", "text"] {
        for format in [CodeFormat::HtmlCss, CodeFormat::HtmlTailwind] {
            let out = export(name, format);
            let tag = format!("{name}.{}", format.as_str());
            let gdir = crate_dir().join("tests/golden").join(&tag);
            check(&gdir.join("index.html"), &out.html, &mut failures);
            if let Some(css) = &out.css {
                check(&gdir.join("styles.css"), css, &mut failures);
            }
            // Deterministic: a second run gives the same text.
            let again = export(name, format);
            assert_eq!(out.html, again.html, "{tag}: html not deterministic");
            assert_eq!(out.css, again.css, "{tag}: css not deterministic");
            out.write_to(&out_root.join(&tag)).unwrap();
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn assets_and_warnings() {
    let out = export("shapes", CodeFormat::HtmlCss);
    assert_eq!(out.assets.len(), 1);
    assert_eq!(out.assets[0].path, "assets/checker.png");
    assert!(
        out.html.contains("src=\"assets/checker.png\""),
        "{}",
        out.html
    );
    let out = export("card", CodeFormat::HtmlCss);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
}
