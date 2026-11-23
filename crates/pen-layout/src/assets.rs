//! External resources needed to build the display list: images of image
//! fills and icon glyphs.

use pen_render::{ImageData, Path};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::SystemTime;

/// Result of [`AssetResolver::image_lookup`].
#[derive(Debug, Clone)]
pub enum ImageLookup {
    Ready(Arc<ImageData>),
    /// Still loading (asynchronous resolvers).
    Pending,
    /// Not found / not decodable.
    Missing,
}

/// Vector glyph of an icon, in a square `view_box x view_box` space.
#[derive(Debug, Clone)]
pub struct IconGlyph {
    pub path: Arc<Path>,
    pub view_box: f32,
    /// Stroke-based icon set (lucide/feather): drawn with a stroke of
    /// `stroke_width` (in view-box units) instead of filled.
    pub stroked: bool,
    pub stroke_width: f32,
}

/// Supplies images and icons. Calls are cached by the display-list builder
/// (one call per distinct url / icon). `Send + Sync` so a [`crate::Pipeline`]
/// can be built on a worker thread and handed to the UI thread.
pub trait AssetResolver: Send + Sync {
    /// Decoded image for an image fill `url` (as written in the file).
    fn image(&self, url: &str) -> Option<Arc<ImageData>>;
    /// Non-blocking variant used by the display-list builder. `Pending` draws a
    /// placeholder and is asked again by [`crate::Pipeline::refresh_assets`]
    /// once [`AssetResolver::take_ready`] reports progress. Default: [`Self::image`].
    fn image_lookup(&self, url: &str) -> ImageLookup {
        self.image(url)
            .map_or(ImageLookup::Missing, ImageLookup::Ready)
    }
    /// True if some `Pending` image finished loading since the last call.
    fn take_ready(&self) -> bool {
        false
    }
    /// Re-checks the files behind the images handed out so far and returns the
    /// urls whose file changed on disk (size or modification time). Those are
    /// forgotten, so the next lookup reloads them. Default: nothing changes.
    fn changed_images(&self) -> Vec<String> {
        Vec::new()
    }
    /// Glyph of icon `name` from icon font `library` (`lucide`, `Material
    /// Symbols Rounded`, ...). `None` draws a placeholder. The default uses the
    /// bundled icon fonts ([`crate::icons::bundled_icon`]).
    fn icon(&self, library: &str, name: &str, weight: f32) -> Option<IconGlyph> {
        crate::icons::bundled_icon(library, name, weight)
    }
}

/// No images, no icons (placeholders everywhere).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAssets;

impl AssetResolver for NoAssets {
    fn image(&self, _: &str) -> Option<Arc<ImageData>> {
        None
    }
    fn icon(&self, _: &str, _: &str, _: f32) -> Option<IconGlyph> {
        None
    }
}

/// No images, bundled icons only.
#[derive(Debug, Default, Clone, Copy)]
pub struct BundledIcons;

impl AssetResolver for BundledIcons {
    fn image(&self, _: &str) -> Option<Arc<ImageData>> {
        None
    }
}

/// Loads images from disk relative to the document directory (`http(s)` and
/// `data:` URLs are not fetched).
///
/// [`FsAssets::new`] decodes synchronously (CLI, tests). [`FsAssets::deferred`]
/// decodes on a background thread: the first lookup of an image answers
/// [`ImageLookup::Pending`] (the builder draws a placeholder) and
/// [`crate::Pipeline::refresh_assets`] later rebuilds the chunks that used it.
#[derive(Debug, Default, Clone)]
pub struct FsAssets {
    pub base_dir: Option<PathBuf>,
    deferred: Option<Arc<Deferred>>,
    /// File and stamp of every image handed out (see [`AssetResolver::changed_images`]).
    seen: Arc<Mutex<SeenImages>>,
}

/// File and stamp of each image url handed out.
type SeenImages = HashMap<String, (PathBuf, Option<FileStamp>)>;

/// Identity of an image file's content: a different size or modification
/// time means the file changed (and gets a new [`ImageData::id`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl FileStamp {
    pub fn of(path: &FsPath) -> Option<FileStamp> {
        let m = std::fs::metadata(path).ok()?;
        Some(FileStamp {
            len: m.len(),
            modified: m.modified().ok(),
        })
    }
}

#[derive(Debug, Default)]
struct Deferred {
    state: Mutex<DeferredState>,
    /// Set when a decode finished since the last [`AssetResolver::take_ready`].
    ready: AtomicBool,
}

#[derive(Debug, Default)]
struct DeferredState {
    done: HashMap<String, Option<Arc<ImageData>>>,
    queued: HashSet<String>,
    worker: Option<mpsc::Sender<(String, PathBuf)>>,
}

impl FsAssets {
    pub fn new(base_dir: Option<PathBuf>) -> Self {
        Self {
            base_dir,
            ..Default::default()
        }
    }

