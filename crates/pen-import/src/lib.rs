//! Web page import ("browser import"): loads a URL in a headless Chromium-based
//! browser (Edge, Chrome or Chromium) driven through the DevTools protocol,
//! crawls the rendered DOM with our own script (`crawl.js`) and converts the
//! snapshot into one editable `.pen` frame.
//!
//! Privacy: the browser always runs with a fresh temporary profile that is
//! deleted afterwards; the user's cookies and sessions are never used, and
//! nothing is sent anywhere except the requests the page itself makes.
//!
//! No GPUI here: the app and the CLI call [`import_url`] / [`import_url_blocking`].

mod assets;
mod browser;
mod cdp;
mod convert;
mod css;
mod infer;
mod snapshot;
mod svg;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

pub use browser::find_browser;

use crate::assets::{AssetWriter, Kind, Sniff, sniff};
use crate::cdp::{Cdp, Page};
use crate::convert::{AssetRefs, ConvertOptions};
use crate::snapshot::Snapshot;

const CRAWL_JS: &str = include_str!("crawl.js");

/// What and how to import.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub url: String,
    /// Default 1440.
    pub viewport_width: u32,
    /// Default 900.
    pub viewport_height: u32,
    /// Whole scrollable page (default) or the viewport only.
    pub full_page: bool,
    /// Import only the first element matching this CSS selector.
    pub selector: Option<String>,
    /// Extra settle time after load (default 1500 ms).
    pub wait_ms: u64,
    /// Explicit Chrome/Edge executable; default: [`find_browser`].
    pub browser: Option<PathBuf>,
    /// The `.pen`'s folder: images and fonts go to `<dir>/images/<site>/` and
    /// `<dir>/fonts/<site>/`. `None`: nothing is written and images become placeholders.
    pub assets_dir: Option<PathBuf>,
    /// Overall time limit (default 90 s).
    pub timeout: Duration,
    /// Maximum number of DOM elements crawled (default 5000).
    pub max_nodes: usize,
    /// Rebuild simple flex rows/columns as auto layout (default false).
    pub infer_layout: bool,
    /// Also capture a screenshot of exactly the imported region (`region_png`).
    pub capture_region: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            url: String::new(),
            viewport_width: 1440,
            viewport_height: 900,
            full_page: true,
            selector: None,
            wait_ms: 1500,
            browser: None,
            assets_dir: None,
            timeout: Duration::from_secs(90),
            max_nodes: 5000,
            infer_layout: false,
            capture_region: false,
        }
    }
}

impl ImportOptions {
    pub fn new(url: impl Into<String>) -> Self {
        ImportOptions {
            url: url.into(),
            ..Default::default()
        }
    }
}

/// Progress notifications, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportProgress {
    Launching,
    Loading,
    Scrolling,
    Crawling,
    Assets { done: usize, total: usize },
    Converting,
}

#[derive(Debug, Clone, Default)]
pub struct ImportStats {
    /// `.pen` nodes produced.
    pub nodes: usize,
    pub texts: usize,
    pub images: usize,
    pub svgs: usize,
    /// DOM elements crawled.
    pub dom_elements: usize,
    pub launch_ms: u64,
    pub load_ms: u64,
    pub scroll_ms: u64,
    pub crawl_ms: u64,
    pub assets_ms: u64,
    pub convert_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ImportResult {
    /// One `.pen` frame node (schema 2.22 JSON) holding the page, without ids.
    /// Image fills reference files relative to `assets_dir`.
    pub root: Value,
    pub title: String,
    pub warnings: Vec<String>,
    pub assets_written: Vec<PathBuf>,
    /// Screenshot of the viewport (top of the page), for reference and agents.
    pub screenshot_png: Option<Vec<u8>>,
    /// Screenshot of the whole imported region (only with `capture_region`).
    pub region_png: Option<Vec<u8>>,
    /// `.pen` font definitions (`{name, url, weight, style}`) for downloaded web fonts,
    /// to merge into the document's `fonts`.
    pub fonts: Vec<Value>,
    pub stats: ImportStats,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("no Chrome, Edge or Chromium installation was found")]
    BrowserNotFound,
    #[error("navigation failed: {0}")]
    Navigation(String),
    #[error("the import timed out")]
    Timeout,
    #[error("page crawl failed: {0}")]
    Crawl(String),
    #[error("{0}")]
    Other(String),
}

