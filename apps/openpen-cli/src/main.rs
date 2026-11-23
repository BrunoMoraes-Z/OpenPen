//! OpenPen headless CLI: `validate`, `migrate` and `render`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pen_schema::{Severity, load_document, save_document, save_document_with, validate};

#[derive(Parser)]
#[command(
    name = "openpen-cli",
    version,
    about = "OpenPen headless tools for .pen files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load, migrate (in memory) and validate files; exits with 1 on errors.
    Validate {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Also print info-level diagnostics.
        #[arg(short, long)]
        verbose: bool,
        /// Treat warnings as errors.
        #[arg(long)]
        strict: bool,
    },
    /// Migrate a file to the current schema version.
    Migrate {
        input: PathBuf,
        /// Output path (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Render a document (or one node) to PNG with the CPU rasterizer.
    Render {
        input: PathBuf,
        /// Output PNG path.
        #[arg(short, long)]
        output: PathBuf,
        /// Render only this node (slash id path, e.g. `frame1` or `inst/child`).
        #[arg(long)]
        node: Option<String>,
        /// Device pixels per document unit.
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Theme selection `axis=value` (repeatable).
        #[arg(long = "theme", value_parser = parse_kv)]
        theme: Vec<(String, String)>,
        /// Background color `#RRGGBB[AA]` (default: transparent).
        #[arg(long)]
        bg: Option<String>,
    },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .ok_or_else(|| format!("expected axis=value, got {s}"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Validate {
            files,
            verbose,
            strict,
        } => run_validate(&files, verbose, strict),
        Command::Migrate { input, output } => run_migrate(&input, output.as_deref()).map(|()| true),
        Command::Render {
            input,
            output,
            node,
            scale,
            theme,
            bg,
        } => run_render(
            &input,
            &output,
            node.as_deref(),
            scale,
            &theme,
            bg.as_deref(),
        )
        .map(|()| true),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Returns `Ok(false)` when any file has errors.
fn run_validate(files: &[PathBuf], verbose: bool, strict: bool) -> Result<bool> {
    let mut ok = true;
    for path in files {
        let name = path.display();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                println!("{name}: error: cannot read: {e}");
                ok = false;
                continue;
            }
        };
        let (doc, report) = match load_document(&bytes) {
            Ok(r) => r,
            Err(e) => {
                println!("{name}: error: {e}");
                ok = false;
                continue;
            }
        };
        let mut diags = report.warnings.clone();
        diags.extend(validate(&doc));
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count();
        for d in &diags {
            if verbose || d.severity >= Severity::Warning {
                println!("{name}: {d}");
            }
        }
        let migrated = if report.migrated() {
            format!(
                ", migrated {} -> {}",
                report.source_version,
                pen_schema::CURRENT_VERSION
            )
        } else {
            String::new()
        };
        println!(
            "{name}: {} nodes, {errors} error(s), {warnings} warning(s){migrated}",
            doc.node_count()
        );
        if errors > 0 || (strict && warnings > 0) {
            ok = false;
        }
    }
    Ok(ok)
}

fn run_migrate(input: &Path, output: Option<&Path>) -> Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let (doc, report) =
        load_document(&bytes).with_context(|| format!("loading {}", input.display()))?;
    for m in &report.migrations {
        eprintln!("migrated {} -> {}", m.from, m.to);
    }
    for w in &report.warnings {
        eprintln!("{w}");
    }
    match output {
        // Keep the input's line endings / trailing newline / BOM.
        Some(out) => std::fs::write(out, save_document_with(&doc, &report.save_options))
            .with_context(|| format!("writing {}", out.display()))?,
        None => println!("{}", save_document(&doc)),
    }
    Ok(())
}

fn run_render(
    input: &Path,
    output: &Path,
    node: Option<&str>,
    scale: f32,
    theme: &[(String, String)],
    bg: Option<&str>,
) -> Result<()> {
    use pen_render::{Color, Rect};
    let bytes = std::fs::read(input).with_context(|| format!("reading {}", input.display()))?;
    let dir = input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (mut scene, _) =
        pen_doc::SceneDoc::load(&bytes, Some(dir), &pen_doc::FsLibraries::default())
            .with_context(|| format!("loading {}", input.display()))?;
    for (k, v) in theme {
        scene.set_theme(k, v);
    }
    for w in scene.warnings() {
        eprintln!("warning: {w}");
    }
    let background = match bg {
        Some(s) => {
            let c = pen_schema::Rgba::parse_hex(s).with_context(|| format!("invalid color {s}"))?;
            Color::rgba(
                c.r as f32 / 255.0,
                c.g as f32 / 255.0,
                c.b as f32 / 255.0,
                c.a as f32 / 255.0,
            )
        }
        None => Color::TRANSPARENT,
    };
    let mut pipe = pen_layout::Pipeline::new(
        pen_layout::TextEngine::new(),
        Box::new(pen_layout::FsAssets::new(Some(dir.to_path_buf()))),
    );
    pipe.rebuild(&mut scene);
    let (list, viewport) = match node {
        Some(id) => {
            let k = scene
                .find(id)
                .with_context(|| format!("node not found: {id}"))?;
            let list = pipe.node_list(&scene, k);
            let r = pipe
                .node_rect(&scene, k)
                .context("node has no layout (disabled?)")?;
            (list, r)
        }
        None => {
            let list = std::mem::take(&mut pipe.list);
            let r = list.bounds().unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
            (list, r)
        }
    };
    let viewport = Rect::new(
        viewport.x,
        viewport.y,
        viewport.w.max(1.0),
        viewport.h.max(1.0),
    );
    let png = pen_raster::render_to_png(&list, viewport, scale, background)?;
    std::fs::write(output, png).with_context(|| format!("writing {}", output.display()))?;
    eprintln!(
        "rendered {}x{} px ({} items)",
        (viewport.w * scale).ceil(),
        (viewport.h * scale).ceil(),
        list.item_count()
    );
    Ok(())
}
