//! `openpen-cli export-code`: HTML + CSS / HTML + Tailwind (pen-codegen).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

#[derive(Args)]
pub struct ExportCodeArgs {
    input: PathBuf,
    /// Output directory (`index.html`, `styles.css`, `assets/`).
    #[arg(short, long)]
    output: PathBuf,
    /// `html-css` or `html-tailwind`.
    #[arg(long, default_value = "html-css")]
    format: String,
    /// Export only this node (slash id path; repeatable). Default: every top-level node.
    #[arg(long)]
    node: Vec<String>,
    /// Theme selection `axis=value` (repeatable).
    #[arg(long = "theme", value_parser = super::parse_kv)]
    theme: Vec<(String, String)>,
    /// Only the markup, without `<!doctype html>`/`<head>`/`<body>`.
    #[arg(long)]
    no_scaffold: bool,
    /// Add `data-name` (layer name) attributes.
    #[arg(long)]
    layer_names: bool,
    /// Add `data-id` (node id path) attributes.
    #[arg(long)]
    layer_ids: bool,
    /// Do not link Google Fonts.
    #[arg(long)]
    no_google_fonts: bool,
}

pub fn run(a: &ExportCodeArgs) -> Result<()> {
    let format = pen_codegen::CodeFormat::parse(&a.format).with_context(|| {
        format!(
            "unknown format {} (use html-css or html-tailwind)",
            a.format
        )
    })?;
    let bytes =
        std::fs::read(&a.input).with_context(|| format!("reading {}", a.input.display()))?;
    let dir = a
        .input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (mut scene, _) =
        pen_doc::SceneDoc::load(&bytes, Some(dir), &pen_doc::FsLibraries::default())
            .with_context(|| format!("loading {}", a.input.display()))?;
    for (k, v) in &a.theme {
        scene.set_theme(k, v);
    }
    for w in scene.warnings() {
        eprintln!("warning: {w}");
    }
    let mut pipe = pen_layout::Pipeline::new(
        pen_layout::TextEngine::new(),
        Box::new(pen_layout::NoAssets),
    );
    pipe.set_script_runner(Box::new(pen_script::ScriptNodeRunner::new()));
    pipe.rebuild(&mut scene);
    let roots = a
        .node
        .iter()
        .map(|id| {
            scene
                .find(id)
                .with_context(|| format!("node not found: {id}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let opts = pen_codegen::CodegenOptions {
        format,
        scaffold: !a.no_scaffold,
        layer_names: a.layer_names,
        layer_ids: a.layer_ids,
        google_fonts: !a.no_google_fonts,
        ..Default::default()
    };
    let out = pen_codegen::generate(&scene, &pipe.layout, &roots, &opts);
    for w in &out.warnings {
        eprintln!("warning: {w}");
    }
    let files = out
        .write_to(&a.output)
        .with_context(|| format!("writing {}", a.output.display()))?;
    eprintln!("wrote {} file(s) to {}", files.len(), a.output.display());
    Ok(())
}
