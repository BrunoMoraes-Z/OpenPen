//! "Import web page…": a URL is loaded in a headless Edge/Chrome by
//! `pen-import` and converted into one `.pen` frame, which is inserted into
//! the active document at a free spot (one undo step, with the agent flash).
//! Used by the dialog ([`dialog`]) and by the MCP tool `import_website`
//! (`agent::hub`). See docs/web-import.md.
//!
//! This module is the thin adapter between the app and `pen-import`: the
//! request/outcome types, the background job (a thread with its own Tokio
//! runtime, cancellable) and the pure helpers (viewport presets, progress,
//! error texts) that are tested without a window.

pub mod dialog;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::channel::{mpsc, oneshot};
use serde_json::Value;

use crate::agent::AgentInfo;
use crate::editor_state::EditorState;
use crate::scene_document::{SceneDocument, id_of};

/// Agent id and label of imports started from the dialog (the flash and the
/// cursor on the canvas).
pub const DIALOG_AGENT_ID: &str = "web-import";
pub const DIALOG_AGENT_NAME: &str = "Web import";
/// Teal, distinct from the agent palette.
pub const DIALOG_AGENT_COLOR: u32 = 0x0fa5a0;

/// pen-import can turn flex rows and columns into auto layout.
pub const INFER_LAYOUT_SUPPORTED: bool = true;

/// Undo label of the insert.
pub const UNDO_LABEL: &str = "Import web page";

/// Whole import (browser start, load, crawl, downloads) budget.
pub const IMPORT_TIMEOUT: Duration = Duration::from_secs(120);
/// Largest number of layers an import produces.
pub const MAX_NODES: usize = 5000;
/// Extra time after the load event for late content (fonts, animations).
pub const SETTLE_MS: u64 = 800;

// ------------------------------------------------------------------ presets

/// Viewport presets of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Desktop,
    Tablet,
    Mobile,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Desktop,
        Preset::Tablet,
        Preset::Mobile,
        Preset::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Desktop => "Desktop",
            Preset::Tablet => "Tablet",
            Preset::Mobile => "Mobile",
            Preset::Custom => "Custom",
        }
    }

    /// Viewport size of the preset (`None` for custom).
    pub fn size(self) -> Option<(u32, u32)> {
        match self {
            Preset::Desktop => Some((1440, 900)),
            Preset::Tablet => Some((768, 1024)),
            Preset::Mobile => Some((390, 844)),
            Preset::Custom => None,
        }
    }
}

/// Viewport of a preset; `custom` is the typed width for [`Preset::Custom`]
/// (its height is the desktop one).
pub fn viewport(preset: Preset, custom: &str) -> Result<(u32, u32), String> {
    if let Some(s) = preset.size() {
        return Ok(s);
    }
    let t = custom.trim().trim_end_matches("px").trim();
    let w: u32 = t
        .parse()
        .map_err(|_| "Enter the viewport width in pixels.".to_string())?;
    let (lo, hi) = (
        pen_mcp::tools::IMPORT_MIN_SIDE,
        pen_mcp::tools::IMPORT_MAX_SIDE,
    );
    if !(lo..=hi).contains(&w) {
        return Err(format!("The width must be between {lo} and {hi} px."));
    }
    Ok((w, 900))
}

// ------------------------------------------------------------------ request

/// One import, as the dialog or the MCP tool asked for it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRequest {
    /// Normalized URL ([`pen_mcp::normalize_web_url`]).
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub full_page: bool,
    pub selector: Option<String>,
    pub infer_layout: bool,
    /// Where images and fonts are written (next to the `.pen`).
    pub assets_dir: Option<PathBuf>,
    /// Return [`stub_outcome`] instead of running a browser (defaults to
    /// [`STUB_ENV`]).
    pub stub: bool,
}

impl ImportRequest {
    pub fn new(url: String) -> Self {
        Self {
            url,
            width: 1440,
            height: 900,
            full_page: true,
            selector: None,
            infer_layout: false,
            assets_dir: None,
            stub: stub_enabled(),
        }
    }