    /// Decodes images on a background thread (see the type docs).
    pub fn deferred(base_dir: Option<PathBuf>) -> Self {
        Self {
            base_dir,
            deferred: Some(Arc::new(Deferred::default())),
            ..Default::default()
        }
    }

    fn remember(&self, url: &str, path: &FsPath) {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if !seen.contains_key(url) {
            seen.insert(url.to_owned(), (path.to_path_buf(), FileStamp::of(path)));
        }
    }

    fn path_of(&self, url: &str) -> Option<PathBuf> {
        if url.is_empty() || url.contains("://") || url.starts_with("data:") {
            return None;
        }
        let rel = pen_doc::library::percent_decode(url.strip_prefix("file://").unwrap_or(url));
        let p = FsPath::new(&rel);
        Some(match &self.base_dir {
            Some(b) if p.is_relative() => b.join(p),
            _ => p.to_path_buf(),
        })
    }
}

fn load_image(path: &FsPath) -> Option<Arc<ImageData>> {
    let stamp = FileStamp::of(path);
    let bytes = std::fs::read(path).ok()?;
    decode_image(&bytes, file_image_id(path, stamp)).map(Arc::new)
}

impl Deferred {
    fn lookup(self: &Arc<Self>, url: &str, path: PathBuf) -> ImageLookup {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(r) = st.done.get(url) {
            return r.clone().map_or(ImageLookup::Missing, ImageLookup::Ready);
        }
        if st.queued.insert(url.to_owned()) {
            if st.worker.is_none() {
                let (tx, rx) = mpsc::channel::<(String, PathBuf)>();
                let weak = Arc::downgrade(self);
                let spawned = std::thread::Builder::new()
                    .name("pen-image-decode".into())
                    .spawn(move || {
                        // Ends when the resolver (and with it the sender) is dropped.
                        while let Ok((url, path)) = rx.recv() {
                            let img = load_image(&path);
                            let Some(d) = weak.upgrade() else { break };
                            let mut st = d.state.lock().unwrap_or_else(|p| p.into_inner());
                            st.queued.remove(&url);
                            st.done.insert(url, img);
                            d.ready.store(true, Ordering::Release);
                        }
                    });
                if spawned.is_ok() {
                    st.worker = Some(tx);
                }
            }
            let sent = st
                .worker
                .as_ref()
                .is_some_and(|w| w.send((url.to_owned(), path.clone())).is_ok());
            if !sent {
                // No worker thread: decode inline.
                st.queued.remove(url);
                let img = load_image(&path);
                st.done.insert(url.to_owned(), img.clone());
                return img.map_or(ImageLookup::Missing, ImageLookup::Ready);
            }
        }
        ImageLookup::Pending
    }
}

impl AssetResolver for FsAssets {
    fn image(&self, url: &str) -> Option<Arc<ImageData>> {
        let path = self.path_of(url)?;
        self.remember(url, &path);
        load_image(&path)
    }

    fn image_lookup(&self, url: &str) -> ImageLookup {
        let Some(path) = self.path_of(url) else {
            return ImageLookup::Missing;
        };
        self.remember(url, &path);
        match &self.deferred {
            Some(d) => d.lookup(url, path),
            None => load_image(&path).map_or(ImageLookup::Missing, ImageLookup::Ready),
        }
    }

    fn take_ready(&self) -> bool {
        self.deferred
            .as_ref()
            .is_some_and(|d| d.ready.swap(false, Ordering::AcqRel))
    }

    fn changed_images(&self) -> Vec<String> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let changed: Vec<String> = seen
            .iter()
            .filter(|(_, (path, stamp))| FileStamp::of(path) != *stamp)
            .map(|(url, _)| url.clone())
            .collect();
        if changed.is_empty() {
            return changed;
        }
        let mut done = self
            .deferred
            .as_ref()
            .map(|d| d.state.lock().unwrap_or_else(|p| p.into_inner()));
        for url in &changed {
            seen.remove(url);
            if let Some(st) = done.as_mut() {
                st.done.remove(url);
            }
        }
        changed
    }
}

/// Stable id of an image url.
pub fn image_id(url: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    h.finish()
}

/// Id of the decoded content of an image file: its path plus its
/// [`FileStamp`], so a file rewritten on disk gets a new id (and new GPU /
/// raster textures, which are keyed by [`ImageData::id`]).
pub fn file_image_id(path: &FsPath, stamp: Option<FileStamp>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    stamp.hash(&mut h);
    h.finish()
}

/// Decodes PNG/JPEG/WebP into premultiplied RGBA8.
pub fn decode_image(bytes: &[u8], id: u64) -> Option<ImageData> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    let mut px = img.into_raw();
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
    Some(ImageData {
        id,
        width,
        height,
        rgba_premul: px,
    })
}