/// Imports `opts.url`. The browser is killed and its profile deleted on every exit path
/// (success, error, timeout, or the future being dropped).
pub async fn import_url(
    opts: ImportOptions,
    progress: impl Fn(ImportProgress) + Send + Sync + 'static,
) -> Result<ImportResult, ImportError> {
    let exe = match &opts.browser {
        Some(p) => p.clone(),
        None => find_browser().ok_or(ImportError::BrowserNotFound)?,
    };
    let timeout = opts.timeout;
    match tokio::time::timeout(timeout, run(opts, exe, &progress)).await {
        Ok(r) => r,
        Err(_) => Err(ImportError::Timeout),
    }
}

/// Blocking variant with its own runtime (on a separate thread, so it is safe to
/// call from inside another runtime too).
pub fn import_url_blocking(
    opts: ImportOptions,
    progress: impl Fn(ImportProgress) + Send + Sync + 'static,
) -> Result<ImportResult, ImportError> {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| ImportError::Other(format!("cannot start a runtime: {e}")))?;
        rt.block_on(import_url(opts, progress))
    })
    .join()
    .map_err(|_| ImportError::Other("the import thread panicked".into()))?
}

fn ms(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

fn other(e: String) -> ImportError {
    ImportError::Other(e)
}

async fn run(
    opts: ImportOptions,
    exe: PathBuf,
    progress: &(impl Fn(ImportProgress) + Send + Sync),
) -> Result<ImportResult, ImportError> {
    let t_all = Instant::now();
    let mut stats = ImportStats::default();
    let mut warnings: Vec<String> = Vec::new();
    let deadline = Instant::now() + opts.timeout;

    // --- launch ---------------------------------------------------------
    progress(ImportProgress::Launching);
    let t = Instant::now();
    let (w, h) = (opts.viewport_width.max(200), opts.viewport_height.max(200));
    let launch_deadline = Instant::now() + Duration::from_secs(30).min(opts.timeout);
    let mut browser =
        tokio::task::spawn_blocking(move || browser::Browser::launch(&exe, w, h, launch_deadline))
            .await
            .map_err(|e| other(e.to_string()))?
            .map_err(other)?;
    let result = async {
        let cdp = Cdp::connect(&browser.ws_url).await.map_err(other)?;
        let target = cdp
            .call(None, "Target.createTarget", json!({ "url": "about:blank" }))
            .await
            .map_err(other)?;
        let target_id = target["targetId"].as_str().unwrap_or_default().to_string();
        let attached = cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .await
            .map_err(other)?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or_else(|| other("cannot attach to the page".into()))?
            .to_string();
        let page = Page { cdp, session };
        stats.launch_ms = ms(t);
        let r = import_in_page(&page, &opts, progress, &mut stats, &mut warnings, deadline).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            page.cdp.call(None, "Browser.close", json!({})),
        )
        .await;
        r
    }
    .await;
    // Kill and clean up off the async threads.
    let _ = tokio::task::spawn_blocking(move || browser.shutdown()).await;
    let mut result = result?;
    stats.total_ms = ms(t_all);
    result.stats = ImportStats {
        nodes: result.stats.nodes,
        texts: result.stats.texts,
        images: result.stats.images,
        svgs: result.stats.svgs,
        dom_elements: result.stats.dom_elements,
        ..stats
    };
    Ok(result)
}