    /// From the MCP arguments (`url` already normalized).
    pub fn from_mcp(args: &pen_mcp::ImportWebsiteArgs, url: String) -> Self {
        let mut r = Self::new(url);
        r.width = args.width.unwrap_or(r.width);
        r.height = args.height.unwrap_or(r.height);
        r.full_page = args.full_page.unwrap_or(true);
        r.selector = args
            .selector
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        r
    }
}

/// The folder that receives a document's imported assets: the folder of the
/// `.pen` itself (pen-import files them under `images/<site>/` and
/// `fonts/<site>/` and writes the fills relative to it).
pub fn assets_dir_for(doc_path: &Path) -> Option<PathBuf> {
    doc_path.parent().map(Path::to_path_buf)
}

/// Why a document cannot receive an import yet.
pub const UNSAVED_DOC: &str = "Save the document first: the page's images and fonts are \
stored in files next to the .pen file.";

// ----------------------------------------------------------------- progress

/// Steps of an import, as reported by pen-import (plus the final insert).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Launching,
    Loading,
    Scrolling,
    Crawling,
    Assets { done: usize, total: usize },
    Converting,
    Inserting,
}

impl Stage {
    pub fn label(self) -> String {
        match self {
            Stage::Launching => "Starting the browser…".into(),
            Stage::Loading => "Loading the page…".into(),
            Stage::Scrolling => "Scrolling to load lazy content…".into(),
            Stage::Crawling => "Reading the page layout…".into(),
            Stage::Assets { done, total } if total > 0 => {
                format!("Downloading images and fonts ({done}/{total})…")
            }
            Stage::Assets { .. } => "Downloading images and fonts…".into(),
            Stage::Converting => "Converting to layers…".into(),
            Stage::Inserting => "Adding the frame to the canvas…".into(),
        }
    }

    /// Rough share of the whole import done when this stage starts or
    /// reports (assets advance inside their band).
    fn fraction(self) -> f32 {
        match self {
            Stage::Launching => 0.03,
            Stage::Loading => 0.10,
            Stage::Scrolling => 0.30,
            Stage::Crawling => 0.45,
            Stage::Assets { done, total } => {
                let t = if total == 0 {
                    0.0
                } else {
                    (done.min(total) as f32) / total as f32
                };
                0.55 + 0.30 * t
            }
            Stage::Converting => 0.88,
            Stage::Inserting => 0.96,
        }
    }
}

/// Progress shown by the dialog: the current stage and a bar that never
/// moves backwards (events can arrive out of order across threads).
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressState {
    pub stage: Stage,
    fraction: f32,
    pub started: Instant,
}

impl ProgressState {
    pub fn new(now: Instant) -> Self {
        Self {
            stage: Stage::Launching,
            fraction: Stage::Launching.fraction(),
            started: now,
        }
    }

    /// Applies a progress event; stale events (an earlier stage) are ignored.
    pub fn advance(&mut self, stage: Stage) {
        let f = stage.fraction();
        if f >= self.fraction {
            self.fraction = f;
            self.stage = stage;
        }
    }

    /// 0..=1.
    pub fn fraction(&self) -> f32 {
        self.fraction.clamp(0.0, 1.0)
    }

    pub fn label(&self) -> String {
        self.stage.label()
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started)
    }
}

// ------------------------------------------------------------------ outcome

/// How an import failed, with texts for people (dialog) and agents (MCP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    BrowserNotFound,
    Navigation(String),
    Timeout,
    Cancelled,
    /// The document cannot take the frame (unsaved, closed, busy, ...).
    Document(String),
    Other(String),
}

