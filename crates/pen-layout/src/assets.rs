//! External resources needed to build the display list: images of image
//! fills and icon glyphs.

use pen_render::{ImageData, Path};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

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
            deferred: None,
        }
    }

    /// Decodes images on a background thread (see the type docs).
    pub fn deferred(base_dir: Option<PathBuf>) -> Self {
        Self {
            base_dir,
            deferred: Some(Arc::new(Deferred::default())),
        }
    }

    fn path_of(&self, url: &str) -> Option<PathBuf> {
        if url.is_empty() || url.contains("://") || url.starts_with("data:") {
            return None;
        }
        let rel = pen_doc::library::percent_decode(url.strip_prefix("file://").unwrap_or(url));
        let p = std::path::Path::new(&rel);
        Some(match &self.base_dir {
            Some(b) if p.is_relative() => b.join(p),
            _ => p.to_path_buf(),
        })
    }
}

fn load_image(path: &std::path::Path, url: &str) -> Option<Arc<ImageData>> {
    let bytes = std::fs::read(path).ok()?;
    decode_image(&bytes, image_id(url)).map(Arc::new)
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
                            let img = load_image(&path, &url);
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
                let img = load_image(&path, url);
                st.done.insert(url.to_owned(), img.clone());
                return img.map_or(ImageLookup::Missing, ImageLookup::Ready);
            }
        }
        ImageLookup::Pending
    }
}

impl AssetResolver for FsAssets {
    fn image(&self, url: &str) -> Option<Arc<ImageData>> {
        load_image(&self.path_of(url)?, url)
    }

    fn image_lookup(&self, url: &str) -> ImageLookup {
        let Some(path) = self.path_of(url) else {
            return ImageLookup::Missing;
        };
        match &self.deferred {
            Some(d) => d.lookup(url, path),
            None => load_image(&path, url).map_or(ImageLookup::Missing, ImageLookup::Ready),
        }
    }

    fn take_ready(&self) -> bool {
        self.deferred
            .as_ref()
            .is_some_and(|d| d.ready.swap(false, Ordering::AcqRel))
    }
}

/// Stable id of an image url.
pub fn image_id(url: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
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
