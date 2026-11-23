//! OpenPen headless CLI: `validate`, `migrate`, `render` and `mcp`.

mod headless;

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
    /// Render a document (or one node) to PNG, SVG or PDF (chosen by the output extension).
    Render {
        input: PathBuf,
        /// Output path: `.png` (CPU rasterizer), `.svg` or `.pdf` (vector).
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
        /// SVG/PDF: convert text to glyph outlines instead of real text.
        #[arg(long)]
        outline: bool,
    },
    /// MCP server for agents: bridges stdio to the running OpenPen app.
    Mcp {
        /// Socket name of the app instance (default: `desktop`).
        #[arg(long)]
        socket: Option<String>,
        /// Agent name shown in the editor (default: the MCP client's name).
        #[arg(long)]
        agent: Option<String>,
        /// Serve this .pen file over stdio without the app (in-memory editing, `save` writes it).
        #[arg(long, value_name = "FILE")]
        headless: Option<PathBuf>,
        /// With --headless: never write the file (the `save` tool is not offered).
        #[arg(long, requires = "headless")]
        readonly: bool,
        /// Print the Claude Code `mcpServers` entry for this executable and exit.
        #[arg(long)]
        print_config: bool,
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
            outline,
        } => run_render(
            &input,
            &output,
            node.as_deref(),
            scale,
            &theme,
            bg.as_deref(),
            outline,
        )
        .map(|()| true),
        Command::Mcp {
            socket,
            agent,
            headless,
            readonly,
            print_config,
        } => run_mcp(
            socket.as_deref(),
            agent.as_deref(),
            headless.as_deref(),
            readonly,
            print_config,
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

fn run_mcp(
    socket: Option<&str>,
    agent: Option<&str>,
    headless: Option<&Path>,
    readonly: bool,
    print_config: bool,
) -> Result<()> {
    if print_config {
        let exe = std::env::current_exe().context("locating openpen-cli")?;
        let mut config = pen_mcp::config::claude_code_config(&exe, socket);
        if let Some(file) = headless {
            let file = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
            let mut args = vec![
                "mcp".to_string(),
                "--headless".to_string(),
                file.display().to_string(),
            ];
            if readonly {
                args.push("--readonly".into());
            }
            config["mcpServers"]["openpen"]["args"] = args.into();
        }
        println!("{config:#}");
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let result = match headless {
        Some(file) => {
            let started = std::time::Instant::now();
            let backend = headless::HeadlessBackend::open(file, readonly)?;
            eprintln!(
                "openpen: serving {} headless ({} ms to load){}",
                backend.path().display(),
                started.elapsed().as_millis(),
                if readonly { ", read-only" } else { "" }
            );
            let opts = pen_mcp::McpOptions {
                agent_name: agent.map(str::to_string),
                enable_save: !readonly,
                ..pen_mcp::McpOptions::with_default_skills()
            };
            runtime
                .block_on(pen_mcp::serve_stdio(std::sync::Arc::new(backend), opts))
                .map_err(anyhow::Error::from)
        }
        None => {
            let name = socket.unwrap_or(pen_mcp::socket::DEFAULT_SOCKET_NAME);
            runtime
                .block_on(pen_mcp::socket::run_stdio_shim(name, agent))
                .map_err(Into::into)
        }
    };
    // A blocking stdin read may still be pending; do not wait for it.
    runtime.shutdown_background();
    result
}

fn run_render(
    input: &Path,
    output: &Path,
    node: Option<&str>,
    scale: f32,
    theme: &[(String, String)],
    bg: Option<&str>,
    outline: bool,
) -> Result<()> {
    use pen_render::{Color, Rect};
    let format = output
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(format.as_str(), "png" | "svg" | "pdf") {
        anyhow::bail!(
            "unsupported output format (use .png, .svg or .pdf): {}",
            output.display()
        );
    }
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
    pipe.set_script_runner(Box::new(pen_script::ScriptNodeRunner::new()));
    pipe.set_shader_renderer(std::sync::Arc::new(pen_shader::ShaderEngine::new()));
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
    let bytes = match format.as_str() {
        "png" => pen_raster::render_to_png(&list, viewport, scale, background)?,
        vector => {
            let opts = pen_export::ExportOptions {
                region: Some(viewport),
                scale,
                background: (background.a > 0.0).then_some(background),
                text: if outline {
                    pen_export::TextMode::Outlines
                } else {
                    pen_export::TextMode::Text
                },
                ..Default::default()
            };
            if vector == "svg" {
                pen_export::svg::export(&list, &opts).into_bytes()
            } else {
                pen_export::pdf::export(&list, &opts)?
            }
        }
    };
    std::fs::write(output, bytes).with_context(|| format!("writing {}", output.display()))?;
    eprintln!(
        "rendered {}x{} px ({} items)",
        (viewport.w * scale).ceil(),
        (viewport.h * scale).ceil(),
        list.item_count()
    );
    Ok(())
}