impl Failure {
    pub fn title(&self) -> &'static str {
        match self {
            Failure::BrowserNotFound => "No browser found",
            Failure::Navigation(_) => "The page could not be loaded",
            Failure::Timeout => "The page took too long",
            Failure::Cancelled => "Import cancelled",
            Failure::Document(_) => "The frame could not be added",
            Failure::Other(_) => "The import failed",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Failure::BrowserNotFound => "OpenPen imports pages with Microsoft Edge or Google \
                Chrome. Install one of them (or Chromium), then try again."
                .into(),
            Failure::Navigation(m) => format!("{m}. Check the address and your connection."),
            Failure::Timeout => format!(
                "The import did not finish within {} s. Try importing only the first \
                 screen (turn off \"Full page\") or a single element with a selector.",
                IMPORT_TIMEOUT.as_secs()
            ),
            Failure::Cancelled => "Nothing was added.".into(),
            Failure::Document(m) | Failure::Other(m) => m.clone(),
        }
    }

    /// One line for a tool result or the log.
    pub fn message(&self) -> String {
        format!("{}: {}", self.title(), self.detail())
    }
}

/// A converted page, ready to insert.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// The `.pen` frame (no ids).
    pub root: Value,
    pub title: String,
    pub warnings: Vec<String>,
    pub assets_written: Vec<PathBuf>,
    pub screenshot_png: Option<Vec<u8>>,
    /// `label: value` facts for the tool result.
    pub stats: Vec<(String, String)>,
}

impl Outcome {
    /// The frame to insert: named after the page title when the converter
    /// left the name empty.
    pub fn frame(&self) -> Value {
        let mut f = self.root.clone();
        if let Value::Object(m) = &mut f {
            let unnamed = m
                .get("name")
                .and_then(Value::as_str)
                .is_none_or(|s| s.trim().is_empty());
            if unnamed && !self.title.trim().is_empty() {
                m.insert("name".into(), Value::String(self.title.trim().to_string()));
            }
        }
        f
    }

    /// Width and height of the frame (0 when not numeric).
    pub fn size(&self) -> (f32, f32) {
        let n = |k: &str| self.root.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        (n("width"), n("height"))
    }
}

// ---------------------------------------------------------------- insertion

/// Inserts a converted page into a document at a free spot of the root (one
/// undo step through the script engine, ids generated). Returns the id of
/// the new frame and the engine response.
pub fn insert_into(
    doc: &mut SceneDocument,
    engine: &mut pen_script::ScriptEngine,
    agent_id: &str,
    frame: &Value,
) -> Result<(String, pen_script::ExecuteResponse), String> {
    let req = pen_script::insert_at_free_spot(agent_id, UNDO_LABEL, frame);
    let resp = doc.run_script(engine, &mut crate::agent::AppScriptHost, req);
    match pen_script::inserted_id(&resp) {
        Some(id) => Ok((id, resp)),
        None => Err(pen_script::place::insert_error(&resp)),
    }
}

/// [`insert_into`] on an open tab: refreshes the panels, flashes the frame
/// with `agent`'s ring, selects it and asks the canvas to zoom to it.
pub fn insert_page(
    state: &mut EditorState,
    engine: &mut pen_script::ScriptEngine,
    agent: &AgentInfo,
    frame: &Value,
    view: pen_render::Rect,
    cx: &mut gpui_kit::Context<EditorState>,
) -> Result<String, String> {
    let req = pen_script::insert_at_free_spot(agent.client_id.clone(), UNDO_LABEL, frame);
    let resp = state.run_agent_script(engine, agent, req, view, 0, cx);
    let Some(id) = pen_script::inserted_id(&resp) else {
        return Err(pen_script::place::insert_error(&resp));
    };
    if let Some(k) = state.doc.scene.find(&id) {
        state.set_selection(vec![id_of(k)], cx);
        state.fit_selection_requested = true;
    }
    // Image fills decode in the background.
    state.watch_assets(cx);
    cx.notify();
    Ok(id)
}

/// The dialog's pseudo-agent (flash color and cursor label).
pub fn dialog_agent() -> AgentInfo {
    AgentInfo {
        client_id: DIALOG_AGENT_ID.into(),
        name: DIALOG_AGENT_NAME.into(),
        color: DIALOG_AGENT_COLOR,
        internal: true,
    }
}

// ---------------------------------------------------------------------- job

