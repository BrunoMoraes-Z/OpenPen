//! Backend CPU (tiny-skia + cosmic-text) da DisplayList: headless, export PNG, testes golden.
//!
//! This is the reference backend: thumbnails, PNG export, golden tests and what an agent "sees".
//! Entry points:
//! - [`render`] / [`render_to_png`]: one-shot helpers using a process-wide shared [`RasterContext`].
//! - [`RasterContext`]: owns the font system, glyph cache, image cache and scratch buffers so that
//!   repeated renders are fast. Create one per thread for parallel rendering.

mod blur;
pub mod geom;
mod paint;
mod renderer;
mod text;

pub use cosmic_text::FontSystem;
pub use tiny_skia::Pixmap;

use pen_render::{Color, DisplayList, Rect};
use renderer::{Renderer, Resources};
use std::sync::{Mutex, OnceLock};

/// Largest output side, in pixels (larger requests are clamped).
pub const MAX_SIDE: u32 = 16_384;

#[derive(Debug, thiserror::Error)]
pub enum RasterError {
    #[error("PNG encoding failed: {0}")]
    Png(String),
}

/// Reusable rendering state: fonts (loaded lazily on the first text item), glyph/shaping caches,
/// decoded images and scratch buffers.
#[derive(Default)]
pub struct RasterContext {
    res: Resources,
}

impl RasterContext {
    /// A context with the bundled fonts (Inter); installed families are copied in on demand,
    /// the first time a text names one (see `pen_fonts::system`).
    pub fn new() -> Self {
        Self::default()
    }

    /// A context using only a caller-provided font system (plus the bundled fonts).
    pub fn with_font_system(fs: FontSystem) -> Self {
        let mut c = Self::default();
        c.res.pending_fonts = Some(fs);
        c
    }

    /// The font system (creates the text engine if needed).
    pub fn font_system(&mut self) -> &mut FontSystem {
        &mut self.res.text_engine().fs
    }

    /// Registers a font file (TTF/OTF/collection) from memory.
    pub fn load_font_data(&mut self, data: Vec<u8>) {
        let engine = self.res.text_engine();
        engine.fs.db_mut().load_font_data(data);
        engine.clear_caches();
    }

    /// True if a font family with this name is available (case-insensitive).
    pub fn has_font_family(&mut self, name: &str) -> bool {
        self.res.text_engine().has_family(name)
    }

    /// Drops decoded images and shaped text (fonts stay loaded).
    pub fn clear_caches(&mut self) {
        self.res.images.clear();
        if let Some(t) = self.res.text.as_mut() {
            t.clear_caches();
        }
    }

    /// Sets the soft memory budget of the decoded-image cache, in bytes.
    pub fn set_image_cache_budget(&mut self, bytes: usize) {
        self.res.images.budget = bytes;
    }

    /// Renders the part of `list` inside `viewport` (document space) at `scale` device pixels per
    /// document unit, over `background`.
    pub fn render(
        &mut self,
        list: &DisplayList,
        viewport: Rect,
        scale: f32,
        background: Color,
    ) -> Pixmap {
        let (w, h) = output_size(&viewport, scale);
        let mut pixmap = Pixmap::new(w, h).expect("non-zero clamped size");
        self.render_into(list, viewport, scale, background, &mut pixmap);
        pixmap
    }

    /// Like [`Self::render`] but draws into an existing pixmap (its size defines the output), so the
    /// caller can reuse the allocation across frames.
    pub fn render_into(
        &mut self,
        list: &DisplayList,
        viewport: Rect,
        scale: f32,
        background: Color,
        out: &mut Pixmap,
    ) {
        out.fill(paint::ts_color(background));
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let view = geom::view_transform(&viewport, scale);
        let target = std::mem::replace(out, Pixmap::new(1, 1).expect("1x1"));
        let mut r = Renderer::new(&mut self.res, target, view);
        for chunk in list.visible(&viewport) {
            for item in &chunk.items {
                r.item(item);
            }
        }
        *out = r.finish();
    }

    /// Renders and encodes as PNG (straight alpha, sRGB).
    pub fn render_to_png(
        &mut self,
        list: &DisplayList,
        viewport: Rect,
        scale: f32,
        background: Color,
    ) -> Result<Vec<u8>, RasterError> {
        self.render(list, viewport, scale, background)
            .encode_png()
            .map_err(|e| RasterError::Png(e.to_string()))
    }
}

/// Output pixel size for a viewport at `scale` (at least 1x1, at most [`MAX_SIDE`]).
pub fn output_size(viewport: &Rect, scale: f32) -> (u32, u32) {
    let side = |v: f32| {
        let p = (v * scale).ceil();
        if p.is_finite() {
            (p.max(1.0) as u32).min(MAX_SIDE)
        } else {
            1
        }
    };
    (side(viewport.w), side(viewport.h))
}

fn shared() -> &'static Mutex<RasterContext> {
    static SHARED: OnceLock<Mutex<RasterContext>> = OnceLock::new();
    SHARED.get_or_init(|| Mutex::new(RasterContext::new()))
}

/// Runs `f` with the process-wide shared context (serialized by a mutex).
pub fn with_shared_context<R>(f: impl FnOnce(&mut RasterContext) -> R) -> R {
    let mut guard = shared().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Renders `list` with the shared context. See [`RasterContext::render`].
pub fn render(list: &DisplayList, viewport: Rect, scale: f32, background: Color) -> Pixmap {
    with_shared_context(|c| c.render(list, viewport, scale, background))
}

/// Renders `list` with the shared context and encodes it as PNG.
pub fn render_to_png(
    list: &DisplayList,
    viewport: Rect,
    scale: f32,
    background: Color,
) -> Result<Vec<u8>, RasterError> {
    with_shared_context(|c| c.render_to_png(list, viewport, scale, background))
}