async fn import_in_page(
    page: &Page,
    opts: &ImportOptions,
    progress: &(impl Fn(ImportProgress) + Send + Sync),
    stats: &mut ImportStats,
    warnings: &mut Vec<String>,
    deadline: Instant,
) -> Result<ImportResult, ImportError> {
    let remaining = || deadline.saturating_duration_since(Instant::now());
    // --- load -----------------------------------------------------------
    progress(ImportProgress::Loading);
    let t = Instant::now();
    let (w, h) = (opts.viewport_width.max(200), opts.viewport_height.max(200));
    page.call("Page.enable", json!({})).await.map_err(other)?;
    page.call(
        "Emulation.setDeviceMetricsOverride",
        json!({ "width": w, "height": h, "deviceScaleFactor": 1, "mobile": false }),
    )
    .await
    .map_err(other)?;
    let _ = page
        .call("Emulation.setScrollbarsHidden", json!({ "hidden": true }))
        .await;
    let nav = page
        .call("Page.navigate", json!({ "url": opts.url }))
        .await
        .map_err(ImportError::Navigation)?;
    if let Some(err) = nav.get("errorText").and_then(Value::as_str)
        && !err.is_empty()
    {
        return Err(ImportError::Navigation(err.to_string()));
    }
    let frame_id = nav["frameId"].as_str().unwrap_or_default().to_string();
    // Wait for the load event (bounded), then for the network to go quiet.
    let load_limit = Instant::now() + Duration::from_secs(30).min(remaining());
    loop {
        let state = page
            .eval("document.readyState")
            .await
            .unwrap_or(Value::Null);
        if state == "complete" {
            break;
        }
        if Instant::now() >= load_limit {
            warnings.push("The page did not finish loading in time; imported as it was.".into());
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    page.eval(CRAWL_JS)
        .await
        .map_err(|e| ImportError::Crawl(format!("cannot install the crawler: {e}")))?;
    let settle_ms = 8000u64.min(remaining().as_millis() as u64 / 4);
    page.eval(&format!("window.__openpen.settle(500, {settle_ms})"))
        .await
        .map_err(ImportError::Crawl)?;
    stats.load_ms = ms(t);

    // --- lazy content -------------------------------------------------------
    let t = Instant::now();
    if opts.full_page || opts.selector.is_some() {
        progress(ImportProgress::Scrolling);
        let budget = 10_000u64.min(remaining().as_millis() as u64 / 4);
        page.eval(&format!("window.__openpen.prescroll({budget})"))
            .await
            .map_err(ImportError::Crawl)?;
        page.eval(&format!("window.__openpen.settle(500, {settle_ms})"))
            .await
            .map_err(ImportError::Crawl)?;
    }
    if opts.wait_ms > 0 {
        tokio::time::sleep(Duration::from_millis(opts.wait_ms).min(remaining() / 4)).await;
    }
    // Freeze: finished/paused animations, then the global clock stopped.
    page.eval("window.__openpen.freeze()")
        .await
        .map_err(ImportError::Crawl)?;
    if page.call("Animation.enable", json!({})).await.is_ok() {
        let _ = page
            .call("Animation.setPlaybackRate", json!({ "playbackRate": 0 }))
            .await;
    }
    page.eval("new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))")
        .await
        .map_err(ImportError::Crawl)?;
    stats.scroll_ms = ms(t);

    // --- screenshots and crawl ------------------------------------------------
    progress(ImportProgress::Crawling);
    let t = Instant::now();
    let screenshot_png = capture(page, None).await.ok();
    let crawl_opts = json!({
        "selector": opts.selector,
        "maxNodes": opts.max_nodes.max(1),
        "fullPage": opts.full_page,
    });
    // Taken before `prepare` touches the DOM, so it shows the untouched page.
    let region_png = if opts.capture_region {
        let region: Option<[f64; 4]> = page
            .eval_str(&format!("window.__openpen.regionOf({crawl_opts})"))
            .await
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .flatten();
        match region {
            Some(mut r) => {
                r[3] = r[3].min(MAX_HEIGHT);
                capture(page, Some(r)).await.ok()
            }
            None => None,
        }
    } else {
        None
    };
    let sel = opts
        .selector
        .as_ref()
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_else(|| "null".into());
    page.eval(&format!("window.__openpen.prepare({sel})"))
        .await
        .map_err(ImportError::Crawl)?;
    let raw = page
        .eval_str(&format!("window.__openpen.crawl({crawl_opts})"))
        .await
        .map_err(ImportError::Crawl)?;
    let mut snap: Snapshot =
        serde_json::from_str(&raw).map_err(|e| ImportError::Crawl(format!("bad snapshot: {e}")))?;
    if let Some(e) = snap.error.take() {
        return Err(ImportError::Crawl(e));
    }
    if snap.region[3] > MAX_HEIGHT {
        warnings.push(format!(
            "The page is {} px tall; only the first {MAX_HEIGHT} px were imported.",
            snap.region[3].round()
        ));
        snap.region[3] = MAX_HEIGHT;
    }
    warnings.append(&mut snap.warnings);
    stats.dom_elements = snap.nodes;
    // Real font family per used stack, as the browser resolved it.
    let fonts = platform_fonts(page, snap.fonts.len()).await;
    stats.crawl_ms = ms(t);

    // --- assets -------------------------------------------------------------
    let t = Instant::now();
    let mut writer = AssetWriter::new(opts.assets_dir.clone(), &snap.url);
    let mut refs = AssetRefs::default();
    let mut font_defs = Vec::new();
    let mut font_alias: HashMap<String, String> = HashMap::new();
    if writer.enabled() {
        collect_assets(
            page,
            &frame_id,
            &snap,
            &mut writer,
            &mut refs,
            &mut font_defs,
            &mut font_alias,
            warnings,
            progress,
        )
        .await;
    } else {
        warnings.push("No assets folder: images were imported as placeholders.".into());
    }
    stats.assets_ms = ms(t);

    // --- convert ------------------------------------------------------------
    progress(ImportProgress::Converting);
    let t = Instant::now();
    let fonts: Vec<Option<String>> = fonts
        .into_iter()
        .enumerate()
        .map(|(i, platform)| {
            // A downloaded web font keeps the name used by its font definition.
            let css_first = snap
                .fonts
                .get(i)
                .and_then(|f| f.as_ref())
                .map(|f| first_family(&f.family));
            if let Some(css) = css_first
                && let Some(alias) = font_alias.get(&css.to_ascii_lowercase())
            {
                return Some(alias.clone());
            }
            platform
        })
        .collect();
    let conv_opts = ConvertOptions {
        infer_layout: opts.infer_layout,
        clip_to_region: !opts.full_page && opts.selector.is_none(),
    };
    let converted = if writer.enabled() {
        let mut store = |bytes: &[u8], ext: &str| writer.store(bytes, Kind::Images, ext).ok();
        convert::convert(&snap, &fonts, &refs, Some(&mut store), &conv_opts)
    } else {
        convert::convert(&snap, &fonts, &refs, None, &conv_opts)
    };
    warnings.extend(converted.warnings);
    stats.convert_ms = ms(t);
    stats.nodes = converted.counts.nodes;
    stats.texts = converted.counts.texts;
    stats.images = converted.counts.images;
    stats.svgs = converted.counts.svgs;

    Ok(ImportResult {
        root: converted.root,
        title: snap.title.clone(),
        warnings: std::mem::take(warnings),
        assets_written: std::mem::take(&mut writer.written),
        screenshot_png,
        region_png,
        fonts: font_defs,
        stats: stats.clone(),
    })
}

/// Longest page imported (Chromium cannot capture taller surfaces reliably).
const MAX_HEIGHT: f64 = 16_000.0;

fn first_family(stack: &str) -> String {
    css::split_top(stack, ',')
        .into_iter()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(['"', '\''])
        .to_string()
}

/// PNG of the viewport (`None`) or of a page-coordinate region.
async fn capture(page: &Page, region: Option<[f64; 4]>) -> Result<Vec<u8>, String> {
    let params = match region {
        None => json!({ "format": "png" }),
        Some([x, y, w, h]) => json!({
            "format": "png",
            "captureBeyondViewport": true,
            "clip": { "x": x, "y": y, "width": w.max(1.0), "height": h.max(1.0), "scale": 1 },
        }),
    };
    let r = page.call("Page.captureScreenshot", params).await?;
    let data = r["data"].as_str().ok_or("no screenshot data")?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| e.to_string())
}

async fn platform_fonts(page: &Page, count: usize) -> Vec<Option<String>> {
    let mut out = vec![None; count];
    if count == 0 {
        return out;
    }
    if page.call("DOM.enable", json!({})).await.is_err()
        || page.call("CSS.enable", json!({})).await.is_err()
        || page
            .call("DOM.getDocument", json!({ "depth": 0 }))
            .await
            .is_err()
    {
        return out;
    }
    for (i, slot) in out.iter_mut().enumerate() {
        let r = page
            .call(
                "Runtime.evaluate",
                json!({ "expression": format!("window.__openpenFontProbes[{i}]") }),
            )
            .await;
        let Some(object_id) = r.ok().and_then(|v| {
            v.pointer("/result/objectId")
                .and_then(Value::as_str)
                .map(str::to_string)
        }) else {
            continue;
        };
        let Ok(node) = page
            .call("DOM.requestNode", json!({ "objectId": object_id }))
            .await
        else {
            continue;
        };
        let Some(node_id) = node["nodeId"].as_i64().filter(|n| *n > 0) else {
            continue;
        };
        let Ok(fonts) = page
            .call("CSS.getPlatformFontsForNode", json!({ "nodeId": node_id }))
            .await
        else {
            continue;
        };
        *slot = fonts["fonts"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .max_by_key(|f| f["glyphCount"].as_i64().unwrap_or(0))
                    .and_then(|f| f["familyName"].as_str())
            })
            .map(str::to_string);
    }
    let _ = page.call("CSS.disable", json!({})).await;
    out
}