/// Cancels an import when told to or when dropped (the browser is killed).
#[derive(Debug, Default)]
pub struct CancelGuard(Option<oneshot::Sender<()>>);

impl CancelGuard {
    /// Stops the import; its result resolves to [`Failure::Cancelled`].
    pub fn cancel(&mut self) {
        if let Some(c) = self.0.take() {
            let _ = c.send(());
        }
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A running import: progress events, the result, and cancellation.
/// Dropping the job (or just its `cancel` guard) cancels the import.
pub struct ImportJob {
    pub progress: mpsc::UnboundedReceiver<Stage>,
    pub result: oneshot::Receiver<Result<Outcome, Failure>>,
    pub cancel: CancelGuard,
}

/// Starts an import on a background thread with its own Tokio runtime.
pub fn spawn(req: ImportRequest) -> ImportJob {
    let (ptx, prx) = mpsc::unbounded();
    let (rtx, rrx) = oneshot::channel();
    let (ctx, crx) = oneshot::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("openpen-web-import".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = rtx.send(Err(Failure::Other(format!("cannot start: {e}"))));
                    return;
                }
            };
            let r = rt.block_on(async move {
                let job = std::pin::pin!(run(req, ptx));
                // The cancel side is polled first: a job cancelled before it
                // started never launches the browser.
                match futures::future::select(crx, job).await {
                    futures::future::Either::Right((r, _)) => r,
                    // Cancelled (or the job was dropped): the import future
                    // is dropped here, which kills the browser.
                    futures::future::Either::Left(_) => Err(Failure::Cancelled),
                }
            });
            // Let child-process cleanup run before the runtime goes.
            rt.shutdown_timeout(Duration::from_secs(3));
            let _ = rtx.send(r);
        });
    if let Err(e) = spawned {
        let (rtx, rrx) = oneshot::channel();
        let _ = rtx.send(Err(Failure::Other(format!("cannot start the import: {e}"))));
        let (_, prx) = mpsc::unbounded();
        return ImportJob {
            progress: prx,
            result: rrx,
            cancel: CancelGuard(None),
        };
    }
    ImportJob {
        progress: prx,
        result: rrx,
        cancel: CancelGuard(Some(ctx)),
    }
}

/// Runs an import to completion on the current Tokio runtime.
pub async fn run(
    req: ImportRequest,
    progress: mpsc::UnboundedSender<Stage>,
) -> Result<Outcome, Failure> {
    backend::import(req, move |s| {
        let _ = progress.unbounded_send(s);
    })
    .await
}

/// Runs an import from synchronous code (headless backends, tests).
pub fn run_blocking(req: ImportRequest) -> Result<Outcome, Failure> {
    let mut job = spawn(req);
    let r = futures::executor::block_on(&mut job.result);
    r.unwrap_or(Err(Failure::Cancelled))
}

/// The importer: pen-import, or a fixed frame with
/// `OPENPEN_WEB_IMPORT_STUB=1` (see [`stub_outcome`]).
mod backend {
    use super::*;

