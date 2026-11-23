//! Home thumbnails: a low-priority worker pool renders each recent document
//! with pen-layout + pen-raster (fit into 512 px, like Pen's preview process)
//! and caches the PNG under `<data dir>/previews/<id>.png`.
//!
//! Freshness follows the legacy "stamp": the PNG's mtime is set to the
//! document's mtime, and a preview is stale when the two differ at second
//! precision. A failed render writes an empty (1x1 transparent) stamped PNG,
//! so a broken document is not retried until it changes.
//!
//! No GPUI here: results go to a callback as straight RGBA pixels.

use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use pen_render::{Color, Rect};

/// Longest side of a preview, in pixels.
pub const PREVIEW_SIZE: f32 = 512.0;

/// Cache id of a document: 64-bit FNV-1a of its normalized path (stable
/// across runs and Rust versions).
pub fn preview_id(doc: &Path) -> String {
    let key = crate::recent::path_key(doc);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn preview_path(cache_dir: &Path, doc: &Path) -> PathBuf {
    cache_dir.join(format!("{}.png", preview_id(doc)))
}

/// `<data dir>/previews`.
pub fn cache_dir() -> Option<PathBuf> {
    crate::recent::data_dir().map(|d| d.join("previews"))
}

fn secs(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
    }
}

/// A preview stamped `preview_mtime` is current for a document modified at
/// `doc_mtime` (same second, like Pen's `floor(ms / 1000)` comparison).
pub fn is_fresh(doc_mtime: SystemTime, preview_mtime: SystemTime) -> bool {
    secs(doc_mtime) == secs(preview_mtime)
}

/// Rendered preview pixels (straight RGBA, row-major).
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PreviewOutcome {
    Image(PreviewImage),
    /// Nothing to draw (empty document or failed render).
    Empty,
    /// The document does not exist.
    Missing,
}

#[derive(Clone, Debug)]
pub struct PreviewResult {
    pub doc: PathBuf,
    /// Document mtime the preview matches.
    pub doc_mtime: Option<SystemTime>,
    pub outcome: PreviewOutcome,
    /// Rendered now (false: read from the cache).
    pub rendered: bool,
    pub elapsed: Duration,
    pub error: Option<String>,
}

/// Renders a document into a preview that fits [`PREVIEW_SIZE`]; `None` when
/// it has nothing to draw.
pub fn render_document(doc: &Path) -> anyhow::Result<Option<pen_raster::Pixmap>> {
    let bytes = std::fs::read(doc)?;
    let dir = doc
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf);
    let (mut scene, _) =
        pen_doc::SceneDoc::load(&bytes, dir.as_deref(), &pen_doc::FsLibraries::default())
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut pipe = pen_layout::Pipeline::new(
        pen_layout::TextEngine::new(),
        Box::new(pen_layout::FsAssets::new(dir)),
    );
    pipe.set_script_runner(Box::new(pen_script::ScriptNodeRunner::new()));
    pipe.set_shader_renderer(Arc::new(pen_shader::ShaderEngine::new()));
    pipe.rebuild(&mut scene);
    let list = std::mem::take(&mut pipe.list);
    let Some(b) = list.bounds().filter(|b| b.w > 0.0 && b.h > 0.0) else {
        return Ok(None);
    };
    let scale = PREVIEW_SIZE / b.w.max(b.h);
    let viewport = Rect::new(b.x, b.y, b.w, b.h);
    let pixmap = RASTER.with(|c| {
        c.borrow_mut()
            .render(&list, viewport, scale, Color::TRANSPARENT)
    });
    Ok(Some(pixmap))
}

thread_local! {
    /// One raster context per worker (fonts and glyph caches are reused
    /// between documents; no lock shared with the canvas).
    static RASTER: std::cell::RefCell<pen_raster::RasterContext> =
        std::cell::RefCell::new(pen_raster::RasterContext::new());
}

