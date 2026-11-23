//! `openpen-cli import-url`: imports a web page (pen-import) into a new `.pen`,
//! with its images and fonts saved next to it.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::{Value, json};

#[derive(Args)]
pub struct ImportUrlArgs {
    /// Page to import (`https://...`; `http://` and `file://` work too).
    url: String,
    /// The `.pen` to create; assets go to `images/<site>/` and `fonts/<site>/` beside it.
    #[arg(short, long)]
    output: PathBuf,
    /// Viewport width in CSS px.
    #[arg(long, default_value_t = 1440)]
    width: u32,
    /// Viewport height in CSS px.
    #[arg(long, default_value_t = 900)]
    height: u32,
    /// Import only what is visible in the viewport (default: the whole page).
    #[arg(long)]
    viewport_only: bool,
    /// Import only the first element matching this CSS selector.
    #[arg(long)]
    selector: Option<String>,
    /// Turn simple flex rows/columns into auto layout.
    #[arg(long)]
    infer_layout: bool,
    /// Chrome/Edge/Chromium executable (default: autodetect).
    #[arg(long)]
    browser: Option<PathBuf>,
    /// Extra settle time after the page loads, in ms.
    #[arg(long, default_value_t = 1500)]
    wait: u64,
    /// Overall time limit, in seconds.
    #[arg(long, default_value_t = 90)]
    timeout: u64,
}

pub fn run(a: &ImportUrlArgs) -> Result<()> {
    if a.output.extension().and_then(|e| e.to_str()) != Some("pen") {
        bail!("the output must be a .pen file: {}", a.output.display());
    }
    let dir = std::path::absolute(&a.output)?
        .parent()
        .map(PathBuf::from)
        .context("output has no folder")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let opts = pen_import::ImportOptions {
        url: a.url.clone(),
        viewport_width: a.width,
        viewport_height: a.height,
        full_page: !a.viewport_only,
        selector: a.selector.clone(),
        wait_ms: a.wait,
        browser: a.browser.clone(),
        assets_dir: Some(dir),
        timeout: Duration::from_secs(a.timeout.max(5)),
        infer_layout: a.infer_layout,
        ..Default::default()
    };
    let res = pen_import::import_url_blocking(opts, |p| eprintln!("import: {p:?}"))?;
    let mut root = res.root;
    let mut ids = pen_schema::IdGenerator::new();
    assign_ids(&mut root, &mut ids);
    let mut doc = json!({ "version": pen_schema::CURRENT_VERSION, "children": [root] });
    if !res.fonts.is_empty() {
        doc["fonts"] = Value::Array(res.fonts);
    }
    // Round-trip through pen-schema: validates and writes the canonical form.
    let bytes = serde_json::to_vec(&doc)?;
    let (document, _) =
        pen_schema::load_document(&bytes).context("the imported frame is invalid")?;
    std::fs::write(&a.output, pen_schema::save_document(&document))
        .with_context(|| format!("writing {}", a.output.display()))?;
    for w in &res.warnings {
        eprintln!("warning: {w}");
    }
    let s = &res.stats;
    eprintln!(
        "imported \"{}\": {} nodes ({} texts, {} images, {} vectors) from {} elements, {} asset file(s), {} ms",
        res.title,
        s.nodes,
        s.texts,
        s.images,
        s.svgs,
        s.dom_elements,
        res.assets_written.len(),
        s.total_ms
    );
    Ok(())
}

fn assign_ids(v: &mut Value, ids: &mut pen_schema::IdGenerator) {
    if let Value::Object(m) = v {
        m.insert("id".into(), Value::String(ids.next_id()));
        if let Some(Value::Array(kids)) = m.get_mut("children") {
            for k in kids {
                assign_ids(k, ids);
            }
        }
    }
}