    pub async fn import(
        req: ImportRequest,
        progress: impl Fn(Stage) + Send + Sync + 'static,
    ) -> Result<Outcome, Failure> {
        if req.stub {
            for s in [
                Stage::Launching,
                Stage::Loading,
                Stage::Crawling,
                Stage::Assets { done: 1, total: 1 },
                Stage::Converting,
            ] {
                progress(s);
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            return Ok(stub_outcome(&req));
        }
        let r = pen_import::import_url(options(&req), move |p| progress(stage_of(p))).await;
        r.map(outcome_of).map_err(failure_of)
    }
}

/// pen-import options for a request.
pub fn options(req: &ImportRequest) -> pen_import::ImportOptions {
    pen_import::ImportOptions {
        url: req.url.clone(),
        viewport_width: req.width,
        viewport_height: req.height,
        full_page: req.full_page,
        selector: req.selector.clone(),
        wait_ms: SETTLE_MS,
        browser: None,
        assets_dir: req.assets_dir.clone(),
        timeout: IMPORT_TIMEOUT,
        max_nodes: MAX_NODES,
        infer_layout: req.infer_layout,
        ..pen_import::ImportOptions::default()
    }
}

pub fn stage_of(p: pen_import::ImportProgress) -> Stage {
    use pen_import::ImportProgress as P;
    match p {
        P::Launching => Stage::Launching,
        P::Loading => Stage::Loading,
        P::Scrolling => Stage::Scrolling,
        P::Crawling => Stage::Crawling,
        P::Assets { done, total } => Stage::Assets { done, total },
        P::Converting => Stage::Converting,
    }
}

pub fn failure_of(e: pen_import::ImportError) -> Failure {
    use pen_import::ImportError as E;
    match e {
        E::BrowserNotFound => Failure::BrowserNotFound,
        E::Navigation(m) => Failure::Navigation(m),
        E::Timeout => Failure::Timeout,
        E::Crawl(m) => Failure::Other(format!("Reading the page failed: {m}")),
        E::Other(m) => Failure::Other(m),
    }
}

pub fn outcome_of(r: pen_import::ImportResult) -> Outcome {
    let s = &r.stats;
    let mut stats = vec![
        ("layers".to_string(), s.nodes.to_string()),
        ("texts".to_string(), s.texts.to_string()),
        ("images".to_string(), s.images.to_string()),
        ("vectors".to_string(), s.svgs.to_string()),
        ("page elements read".to_string(), s.dom_elements.to_string()),
        (
            "browser time".to_string(),
            format!("{:.1} s", s.total_ms as f64 / 1000.0),
        ),
    ];
    if !r.fonts.is_empty() {
        stats.push((
            "web fonts downloaded".to_string(),
            r.fonts.len().to_string(),
        ));
    }
    Outcome {
        root: r.root,
        title: r.title,
        warnings: r.warnings,
        assets_written: r.assets_written,
        screenshot_png: r.screenshot_png,
        stats,
    }
}

/// Environment variable that makes imports return [`stub_outcome`] without
/// a browser (UI tests and demos).
pub const STUB_ENV: &str = "OPENPEN_WEB_IMPORT_STUB";

fn stub_enabled() -> bool {
    std::env::var(STUB_ENV).is_ok_and(|v| v.trim() == "1")
}

/// A fixed page: a frame with a header bar, a heading, a paragraph and a
/// button, sized to the request's viewport.
pub fn stub_outcome(req: &ImportRequest) -> Outcome {
    let w = req.width as f64;
    let root = serde_json::json!({
        "type": "frame",
        "name": "Example Domain",
        "width": w,
        "height": 640,
        "fill": "#f0f0f2",
        "clip": true,
        "children": [
            {"type": "rectangle", "name": "header", "x": 0, "y": 0, "width": w, "height": 64,
             "fill": "#ffffff"},
            {"type": "text", "name": "h1", "x": 48, "y": 120, "content": "Example Domain",
             "fontFamily": "Inter", "fontSize": 32, "fontWeight": "700", "fill": "#111111"},
            {"type": "text", "name": "p", "x": 48, "y": 180, "width": (w - 96.0).max(100.0),
             "textGrowth": "fixed-width",
             "content": "This domain is for use in illustrative examples in documents.",
             "fontFamily": "Inter", "fontSize": 16, "fill": "#333333"},
            {"type": "frame", "name": "a", "x": 48, "y": 240, "width": 160, "height": 44,
             "cornerRadius": 8, "fill": "#38488f",
             "children": [
                {"type": "text", "name": "More information", "x": 16, "y": 12,
                 "content": "More information", "fontFamily": "Inter", "fontSize": 14,
                 "fill": "#ffffff"}
             ]}
        ]
    });
    Outcome {
        root,
        title: "Example Domain".into(),
        warnings: vec!["Stub import: no browser was used.".into()],
        assets_written: Vec::new(),
        screenshot_png: None,
        stats: vec![("nodes".into(), "6".into())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_custom_width() {
        assert_eq!(viewport(Preset::Desktop, "").unwrap(), (1440, 900));
        assert_eq!(viewport(Preset::Tablet, "x").unwrap(), (768, 1024));
        assert_eq!(viewport(Preset::Mobile, "").unwrap(), (390, 844));
        assert_eq!(viewport(Preset::Custom, " 1280 ").unwrap(), (1280, 900));
        assert_eq!(viewport(Preset::Custom, "1024px").unwrap(), (1024, 900));
        assert!(viewport(Preset::Custom, "").is_err());
        assert!(viewport(Preset::Custom, "wide").is_err());
        assert!(viewport(Preset::Custom, "50").is_err());
        assert!(viewport(Preset::Custom, "99999").is_err());
        assert_eq!(Preset::ALL.map(Preset::label).len(), 4);
    }

    #[test]
    fn progress_is_monotonic_and_labeled() {
        let t0 = Instant::now();
        let mut p = ProgressState::new(t0);
        assert_eq!(p.stage, Stage::Launching);
        let mut last = p.fraction();
        for s in [
            Stage::Loading,
            Stage::Scrolling,
            Stage::Crawling,
            Stage::Assets { done: 0, total: 4 },
            Stage::Assets { done: 2, total: 4 },
            Stage::Assets { done: 4, total: 4 },
            Stage::Converting,
            Stage::Inserting,
        ] {
            p.advance(s);
            assert_eq!(p.stage, s);
            assert!(p.fraction() >= last, "{s:?}");
            last = p.fraction();
        }
        // A late event from an earlier stage does not move the bar back.
        p.advance(Stage::Loading);
        assert_eq!(p.stage, Stage::Inserting);
        assert!(p.fraction() < 1.0);
        assert!(p.label().contains("canvas"));
        assert!(Stage::Assets { done: 2, total: 5 }.label().contains("2/5"));
        assert!(!Stage::Assets { done: 0, total: 0 }.label().contains('/'));
        // Bogus counts stay inside the band.
        let mut q = ProgressState::new(t0);
        q.advance(Stage::Assets { done: 9, total: 3 });
        assert!(q.fraction() <= Stage::Converting.fraction());
    }

    #[test]
    fn failures_have_hints() {
        assert!(Failure::BrowserNotFound.detail().contains("Edge"));
        assert!(Failure::Timeout.detail().contains("Full page"));
        assert!(
            Failure::Navigation("net::ERR_NAME_NOT_RESOLVED".into())
                .message()
                .contains("ERR_NAME_NOT_RESOLVED")
        );
    }

    #[test]
    fn mcp_args_map_to_a_request() {
        let a = pen_mcp::ImportWebsiteArgs {
            url: "example.com".into(),
            width: Some(390),
            full_page: Some(false),
            selector: Some("  main ".into()),
            ..Default::default()
        };
        let r = ImportRequest::from_mcp(&a, "https://example.com/".into());
        assert_eq!((r.width, r.height, r.full_page), (390, 900, false));
        assert_eq!(r.selector.as_deref(), Some("main"));
        let d = ImportRequest::from_mcp(&pen_mcp::ImportWebsiteArgs::default(), "u".into());
        assert_eq!((d.width, d.full_page, d.selector), (1440, true, None));
    }

    #[test]
    fn frame_takes_the_title_when_unnamed() {
        let mut o = stub_outcome(&ImportRequest::new("https://example.com/".into()));
        assert_eq!(o.frame()["name"], "Example Domain");
        o.root.as_object_mut().unwrap().remove("name");
        o.title = "  Hello ".into();
        assert_eq!(o.frame()["name"], "Hello");
        assert_eq!(o.size(), (1440.0, 640.0));
    }

    #[test]
    fn cancelling_a_job_resolves_cancelled() {
        // Cancelled before the thread polls it: no browser is launched.
        let mut job = spawn(ImportRequest::new("https://example.invalid/".into()));
        job.cancel.cancel();
        let r = futures::executor::block_on(&mut job.result).unwrap();
        assert_eq!(r.unwrap_err(), Failure::Cancelled);
    }
}