/// Downloads `url` through the browser (memory cache, then the network stack, then a page fetch).
async fn fetch_resource(page: &Page, frame_id: &str, url: &str) -> Result<Vec<u8>, String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',').ok_or("bad data URL")?;
        return if meta.ends_with(";base64") {
            b64.decode(data.trim()).map_err(|e| e.to_string())
        } else {
            Ok(percent_decode(data))
        };
    }
    if let Ok(r) = page
        .call(
            "Page.getResourceContent",
            json!({ "frameId": frame_id, "url": url }),
        )
        .await
        && let Some(content) = r["content"].as_str()
    {
        let bytes = if r["base64Encoded"].as_bool().unwrap_or(false) {
            b64.decode(content).map_err(|e| e.to_string())?
        } else {
            content.as_bytes().to_vec()
        };
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    if let Ok(r) = page
        .call(
            "Network.loadNetworkResource",
            json!({
                "frameId": frame_id,
                "url": url,
                "options": { "disableCache": false, "includeCredentials": false },
            }),
        )
        .await
    {
        let res = &r["resource"];
        if res["success"].as_bool().unwrap_or(false)
            && let Some(stream) = res["stream"].as_str()
        {
            let mut bytes = Vec::new();
            let mut ok = true;
            loop {
                let Ok(chunk) = page
                    .call("IO.read", json!({ "handle": stream, "size": 1 << 20 }))
                    .await
                else {
                    ok = false;
                    break;
                };
                let data = chunk["data"].as_str().unwrap_or("");
                if chunk["base64Encoded"].as_bool().unwrap_or(false) {
                    match b64.decode(data) {
                        Ok(b) => bytes.extend(b),
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                } else {
                    bytes.extend_from_slice(data.as_bytes());
                }
                if chunk["eof"].as_bool().unwrap_or(true) {
                    break;
                }
            }
            let _ = page.call("IO.close", json!({ "handle": stream })).await;
            if ok && !bytes.is_empty() {
                return Ok(bytes);
            }
        }
    }
    let r = page
        .eval_str(&format!(
            "window.__openpen.fetchB64({})",
            serde_json::to_string(url).unwrap_or_default()
        ))
        .await?;
    let v: Value = serde_json::from_str(&r).map_err(|e| e.to_string())?;
    b64.decode(v["b64"].as_str().unwrap_or(""))
        .map_err(|e| e.to_string())
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Re-encodes images the renderer can't decode (GIF, AVIF, HEIC, ICO, BMP, SVG) as PNG.
async fn to_renderable(
    page: &Page,
    bytes: Vec<u8>,
    display_w: f64,
) -> Option<(Vec<u8>, &'static str)> {
    let kind = sniff(&bytes);
    if kind.renderable() {
        return Some((bytes, kind.ext()));
    }
    if kind == Sniff::Svg {
        return svg::svg_bytes_to_png(&bytes, display_w.max(16.0)).map(|p| (p, "png"));
    }
    if matches!(kind, Sniff::Ttf | Sniff::Otf | Sniff::Woff | Sniff::Woff2) {
        return None;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mime = if kind == Sniff::Unknown {
        "application/octet-stream"
    } else {
        kind.mime()
    };
    let expr = format!(
        "window.__openpen.toPng({}, {}, 0, 0)",
        serde_json::to_string(&b64).unwrap_or_default(),
        serde_json::to_string(mime).unwrap_or_default()
    );
    let png64 = page.eval_str(&expr).await.ok()?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(png64)
        .ok()?;
    (sniff(&png) == Sniff::Png).then_some((png, "png"))
}

#[allow(clippy::too_many_arguments)]
async fn collect_assets(
    page: &Page,
    frame_id: &str,
    snap: &Snapshot,
    writer: &mut AssetWriter,
    refs: &mut AssetRefs,
    font_defs: &mut Vec<Value>,
    font_alias: &mut HashMap<String, String>,
    warnings: &mut Vec<String>,
    progress: &(impl Fn(ImportProgress) + Send + Sync),
) {
    // Image URLs with the largest size they are displayed at.
    let mut images: Vec<(String, f64)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut add = |url: &str, w: f64, images: &mut Vec<(String, f64)>| {
        if url.is_empty() || url.starts_with("blob:") && url.len() > 4096 {
            return;
        }
        match seen.get(url) {
            Some(&i) => images[i].1 = images[i].1.max(w),
            None => {
                seen.insert(url.to_string(), images.len());
                images.push((url.to_string(), w));
            }
        }
    };
    if let Some(root) = &snap.root {
        root.walk(&mut |n| {
            if let Some(src) = &n.src {
                add(src, n.r[2], &mut images);
            }
            if n.s.contains_key("backgroundImage") {
                for l in css::background_layers(n.style("backgroundImage")) {
                    if let css::Layer::Url(u) = l {
                        add(&u, n.r[2], &mut images);
                    }
                }
            }
        });
    }
    let shot_rects: Vec<[f64; 4]> = if snap.shots > 0 {
        page.eval_str("window.__openpen.shotRects()")
            .await
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let faces: Vec<&snapshot::FontFace> = snap.font_faces.iter().take(40).collect();
    let total = images.len() + shot_rects.len() + faces.len();
    let mut done = 0;
    progress(ImportProgress::Assets { done, total });
    let mut failed = 0usize;
    for (url, w) in &images {
        match fetch_resource(page, frame_id, url).await {
            Ok(bytes) => match to_renderable(page, bytes, *w).await {
                Some((data, ext)) => match writer.store(&data, Kind::Images, ext) {
                    Ok(rel) => {
                        refs.images.insert(url.clone(), rel);
                    }
                    Err(e) => warnings.push(e),
                },
                None => failed += 1,
            },
            Err(_) => failed += 1,
        }
        done += 1;
        progress(ImportProgress::Assets { done, total });
    }
    if failed > 0 {
        warnings.push(format!(
            "{failed} image(s) could not be downloaded or decoded; they are placeholders."
        ));
    }
    for (i, r) in shot_rects.iter().enumerate().take(200) {
        if r[2] >= 1.0
            && r[3] >= 1.0
            && let Ok(png) = capture(page, Some(*r)).await
            && let Ok(rel) = writer.store(&png, Kind::Images, "png")
        {
            refs.shots.insert(i, rel);
        }
        done += 1;
        progress(ImportProgress::Assets { done, total });
    }
    let mut woff = false;
    for f in faces {
        if let Ok(bytes) = fetch_resource(page, frame_id, &f.url).await {
            let kind = sniff(&bytes);
            if matches!(kind, Sniff::Ttf | Sniff::Otf | Sniff::Woff | Sniff::Woff2)
                && let Ok(rel) = writer.store(&bytes, Kind::Fonts, kind.ext())
            {
                woff |= matches!(kind, Sniff::Woff | Sniff::Woff2);
                let name = assets::font_family(&bytes).unwrap_or_else(|| f.family.clone());
                font_alias
                    .entry(f.family.to_ascii_lowercase())
                    .or_insert_with(|| name.clone());
                let weight = f
                    .weight
                    .split_whitespace()
                    .next()
                    .and_then(|w| match w {
                        "normal" => Some(400.0),
                        "bold" => Some(700.0),
                        w => w.parse::<f64>().ok(),
                    })
                    .unwrap_or(400.0);
                let mut def = json!({ "name": name, "url": rel, "weight": weight });
                if f.style.contains("italic") || f.style.contains("oblique") {
                    def["style"] = Value::String("italic".into());
                }
                if !font_defs.contains(&def) {
                    font_defs.push(def);
                }
            }
        }
        done += 1;
        progress(ImportProgress::Assets { done, total });
    }
    if woff {
        warnings.push(
            "Some web fonts are WOFF/WOFF2; they were saved, but text may render with a fallback font."
                .into(),
        );
    }
}