fn pixmap_rgba(p: &pen_raster::Pixmap) -> PreviewImage {
    let mut rgba = Vec::with_capacity(p.pixels().len() * 4);
    for px in p.pixels() {
        let c = px.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    PreviewImage {
        width: p.width(),
        height: p.height(),
        rgba,
    }
}

/// A 1x1 transparent PNG (the "nothing to show" marker).
fn empty_png() -> Vec<u8> {
    let mut out = Vec::new();
    let img = image::RgbaImage::new(1, 1);
    let _ = img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

/// Writes `data` to `path` through a temp file stamped with `mtime`, then
/// renames it (3 tries, 100 ms apart: antivirus/indexers hold files on Windows).
pub fn write_stamped(path: &Path, data: &[u8], mtime: SystemTime) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        f.set_modified(mtime)?;
        drop(f);
        let mut tries = 0;
        loop {
            match std::fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(e) if tries < 3 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        }
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

fn decode_png(bytes: &[u8]) -> Option<PreviewOutcome> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let img = img.to_rgba8();
    if img.width() <= 1 && img.height() <= 1 {
        return Some(PreviewOutcome::Empty);
    }
    Some(PreviewOutcome::Image(PreviewImage {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }))
}

/// Makes sure the cached preview of `doc` is current and returns it:
/// reads the cache when fresh, renders and writes it otherwise.
pub fn ensure_preview(cache_dir: &Path, doc: &Path) -> PreviewResult {
    let t0 = Instant::now();
    let result = |doc_mtime, outcome, rendered, error| PreviewResult {
        doc: doc.to_path_buf(),
        doc_mtime,
        outcome,
        rendered,
        elapsed: t0.elapsed(),
        error,
    };
    let Ok(doc_mtime) = std::fs::metadata(doc).and_then(|m| m.modified()) else {
        return result(None, PreviewOutcome::Missing, false, None);
    };
    let png = preview_path(cache_dir, doc);
    if let Ok(m) = std::fs::metadata(&png).and_then(|m| m.modified())
        && is_fresh(doc_mtime, m)
        && let Some(outcome) = std::fs::read(&png).ok().and_then(|b| decode_png(&b))
    {
        return result(Some(doc_mtime), outcome, false, None);
    }
    let rendered = std::panic::catch_unwind(AssertUnwindSafe(|| render_document(doc)));
    let (bytes, outcome, error) = match rendered {
        Ok(Ok(Some(pixmap))) => match pixmap.encode_png() {
            Ok(bytes) => (bytes, PreviewOutcome::Image(pixmap_rgba(&pixmap)), None),
            Err(e) => (empty_png(), PreviewOutcome::Empty, Some(e.to_string())),
        },
        Ok(Ok(None)) => (empty_png(), PreviewOutcome::Empty, None),
        Ok(Err(e)) => (empty_png(), PreviewOutcome::Empty, Some(e.to_string())),
        Err(_) => (
            empty_png(),
            PreviewOutcome::Empty,
            Some("renderer panicked".into()),
        ),
    };
    let _ = std::fs::create_dir_all(cache_dir);
    let error = match write_stamped(&png, &bytes, doc_mtime) {
        Ok(()) => error,
        Err(e) => Some(error.unwrap_or_else(|| format!("writing {}: {e}", png.display()))),
    };
    result(Some(doc_mtime), outcome, true, error)
}

/// Moves a cached preview along with its document (rename).
pub fn rename_cached(cache_dir: &Path, old: &Path, new: &Path) {
    let _ = std::fs::rename(preview_path(cache_dir, old), preview_path(cache_dir, new));
}

pub fn remove_cached(cache_dir: &Path, doc: &Path) {
    let _ = std::fs::remove_file(preview_path(cache_dir, doc));
}

// ------------------------------------------------------------------ pool

type Sink = Arc<dyn Fn(PreviewResult) + Send + Sync>;

struct Queue {
    pending: VecDeque<PathBuf>,
    busy: HashSet<String>,
    workers: usize,
    idle: usize,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    cache_dir: PathBuf,
    sink: Sink,
    max_workers: usize,
}

/// Low-priority worker threads that run [`ensure_preview`] jobs. Workers
/// start on demand (at most `max_workers`) and exit when the pool drops.
pub struct PreviewPool {
    shared: Arc<Shared>,
}

/// Default concurrency: half the cores, 1 to 3 threads.
pub fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(1, 3))
        .unwrap_or(1)
}

