//! `import_website` for the headless MCP server: runs pen-import on a
//! background thread with its own Tokio runtime (the stdio server's runtime
//! is single-threaded and the browser launch blocks), cancellable by
//! dropping the future.
//!
//! `OPENPEN_WEB_IMPORT_STUB=1` returns a fixed frame without a browser (the
//! same switch as the app; used by the tests).

use std::path::PathBuf;
use std::time::Duration;

use pen_mcp::ImportWebsiteArgs;
use serde_json::{Value, json};

/// Environment switch for the browserless stub.
pub const STUB_ENV: &str = "OPENPEN_WEB_IMPORT_STUB";

/// Whole import budget.
pub const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);

/// A converted page.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub root: Value,
    pub title: String,
    pub warnings: Vec<String>,
    pub assets: usize,
    pub screenshot: Option<Vec<u8>>,
    pub stats: Vec<(String, String)>,
}

impl Page {
    /// The frame to insert, named after the page when it has no name.
    pub fn frame(&self) -> Value {
        let mut f = self.root.clone();
        if let Value::Object(m) = &mut f {
            let unnamed = m
                .get("name")
                .and_then(Value::as_str)
                .is_none_or(|s| s.trim().is_empty());
            if unnamed && !self.title.trim().is_empty() {
                m.insert("name".into(), Value::String(self.title.trim().into()));
            }
        }
        f
    }

    pub fn size(&self) -> (f32, f32) {
        let n = |k: &str| self.root.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        (n("width"), n("height"))
    }
}

/// Imports `url` (normalized) with the tool's options; assets go to
/// `assets_dir` (the document's folder).
pub async fn import(
    args: &ImportWebsiteArgs,
    url: String,
    assets_dir: PathBuf,
) -> Result<Page, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let args = args.clone();
    std::thread::Builder::new()
        .name("openpen-web-import".into())
        .spawn(move || {
            let r = if stub_enabled() {
                std::thread::sleep(Duration::from_millis(50));
                Ok(stub(&args, &url))
            } else {
                run_real(&args, url, assets_dir, cancel_rx)
            };
            let _ = tx.send(r);
        })
        .map_err(|e| format!("cannot start the import: {e}"))?;
    // Dropping `cancel_tx` (this future dropped) stops the import.
    let _guard = cancel_tx;
    rx.await
        .unwrap_or_else(|_| Err("the import stopped unexpectedly".into()))
}

fn stub_enabled() -> bool {
    std::env::var(STUB_ENV).is_ok_and(|v| v.trim() == "1")
}

/// The browserless stand-in: a small page sized to the viewport.
fn stub(args: &ImportWebsiteArgs, url: &str) -> Page {
    let w = args.width.unwrap_or(1440) as f64;
    Page {
        root: json!({
            "type": "frame", "name": "Example Domain", "width": w, "height": 640,
            "fill": "#f0f0f2", "clip": true,
            "children": [
                {"type": "text", "name": "h1", "x": 48, "y": 120, "content": "Example Domain",
                 "fontFamily": "Inter", "fontSize": 32, "fill": "#111111"},
                {"type": "rectangle", "name": "a", "x": 48, "y": 200, "width": 160,
                 "height": 44, "cornerRadius": 8, "fill": "#38488f"}
            ]
        }),
        title: "Example Domain".into(),
        warnings: vec![format!("Stub import of {url}: no browser was used.")],
        assets: 0,
        screenshot: None,
        stats: vec![("nodes".into(), "3".into())],
    }
}

fn run_real(
    args: &ImportWebsiteArgs,
    url: String,
    assets_dir: PathBuf,
    cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<Page, String> {
    let opts = pen_import::ImportOptions {
        url,
        viewport_width: args.width.unwrap_or(1440),
        viewport_height: args.height.unwrap_or(900),
        full_page: args.full_page.unwrap_or(true),
        selector: args
            .selector
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        wait_ms: 800,
        assets_dir: Some(assets_dir),
        timeout: IMPORT_TIMEOUT,
        ..pen_import::ImportOptions::default()
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the import: {e}"))?;
    let r = rt.block_on(async move {
        tokio::select! {
            r = pen_import::import_url(opts, |_| {}) => Some(r),
            // The tool call was dropped: dropping the import kills the browser.
            _ = cancel => None,
        }
    });
    rt.shutdown_timeout(Duration::from_secs(3));
    let r = r.ok_or_else(|| "the import was cancelled".to_string())?;
    let r = r.map_err(|e| {
        use pen_import::ImportError as E;
        match e {
            E::BrowserNotFound => "No browser found: OpenPen imports pages with Microsoft Edge                 or Google Chrome. Install one of them (or Chromium) and try again."
                .to_string(),
            E::Navigation(m) => format!("The page could not be loaded: {m}"),
            E::Timeout => format!(
                "The import did not finish within {} s; try `fullPage: false` or a `selector`.",
                IMPORT_TIMEOUT.as_secs()
            ),
            other => format!("The import failed: {other}"),
        }
    })?;
    let s = &r.stats;
    let stats = vec![
        ("layers".to_string(), s.nodes.to_string()),
        ("texts".to_string(), s.texts.to_string()),
        ("images".to_string(), s.images.to_string()),
        ("vectors".to_string(), s.svgs.to_string()),
        ("page elements read".to_string(), s.dom_elements.to_string()),
    ];
    Ok(Page {
        root: r.root,
        title: r.title,
        warnings: r.warnings,
        assets: r.assets_written.len(),
        screenshot: r.screenshot_png,
        stats,
    })
}