impl PreviewPool {
    pub fn new(
        cache_dir: PathBuf,
        max_workers: usize,
        sink: impl Fn(PreviewResult) + Send + Sync + 'static,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue {
                    pending: VecDeque::new(),
                    busy: HashSet::new(),
                    workers: 0,
                    idle: 0,
                    shutdown: false,
                }),
                ready: Condvar::new(),
                cache_dir,
                sink: Arc::new(sink),
                max_workers: max_workers.max(1),
            }),
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.shared.cache_dir
    }

    /// Replaces the queue with `docs` (in priority order), skipping the ones
    /// being worked on.
    pub fn request(&self, docs: impl IntoIterator<Item = PathBuf>) {
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.pending.clear();
        let mut seen = HashSet::new();
        for d in docs {
            let key = crate::recent::path_key(&d);
            if !q.busy.contains(&key) && seen.insert(key) {
                q.pending.push_back(d);
            }
        }
        let want = q.pending.len().min(self.shared.max_workers);
        while q.workers < want {
            let shared = self.shared.clone();
            let spawned = std::thread::Builder::new()
                .name("openpen-preview".into())
                .spawn(move || worker(shared));
            if spawned.is_err() {
                break;
            }
            q.workers += 1;
        }
        self.shared.ready.notify_all();
    }

    /// Jobs not started yet.
    pub fn pending(&self) -> usize {
        let q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.pending.len() + q.busy.len()
    }
}

impl Drop for PreviewPool {
    fn drop(&mut self) {
        let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.shutdown = true;
        q.pending.clear();
        self.shared.ready.notify_all();
    }
}

fn worker(shared: Arc<Shared>) {
    lower_thread_priority();
    loop {
        let doc = {
            let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    q.workers -= 1;
                    return;
                }
                if let Some(d) = q.pending.pop_front() {
                    q.busy.insert(crate::recent::path_key(&d));
                    break d;
                }
                q.idle += 1;
                q = shared.ready.wait(q).unwrap_or_else(|e| e.into_inner());
                q.idle -= 1;
            }
        };
        let res = ensure_preview(&shared.cache_dir, &doc);
        {
            let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.busy.remove(&crate::recent::path_key(&doc));
        }
        (shared.sink)(res);
    }
}

/// Background priority (CPU and I/O) on Windows, so previews never compete
/// with the UI thread.
fn lower_thread_priority() {
    #[cfg(windows)]
    // SAFETY: plain Win32 calls on the current thread's pseudo-handle.
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
            THREAD_PRIORITY_LOWEST,
        };
        let t = GetCurrentThread();
        if SetThreadPriority(t, THREAD_MODE_BACKGROUND_BEGIN) == 0 {
            SetThreadPriority(t, THREAD_PRIORITY_LOWEST);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const DOC: &str = r##"{"version":"2.10","children":[{"type":"frame","id":"a","x":0,"y":0,"width":200,"height":100,"fill":"#ff0000"}]}"##;

    #[test]
    fn freshness_is_per_second() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_250);
        assert!(is_fresh(t, t + Duration::from_millis(500)));
        assert!(!is_fresh(t, t + Duration::from_millis(900)));
        assert!(!is_fresh(t, t - Duration::from_secs(5)));
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let a = preview_id(Path::new("/x/a.pen"));
        assert_eq!(a, preview_id(Path::new("/x/a.pen")));
        assert_ne!(a, preview_id(Path::new("/x/b.pen")));
        assert_eq!(a.len(), 16);
        if cfg!(windows) {
            assert_eq!(
                preview_id(Path::new(r"C:\X\A.pen")),
                preview_id(Path::new("c:/x/a.pen"))
            );
        }
    }

    #[test]
    fn generates_stamps_and_reuses() {
        let dir = tmp("openpen-preview-gen");
        let doc = dir.join("doc.pen");
        std::fs::write(&doc, DOC).unwrap();
        let cache = dir.join("previews");
        let r = ensure_preview(&cache, &doc);
        assert!(r.rendered, "{:?}", r.error);
        let PreviewOutcome::Image(img) = &r.outcome else {
            panic!("no image: {:?}", r.outcome);
        };
        assert_eq!((img.width, img.height), (512, 256));
        // Opaque red in the middle.
        let mid = ((128 * 512 + 256) * 4) as usize;
        assert_eq!(&img.rgba[mid..mid + 4], &[255, 0, 0, 255]);
        let png = preview_path(&cache, &doc);
        let stamp = std::fs::metadata(&png).unwrap().modified().unwrap();
        let doc_m = std::fs::metadata(&doc).unwrap().modified().unwrap();
        assert!(is_fresh(doc_m, stamp));
        // Second call reads the cache.
        let r2 = ensure_preview(&cache, &doc);
        assert!(!r2.rendered);
        assert_eq!(r2.outcome, r.outcome);
        // Touching the document makes it stale.
        let f = std::fs::File::options().write(true).open(&doc).unwrap();
        f.set_modified(doc_m + Duration::from_secs(10)).unwrap();
        drop(f);
        assert!(ensure_preview(&cache, &doc).rendered);
        // Leftover temp files: none.
        let tmp_left = std::fs::read_dir(&cache)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(tmp_left, 0);
    }

    #[test]
    fn broken_and_missing_documents() {
        let dir = tmp("openpen-preview-broken");
        let cache = dir.join("previews");
        let bad = dir.join("bad.pen");
        std::fs::write(&bad, "{ not json").unwrap();
        let r = ensure_preview(&cache, &bad);
        assert_eq!(r.outcome, PreviewOutcome::Empty);
        assert!(r.error.is_some());
        // The empty stamped PNG stops retries until the file changes.
        let r2 = ensure_preview(&cache, &bad);
        assert!(!r2.rendered);
        assert_eq!(r2.outcome, PreviewOutcome::Empty);
        let r3 = ensure_preview(&cache, &dir.join("nope.pen"));
        assert_eq!(r3.outcome, PreviewOutcome::Missing);
        let empty = dir.join("empty.pen");
        std::fs::write(&empty, r#"{"version":"2.10","children":[]}"#).unwrap();
        assert_eq!(
            ensure_preview(&cache, &empty).outcome,
            PreviewOutcome::Empty
        );
    }

    #[test]
    fn pool_streams_results() {
        let dir = tmp("openpen-preview-pool");
        let mut docs = Vec::new();
        for i in 0..6 {
            let d = dir.join(format!("d{i}.pen"));
            std::fs::write(&d, DOC).unwrap();
            docs.push(d);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let pool = PreviewPool::new(dir.join("previews"), 2, move |r| {
            let _ = tx.lock().unwrap().send(r);
        });
        pool.request(docs.clone());
        let mut got = HashSet::new();
        for _ in 0..docs.len() {
            let r = rx.recv_timeout(Duration::from_secs(60)).unwrap();
            assert!(matches!(r.outcome, PreviewOutcome::Image(_)));
            got.insert(r.doc);
        }
        assert_eq!(got.len(), docs.len());
        assert!(pool.shared.queue.lock().unwrap().workers <= 2);
    }

    /// Previews of corpus copies (skipped without `testdata/`).
    #[test]
    fn corpus_previews() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let mut files: Vec<PathBuf> = ["user", "legacy"]
            .iter()
            .filter_map(|d| std::fs::read_dir(root.join(d)).ok())
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "pen"))
            .collect();
        if files.is_empty() {
            eprintln!("skip: no corpus");
            return;
        }
        files.sort();
        files.truncate(4);
        let dir = tmp("openpen-preview-corpus");
        for f in files {
            let copy = dir.join(f.file_name().unwrap());
            std::fs::copy(&f, &copy).unwrap();
            let r = ensure_preview(&dir.join("previews"), &copy);
            eprintln!("{}: {:?} in {:.1?}", copy.display(), r.error, r.elapsed);
            if let PreviewOutcome::Image(img) = &r.outcome {
                assert!(img.width.max(img.height) as f32 <= PREVIEW_SIZE + 1.0);
                assert!(img.width.max(img.height) as f32 >= PREVIEW_SIZE - 1.0);
            } else {
                assert!(r.error.is_none(), "{:?}", r.error);
            }
        }
    }
}
