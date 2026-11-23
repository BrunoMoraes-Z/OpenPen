//! GPUI painting of a `DisplayList`: per-chunk prepared data (bounds, tessellations, shaped
//! text), image/gradient texture caches and the per-frame painter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    Background, BorderStyle, Bounds, BoxShadow, ContentMask, Corners, Edges, Font, FontFeatures,
    FontId, FontStyle, FontWeight, GlyphId, Hsla, LineLayout, PaintQuad, Path, Pixels, Point,
    RenderImage, Rgba, SharedString, TextRun, Window, WindowTextSystem, linear_color_stop,
    linear_gradient, point, px, quad, size, transparent_black,
};
use pen_render::{
    Affine2, Color, DisplayItem, ImageData, ImageFit, NodeKey, Paint, Rect, Shadow, Shape, Stroke,
    TextAlign, TextBlock, Vec2,
};

use crate::fallback::{
    self, ChunkPlan, FallbackCache, FallbackKind, FallbackScope, RegionLimits, ScopeId, TexKey,
};
use crate::geom::{self, Tess};
use crate::lod::LodPolicy;
use crate::order::Segment;

/// Counters for the last painted frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameStats {
    /// CPU time spent in the canvas paint callback.
    pub paint_time: Duration,
    /// When the canvas paint callback finished.
    pub painted_at: Option<Instant>,
    pub chunks_total: u32,
    pub chunks_visible: u32,
    pub items_drawn: u32,
    pub items_culled: u32,
    pub quads: u32,
    pub paths: u32,
    pub path_triangles: u32,
    pub glyphs: u32,
    pub text_bars: u32,
    pub images: u32,
    pub shadows: u32,
    /// Items rendered with an approximation (unsupported blend/blur, path shadows, ...).
    pub approximated: u32,
    pub retessellated: u32,
    pub chunks_prepared: u32,
    pub lines_shaped: u32,
    /// Time spent painting shapes / text (subset of `paint_time`).
    pub shape_time: Duration,
    pub text_time: Duration,
    /// Time spent building and submitting GPUI paths (subset of `shape_time`).
    pub path_time: Duration,
    /// Chunk segments painted as one GPUI layer / with per-primitive ordering.
    pub layers: u32,
    pub direct_segments: u32,
    /// Visible scopes that need the CPU-raster fallback.
    pub fallback_scopes: u32,
    /// ... drawn from their exact texture / from another zoom bucket while it renders /
    /// approximated on the GPU because no texture covers them yet.
    pub fallback_drawn: u32,
    pub fallback_standin: u32,
    pub fallback_waiting: u32,
    /// Fallback textures dispatched to the background executor this frame.
    pub fallback_jobs: u32,
    /// Bytes held by the fallback texture cache.
    pub fallback_bytes: u64,
}

// ---------------------------------------------------------------------------------------------
// Conversions

pub(crate) fn bounds_of(r: &Rect) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(r.x), px(r.y)),
        size: size(px(r.w), px(r.h)),
    }
}

pub(crate) fn rect_of(b: &Bounds<Pixels>) -> Rect {
    Rect::new(
        b.origin.x.as_f32(),
        b.origin.y.as_f32(),
        b.size.width.as_f32(),
        b.size.height.as_f32(),
    )
}

fn intersect(a: &Rect, b: &Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Rect::new(
        x,
        y,
        (a.right().min(b.right()) - x).max(0.0),
        (a.bottom().min(b.bottom()) - y).max(0.0),
    )
}

fn intersect_opt(a: &Rect, b: &Rect) -> Option<Rect> {
    let r = intersect(a, b);
    (!is_empty(&r)).then_some(r)
}

fn is_empty(r: &Rect) -> bool {
    r.w <= 0.0 || r.h <= 0.0
}

pub(crate) fn hsla(c: Color, alpha: f32) -> Hsla {
    Rgba {
        r: c.r,
        g: c.g,
        b: c.b,
        a: (c.a * alpha).clamp(0.0, 1.0),
    }
    .into()
}

fn corners(r: [f32; 4]) -> Corners<Pixels> {
    Corners {
        top_left: px(r[0]),
        top_right: px(r[1]),
        bottom_right: px(r[2]),
        bottom_left: px(r[3]),
    }
}

/// Screen-space endpoints of a local linear gradient under the affine `s`. For non-conformal
/// transforms the gradient direction is `A^-T d` (isolines stay parallel), not `A d`.
pub fn screen_linear(s: &Affine2, start: Vec2, end: Vec2) -> (Vec2, Vec2) {
    let d = end - start;
    let l2 = d.length_squared();
    let p0 = s.transform_point2(start);
    if l2 < 1e-12 || s.matrix2.determinant().abs() < 1e-12 {
        return (p0, s.transform_point2(end));
    }
    let g = s.matrix2.inverse().transpose() * (d / l2);
    let g2 = g.length_squared();
    (p0, p0 + g / g2)
}

// ---------------------------------------------------------------------------------------------
// Paint target

/// The GPUI calls the painter makes. Implemented by [`Window`]; tests implement it with a
/// recorder to check which primitives a frame emits without opening a window.
pub(crate) trait Surface {
    fn paint_quad(&mut self, quad: PaintQuad);
    fn paint_drop_shadows(&mut self, b: Bounds<Pixels>, c: Corners<Pixels>, s: &[BoxShadow]);
    fn paint_inset_shadows(&mut self, b: Bounds<Pixels>, c: Corners<Pixels>, s: &[BoxShadow]);
    fn paint_path(&mut self, path: Path<Pixels>, bg: Background);
    fn paint_image(
        &mut self,
        bounds: Bounds<Pixels>,
        image_bounds: Bounds<Pixels>,
        radii: Corners<Pixels>,
        image: Arc<RenderImage>,
    );
    fn paint_glyph(&mut self, o: Point<Pixels>, f: FontId, g: GlyphId, size: Pixels, c: Hsla);
    fn paint_emoji(&mut self, o: Point<Pixels>, f: FontId, g: GlyphId, size: Pixels);
    fn drop_image(&mut self, image: Arc<RenderImage>);
    fn content_mask(&self) -> ContentMask<Pixels>;
    fn with_content_mask<R>(
        &mut self,
        mask: Option<ContentMask<Pixels>>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R;
    fn paint_layer<R>(&mut self, bounds: Bounds<Pixels>, f: impl FnOnce(&mut Self) -> R) -> R;
    /// Shape every line of a text block (see [`shape_block`]).
    fn shape_block(&mut self, block: &TextBlock) -> Vec<TextLineShape>;
}

impl Surface for Window {
    fn paint_quad(&mut self, quad: PaintQuad) {
        Window::paint_quad(self, quad)
    }
    fn paint_drop_shadows(&mut self, b: Bounds<Pixels>, c: Corners<Pixels>, s: &[BoxShadow]) {
        Window::paint_drop_shadows(self, b, c, s)
    }
    fn paint_inset_shadows(&mut self, b: Bounds<Pixels>, c: Corners<Pixels>, s: &[BoxShadow]) {
        Window::paint_inset_shadows(self, b, c, s)
    }
    fn paint_path(&mut self, path: Path<Pixels>, bg: Background) {
        Window::paint_path(self, path, bg)
    }
    fn paint_image(
        &mut self,
        bounds: Bounds<Pixels>,
        image_bounds: Bounds<Pixels>,
        radii: Corners<Pixels>,
        image: Arc<RenderImage>,
    ) {
        let _ = Window::paint_image(self, bounds, image_bounds, radii, image, 0, false);
    }
    fn paint_glyph(&mut self, o: Point<Pixels>, f: FontId, g: GlyphId, size: Pixels, c: Hsla) {
        let _ = Window::paint_glyph(self, o, f, g, size, c);
    }
    fn paint_emoji(&mut self, o: Point<Pixels>, f: FontId, g: GlyphId, size: Pixels) {
        let _ = Window::paint_emoji(self, o, f, g, size);
    }
    fn drop_image(&mut self, image: Arc<RenderImage>) {
        let _ = Window::drop_image(self, image);
    }
    fn content_mask(&self) -> ContentMask<Pixels> {
        Window::content_mask(self)
    }
    fn with_content_mask<R>(
        &mut self,
        mask: Option<ContentMask<Pixels>>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        Window::with_content_mask(self, mask, f)
    }
    fn paint_layer<R>(&mut self, bounds: Bounds<Pixels>, f: impl FnOnce(&mut Self) -> R) -> R {
        Window::paint_layer(self, bounds, f)
    }
    fn shape_block(&mut self, block: &TextBlock) -> Vec<TextLineShape> {
        shape_block(self.text_system(), block)
    }
}

// ---------------------------------------------------------------------------------------------
// Prepared per-chunk data

#[derive(Default)]
pub(crate) struct TextSeg {
    layout: Arc<LineLayout>,
    x: f32,
    run: usize,
}

#[derive(Default)]
pub(crate) struct TextLineShape {
    segs: Vec<TextSeg>,
    width: f32,
}

#[derive(Default)]
pub(crate) struct PreparedItem {
    /// Doc-space bounds (None for scope items).
    bounds: Option<Rect>,
    fill: Option<Tess>,
    stroke: Option<Tess>,
    text: Option<Vec<TextLineShape>>,
    /// Clipped rect + radii (doc space) when a rounded parent clip is drawn natively.
    adopt: Option<(Rect, [f32; 4])>,
}

pub(crate) struct PreparedChunk {
    /// The chunk this data was prepared from. Holding the `Arc` pins its identity: the data is
    /// valid exactly while the list holds this same allocation (`Arc::ptr_eq`). `(root,
    /// version)` alone is not a safe key: versions are counters of one display-list builder,
    /// so a list from another builder (a reloaded document, another pipeline, a test) can
    /// reuse a pair for different content.
    source: Arc<pen_render::Chunk>,
    /// Canvas-local generation, unique per prepared chunk: the key of every texture derived
    /// from it (fallback scopes, gradients) instead of the builder's `version`.
    pub(crate) epoch: u64,
    items: Vec<PreparedItem>,
    /// Fallback scopes, clip adoptions and draw-order segments.
    pub(crate) plan: ChunkPlan,
}

impl PreparedChunk {
    fn new(source: &Arc<pen_render::Chunk>, epoch: u64) -> Self {
        let items = &source.items;
        let bounds: Vec<Option<Rect>> = items.iter().map(geom::item_doc_bounds).collect();
        let plan = fallback::analyze(items, &bounds);
        let mut prepared: Vec<PreparedItem> = bounds
            .into_iter()
            .map(|bounds| PreparedItem {
                bounds,
                ..Default::default()
            })
            .collect();
        for a in &plan.adopt {
            if let Some(p) = prepared.get_mut(a.item as usize) {
                p.adopt = Some((a.rect, a.radii));
            }
        }
        Self {
            source: source.clone(),
            epoch,
            items: prepared,
            plan,
        }
    }

    /// True if this data was prepared from `chunk` (the same allocation).
    pub(crate) fn is_for(&self, chunk: &Arc<pen_render::Chunk>) -> bool {
        Arc::ptr_eq(&self.source, chunk)
    }
}

// ---------------------------------------------------------------------------------------------
// Texture caches

struct CachedImage {
    image: Arc<RenderImage>,
    last_used: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GradKey {
    root: NodeKey,
    version: u64,
    item: u32,
    fill: u16,
    alpha: u8,
}

/// Largest texture side we upload; bigger images are downsampled once.
const MAX_IMAGE_SIDE: u32 = 4096;
/// Entries unused for this many frames are released from the GPU atlas.
const EVICT_AFTER_FRAMES: u64 = 600;

#[derive(Default)]
pub(crate) struct TextureCache {
    images: HashMap<(u64, u8), CachedImage>,
    gradients: HashMap<GradKey, CachedImage>,
    avg: HashMap<u64, Color>,
    frame: u64,
}

fn alpha_bucket(a: f32) -> u8 {
    (a.clamp(0.0, 1.0) * 64.0).round() as u8
}

pub(crate) fn render_image_from_bgra(w: u32, h: u32, bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    let buf = image::RgbaImage::from_raw(w, h, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

/// Premultiplied RGBA → straight BGRA with `alpha` applied, downsampled to fit `max_side`.
pub fn image_to_bgra(data: &ImageData, alpha: f32, max_side: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (data.width.max(1), data.height.max(1));
    let step = w.max(h).div_ceil(max_side).max(1);
    let (ow, oh) = (w.div_ceil(step), h.div_ceil(step));
    let mut out = Vec::with_capacity((ow * oh * 4) as usize);
    let src = &data.rgba_premul;
    for y in 0..oh {
        for x in 0..ow {
            let i = (((y * step).min(h - 1) * w + (x * step).min(w - 1)) * 4) as usize;
            let px = src.get(i..i + 4).unwrap_or(&[0, 0, 0, 0]);
            let a = px[3];
            let un = |c: u8| {
                if a == 0 {
                    0
                } else {
                    ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                }
            };
            out.extend_from_slice(&[
                un(px[2]),
                un(px[1]),
                un(px[0]),
                (a as f32 * alpha).round() as u8,
            ]);
        }
    }
    (ow, oh, out)
}

/// Mean (straight) color of an image, sampling at most ~4k pixels.
pub fn image_average(data: &ImageData) -> Color {
    let n = (data.width as usize * data.height as usize).max(1);
    let stride = (n / 4096).max(1);
    let mut acc = [0f64; 4];
    let mut count = 0f64;
    for px in data.rgba_premul.as_chunks::<4>().0.iter().step_by(stride) {
        for (a, v) in acc.iter_mut().zip(px) {
            *a += *v as f64;
        }
        count += 1.0;
    }
    if count == 0.0 || acc[3] == 0.0 {
        return Color::TRANSPARENT;
    }
    // Premultiplied sums: divide color by alpha sum.
    Color::rgba(
        (acc[0] / acc[3]) as f32,
        (acc[1] / acc[3]) as f32,
        (acc[2] / acc[3]) as f32,
        (acc[3] / count / 255.0) as f32,
    )
}

impl TextureCache {
    fn image(&mut self, data: &ImageData, alpha: f32) -> Option<Arc<RenderImage>> {
        let key = (data.id, alpha_bucket(alpha));
        let frame = self.frame;
        if let Some(e) = self.images.get_mut(&key) {
            e.last_used = frame;
            return Some(e.image.clone());
        }
        let (w, h, bgra) = image_to_bgra(data, key.1 as f32 / 64.0, MAX_IMAGE_SIDE);
        let image = render_image_from_bgra(w, h, bgra)?;
        self.images.insert(
            key,
            CachedImage {
                image: image.clone(),
                last_used: frame,
            },
        );
        Some(image)
    }

    fn average(&mut self, data: &ImageData) -> Color {
        *self
            .avg
            .entry(data.id)
            .or_insert_with(|| image_average(data))
    }

    fn gradient(
        &mut self,
        key: GradKey,
        make: impl FnOnce() -> (u32, u32, Vec<u8>),
    ) -> Option<Arc<RenderImage>> {
        let frame = self.frame;
        if let Some(e) = self.gradients.get_mut(&key) {
            e.last_used = frame;
            return Some(e.image.clone());
        }
        let (w, h, bgra) = make();
        let image = render_image_from_bgra(w, h, bgra)?;
        self.gradients.insert(
            key,
            CachedImage {
                image: image.clone(),
                last_used: frame,
            },
        );
        Some(image)
    }

    pub(crate) fn begin_frame(&mut self, window: &mut Window) {
        self.frame += 1;
        if !self.frame.is_multiple_of(120) {
            return;
        }
        let frame = self.frame;
        let mut dropped = Vec::new();
        self.images.retain(|_, e| {
            let keep = frame - e.last_used < EVICT_AFTER_FRAMES;
            if !keep {
                dropped.push(e.image.clone());
            }
            keep
        });
        self.gradients.retain(|_, e| {
            let keep = frame - e.last_used < EVICT_AFTER_FRAMES;
            if !keep {
                dropped.push(e.image.clone());
            }
            keep
        });
        for img in dropped {
            let _ = window.drop_image(img);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Render cache

/// A fallback texture the painter wants rendered (dispatched by the canvas view).
#[derive(Clone, Copy)]
pub(crate) struct FallbackRequest {
    pub key: TexKey,
    pub chunk: usize,
    pub start: usize,
    pub end: usize,
    pub kind: FallbackKind,
    pub region: Rect,
    pub scale: f32,
    pub alpha: f32,
    pub background: Color,
    /// Something (a stale bucket) is already drawn in its place.
    pub has_standin: bool,
}

#[derive(Default)]
pub(crate) struct RenderCache {
    /// Prepared chunks keyed by the address of their `Arc<Chunk>` (kept alive by
    /// [`PreparedChunk::source`], so an address is never reused while its entry exists).
    chunks: HashMap<usize, PreparedChunk>,
    next_epoch: u64,
    pub(crate) textures: TextureCache,
    pub(crate) fallback: FallbackCache<Arc<RenderImage>>,
    pub(crate) requests: Vec<FallbackRequest>,
    visible: Vec<usize>,
    alpha_stack: Vec<f32>,
    clip_stack: Vec<Rect>,
    shadows: Vec<BoxShadow>,
}

impl RenderCache {
    /// Forget prepared data (and the fallback textures derived from it) for chunks that are
    /// not in `list` any more. A chunk that was replaced (a new `Arc`, whatever its version)
    /// counts as gone.
    pub(crate) fn retain_roots(&mut self, list: &pen_render::DisplayList) {
        let live: std::collections::HashSet<usize> = list.chunks.iter().map(chunk_addr).collect();
        self.chunks.retain(|k, _| live.contains(k));
        let epochs: std::collections::HashSet<(NodeKey, u64)> = self
            .chunks
            .values()
            .map(|p| (p.source.root, p.epoch))
            .collect();
        self.fallback
            .retain_versions(|root, epoch| epochs.contains(&(root, epoch)));
    }

    pub(crate) fn clear(&mut self) {
        self.chunks.clear();
        self.fallback.clear();
    }

    /// Store a finished fallback texture if its chunk version is still current.
    pub(crate) fn finish_fallback(
        &mut self,
        key: TexKey,
        out: Option<(Rect, Arc<RenderImage>, usize)>,
    ) {
        let current = self
            .chunks
            .values()
            .any(|p| p.epoch == key.id.version && p.source.root == key.id.root);
        match out {
            Some((rect, image, bytes)) if current => self.fallback.insert(key, rect, image, bytes),
            _ => self.fallback.cancel_pending(&key),
        }
    }
}

fn chunk_addr(c: &Arc<pen_render::Chunk>) -> usize {
    Arc::as_ptr(c) as usize
}

/// Per-frame inputs.
pub(crate) struct FrameParams {
    /// Canvas bounds in window coordinates.
    pub canvas: Rect,
    /// Document → window transform.
    pub doc_to_window: Affine2,
    pub zoom: f32,
    pub policy: LodPolicy,
    /// Device pixels per logical pixel.
    pub scale_factor: f32,
    /// Canvas background (backdrop fallback textures are rendered over it).
    pub background: Color,
    pub raster_fallback: bool,
    pub limits: RegionLimits,
}

/// Paint all visible chunks of `list`. Must be called during the GPUI paint phase.
pub(crate) fn paint_list<S: Surface>(
    list: &pen_render::DisplayList,
    cache: &mut RenderCache,
    params: &FrameParams,
    window: &mut S,
    stats: &mut FrameStats,
) {
    let RenderCache {
        chunks,
        next_epoch,
        textures,
        fallback,
        requests,
        visible,
        alpha_stack,
        clip_stack,
        shadows,
    } = cache;
    fallback.begin_frame();
    for img in fallback.take_evicted() {
        window.drop_image(img);
    }
    let inv = params.doc_to_window.inverse();
    let view_doc = params.canvas.transformed(&inv);
    crate::lod::visible_chunks(list, &view_doc, params.zoom, &params.policy, visible);
    stats.chunks_total = list.chunks.len() as u32;
    stats.chunks_visible = visible.len() as u32;
    let mut painter = Painter {
        textures,
        fallback,
        requests,
        list,
        chunk_index: 0,
        alpha: 1.0,
        alpha_stack,
        clip_stack,
        skip_until: 0,
        scope_cursor: 0,
        in_layer: false,
        shadows,
        stats,
        params,
        view_doc,
        root: NodeKey(0),
        epoch: 0,
    };
    let layered = params.policy.flatten_chunks(params.zoom);
    for &ci in visible.iter() {
        let chunk = &list.chunks[ci];
        let prep = chunks.entry(chunk_addr(chunk)).or_insert_with(|| {
            *next_epoch += 1;
            painter.stats.chunks_prepared += 1;
            PreparedChunk::new(chunk, *next_epoch)
        });
        debug_assert!(prep.is_for(chunk));
        painter.root = chunk.root;
        painter.epoch = prep.epoch;
        painter.chunk_index = ci;
        painter.alpha = 1.0;
        painter.alpha_stack.clear();
        painter.clip_stack.clear();
        painter.skip_until = 0;
        painter.scope_cursor = 0;
        let PreparedChunk { items, plan, .. } = prep;
        if layered {
            // Strictly below the bar threshold of the largest text (with a margin for the
            // font-size quantization), no glyph is drawn: use the bar segmentation.
            let bars = plan.max_text_size * params.zoom * 1.01 < params.policy.text_bar_px;
            let segments = if bars {
                &plan.segments_bars
            } else {
                &plan.segments
            };
            for seg in segments {
                match seg {
                    Segment::Layer { bounds, .. } => {
                        painter.in_layer = true;
                        painter.stats.layers += 1;
                        let b = bounds_of(&bounds.transformed(&params.doc_to_window));
                        window.paint_layer(b, |w| {
                            painter.paint_range(&chunk.items, items, plan, seg.range(), w)
                        });
                    }
                    Segment::Direct { .. } => {
                        painter.in_layer = false;
                        painter.stats.direct_segments += 1;
                        painter.paint_range(&chunk.items, items, plan, seg.range(), window);
                    }
                }
            }
        } else {
            painter.in_layer = false;
            painter.paint_range(&chunk.items, items, plan, 0..chunk.items.len(), window);
        }
    }
    painter.stats.fallback_bytes = painter.fallback.bytes() as u64;
}

struct Painter<'a> {
    textures: &'a mut TextureCache,
    fallback: &'a mut FallbackCache<Arc<RenderImage>>,
    requests: &'a mut Vec<FallbackRequest>,
    list: &'a pen_render::DisplayList,
    chunk_index: usize,
    alpha: f32,
    alpha_stack: &'a mut Vec<f32>,
    /// Screen-space content masks of the open clips (already intersected).
    clip_stack: &'a mut Vec<Rect>,
    /// Items before this index are skipped (culled scopes / scopes drawn from a texture).
    skip_until: usize,
    scope_cursor: usize,
    /// Painting inside a chunk segment layer (glyphs must not open nested layers).
    in_layer: bool,
    shadows: &'a mut Vec<BoxShadow>,
    stats: &'a mut FrameStats,
    params: &'a FrameParams,
    view_doc: Rect,
    root: NodeKey,
    /// Canvas-local generation of the chunk being painted (see [`PreparedChunk::epoch`]).
    epoch: u64,
}

/// Index just past the scope closing the push at `items[start - 1]`.
fn skip_scope(items: &[DisplayItem], start: usize, clip: bool) -> usize {
    let mut depth = 1;
    let mut i = start;
    while i < items.len() {
        match (&items[i], clip) {
            (DisplayItem::PushClip { .. }, true) | (DisplayItem::PushLayer { .. }, false) => {
                depth += 1
            }
            (DisplayItem::PopClip, true) | (DisplayItem::PopLayer, false) => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    items.len()
}

impl Painter<'_> {
    /// Paint `items[range]`. Clip / alpha state lives in the painter, so a range may start or
    /// end inside a scope (segments of one chunk are painted one after another).
    fn paint_range<S: Surface>(
        &mut self,
        items: &[DisplayItem],
        prep: &mut [PreparedItem],
        plan: &ChunkPlan,
        range: std::ops::Range<usize>,
        window: &mut S,
    ) {
        for idx in range {
            if idx < self.skip_until {
                continue;
            }
            while plan
                .scopes
                .get(self.scope_cursor)
                .is_some_and(|s| (s.start as usize) < idx)
            {
                self.scope_cursor += 1;
            }
            if let Some(sc) = plan.scopes.get(self.scope_cursor)
                && sc.start as usize == idx
            {
                self.scope_cursor += 1;
                if self.paint_fallback(sc, window) {
                    self.skip_until = sc.end as usize;
                    continue;
                }
                self.stats.approximated += 1;
            }
            match &items[idx] {
                DisplayItem::PushClip { transform, shape } => {
                    let s = self.params.doc_to_window * *transform;
                    let r = geom::shape_local_bounds(shape).transformed(&s);
                    let mask = intersect(&r, &self.mask());
                    if is_empty(&mask) {
                        self.skip_until = skip_scope(items, idx + 1, true);
                        continue;
                    }
                    self.clip_stack.push(mask);
                }
                DisplayItem::PopClip => {
                    self.clip_stack.pop();
                }
                DisplayItem::PushLayer { opacity, .. } => {
                    let a = self.alpha * opacity.clamp(0.0, 1.0);
                    if a < 1.0 / 512.0 {
                        self.skip_until = skip_scope(items, idx + 1, false);
                        continue;
                    }
                    self.alpha_stack.push(self.alpha);
                    self.alpha = a;
                }
                DisplayItem::PopLayer => {
                    if let Some(a) = self.alpha_stack.pop() {
                        self.alpha = a;
                    }
                }
                item @ (DisplayItem::Shape { .. } | DisplayItem::Text { .. }) => {
                    if !self.visible(prep[idx].bounds.as_ref()) {
                        self.stats.items_culled += 1;
                        continue;
                    }
                    self.stats.items_drawn += 1;
                    match self.clip_stack.last() {
                        Some(m) => {
                            let mask = ContentMask {
                                bounds: bounds_of(m),
                            };
                            window.with_content_mask(Some(mask), |w| {
                                self.paint_item(w, idx, &mut prep[idx], item)
                            })
                        }
                        None => self.paint_item(window, idx, &mut prep[idx], item),
                    }
                }
            }
        }
    }

    /// Current clip mask in window coordinates.
    fn mask(&self) -> Rect {
        self.clip_stack
            .last()
            .copied()
            .unwrap_or(self.params.canvas)
    }

    fn paint_item<S: Surface>(
        &mut self,
        window: &mut S,
        idx: usize,
        prep: &mut PreparedItem,
        item: &DisplayItem,
    ) {
        match item {
            DisplayItem::Shape {
                transform,
                shape,
                fills,
                stroke,
                shadows,
                ..
            } => {
                let s = self.params.doc_to_window * *transform;
                let t0 = Instant::now();
                self.paint_shape(
                    window,
                    idx,
                    prep,
                    &s,
                    transform,
                    shape,
                    fills,
                    stroke.as_ref(),
                    shadows,
                );
                self.stats.shape_time += t0.elapsed();
            }
            DisplayItem::Text {
                transform, block, ..
            } => {
                let s = self.params.doc_to_window * *transform;
                let t0 = Instant::now();
                self.paint_text(window, prep, &s, block);
                self.stats.text_time += t0.elapsed();
            }
            _ => {}
        }
    }

    /// Draw a fallback scope from its CPU texture (exact bucket, else the nearest cached one)
    /// and request the missing texture. `false` = draw the GPU approximation instead.
    fn paint_fallback<S: Surface>(&mut self, sc: &FallbackScope, window: &mut S) -> bool {
        self.stats.fallback_scopes += 1;
        let p = self.params;
        if !p.raster_fallback {
            return false;
        }
        let Some(need) = intersect_opt(&sc.bounds, &self.view_doc) else {
            // Off screen: nothing to draw either way.
            return true;
        };
        if sc.bounds.w.max(sc.bounds.h) * p.zoom < p.policy.fallback_min_px {
            return false;
        }
        let Some(region) = fallback::plan_region(
            &sc.bounds,
            &self.view_doc,
            p.zoom * p.scale_factor,
            &p.limits,
        ) else {
            return false;
        };
        let (alpha, env) = match sc.kind {
            FallbackKind::Isolated => (alpha_bucket(self.alpha), 0),
            FallbackKind::Backdrop => (
                64,
                fallback::backdrop_env(self.list, self.chunk_index, &sc.bounds, p.background),
            ),
        };
        let key = TexKey {
            id: ScopeId {
                root: self.root,
                version: self.epoch,
                scope: sc.start,
            },
            bucket: region.bucket,
            alpha,
            env,
            cells: region.cells,
        };
        let hit = self.fallback.get(&key);
        let exact = hit.is_some();
        let shown = hit.or_else(|| self.fallback.nearest(&key, &need));
        if !exact && !self.fallback.is_pending(&key) {
            self.requests.push(FallbackRequest {
                key,
                chunk: self.chunk_index,
                start: sc.start as usize,
                end: sc.end as usize,
                kind: sc.kind,
                region: region.doc,
                scale: region.scale,
                alpha: alpha as f32 / 64.0,
                background: p.background,
                has_standin: shown.is_some(),
            });
        }
        let Some((rect, image)) = shown else {
            self.stats.fallback_waiting += 1;
            return false;
        };
        if exact {
            self.stats.fallback_drawn += 1;
        } else {
            self.stats.fallback_standin += 1;
        }
        let b = bounds_of(&rect.transformed(&p.doc_to_window));
        let draw = |w: &mut S| {
            w.paint_image(b, b, Corners::default(), image);
        };
        match self.clip_stack.last() {
            Some(m) => window.with_content_mask(
                Some(ContentMask {
                    bounds: bounds_of(m),
                }),
                draw,
            ),
            None => draw(window),
        }
        self.stats.images += 1;
        true
    }

    fn visible(&self, b: Option<&Rect>) -> bool {
        match b {
            Some(b) => {
                b.intersects(&self.view_doc) && !self.params.policy.too_small(b, self.params.zoom)
            }
            None => false,
        }
    }

    // -----------------------------------------------------------------------------------------
    // Shapes

    #[allow(clippy::too_many_arguments)]
    fn paint_shape<S: Surface>(
        &mut self,
        window: &mut S,
        idx: usize,
        prep: &mut PreparedItem,
        s: &Affine2,
        doc_t: &Affine2,
        shape: &Shape,
        fills: &[Paint],
        stroke: Option<&Stroke>,
        shadows: &[Shadow],
    ) {
        let scale = geom::transform_scale(s);
        let draw_shadows = !shadows.is_empty() && self.params.policy.draw_shadows(self.params.zoom);
        let quad_geom = geom::quad_geom(s, shape);

        if let Some((full, full_radii)) = quad_geom {
            // A rounded parent clip adopted natively: draw the clipped rect with its radii.
            let (rect, radii) = match prep.adopt {
                Some((q, rq)) => {
                    let k = self.params.zoom;
                    (q.transformed(&self.params.doc_to_window), rq.map(|r| r * k))
                }
                None => (full, full_radii),
            };
            let b = bounds_of(&rect);
            let c = corners(radii);
            if draw_shadows {
                self.collect_shadows(s, scale, shadows, false);
                if !self.shadows.is_empty() {
                    window.paint_drop_shadows(b, c, self.shadows);
                }
            }
            for (fi, paint) in fills.iter().enumerate() {
                self.paint_quad_fill(window, idx, fi, &rect, &full, &radii, doc_t, shape, paint);
            }
            if draw_shadows {
                self.collect_shadows(s, scale, shadows, true);
                if !self.shadows.is_empty() {
                    window.paint_inset_shadows(b, c, self.shadows);
                }
            }
            if let Some(st) = stroke {
                let diagonal = s.matrix2.x_axis.y.abs() < 1e-6 && s.matrix2.y_axis.x.abs() < 1e-6;
                let solid = st.paints.iter().all(|p| matches!(p, Paint::Solid(_)));
                if diagonal && solid && !st.dash.iter().any(|d| *d > 0.0) {
                    self.paint_quad_stroke(window, s, &rect, &radii, st);
                } else {
                    self.paint_path_stroke(window, prep, s, scale, shape, st);
                }
            }
            return;
        }

        // Tiny non-rect shapes: a quad over the bounds with coverage-reduced alpha.
        if let Some(db) = prep.bounds
            && self.params.policy.path_as_quad(&db, self.params.zoom)
        {
            let r = geom::shape_local_bounds(shape).transformed(s);
            let paint = fills
                .first()
                .or_else(|| stroke.and_then(|st| st.paints.first()));
            if let Some(paint) = paint {
                let c = self.flat_color(paint);
                let coverage = if fills.is_empty() { 0.35 } else { 0.7 };
                self.stats.quads += 1;
                window.paint_quad(gpui_kit::fill(
                    bounds_of(&r),
                    hsla(c, self.alpha * coverage),
                ));
            }
            return;
        }

        // Path route.
        let tol_ok = prep
            .fill
            .as_ref()
            .is_some_and(|t| geom::tess_valid(t.tol, scale));
        if !tol_ok && (!fills.is_empty() || draw_shadows) {
            let tol = geom::pick_tolerance(scale);
            let t = prep.fill.get_or_insert_with(Tess::default);
            geom::tessellate_fill(
                &geom::lyon_path(shape, 0.0),
                geom::fill_rule_of(shape),
                tol,
                &mut t.tris,
            );
            t.tol = tol;
            self.stats.retessellated += 1;
        }
        if draw_shadows {
            let alpha = self.alpha;
            for sh in shadows {
                match sh {
                    Shadow::Drop {
                        offset,
                        blur,
                        spread,
                        color,
                    } if *blur <= 0.0 && *spread == 0.0 => {
                        let o = s.matrix2 * *offset;
                        let s2 = Affine2::from_translation(o) * *s;
                        if let Some(tess) = prep.fill.as_ref() {
                            self.emit_path(window, &tess.tris, &s2, |_| {
                                Some(hsla(*color, alpha).into())
                            });
                        }
                    }
                    _ => self.stats.approximated += 1,
                }
            }
        }
        for paint in fills {
            let bg = self.path_background(paint, s);
            if let Some(tess) = prep.fill.as_ref() {
                let alpha = self.alpha;
                self.emit_path(window, &tess.tris, s, |clip| bg.resolve(clip, alpha));
            }
        }
        if let Some(st) = stroke {
            self.paint_path_stroke(window, prep, s, scale, shape, st);
        }
    }

    /// Fill `self.shadows` with the drop (`inset == false`) or inner shadows, in screen units.
    fn collect_shadows(&mut self, s: &Affine2, scale: f32, shadows: &[Shadow], inset: bool) {
        self.shadows.clear();
        let min_px = self.params.policy.min_shadow_px;
        for sh in shadows {
            let (offset, blur, spread, color, is_inset) = match sh {
                Shadow::Drop {
                    offset,
                    blur,
                    spread,
                    color,
                } => (offset, blur, spread, color, false),
                Shadow::Inner {
                    offset,
                    blur,
                    spread,
                    color,
                } => (offset, blur, spread, color, true),
            };
            if is_inset != inset || color.a * self.alpha <= 0.0 {
                continue;
            }
            let o = s.matrix2 * *offset;
            if (o.length() + (blur.abs() + spread.abs()) * scale) < min_px {
                continue;
            }
            self.stats.shadows += 1;
            self.shadows.push(BoxShadow {
                color: hsla(*color, self.alpha),
                offset: point(px(o.x), px(o.y)),
                // Design-tool blur is ~2σ (CSS); GPUI's blur_radius is σ.
                blur_radius: px(blur.max(0.0) * scale * 0.5),
                spread_radius: px(spread * scale),
                inset,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_quad_fill<S: Surface>(
        &mut self,
        window: &mut S,
        idx: usize,
        fi: usize,
        rect: &Rect,
        full: &Rect,
        radii: &[f32; 4],
        doc_t: &Affine2,
        shape: &Shape,
        paint: &Paint,
    ) {
        let b = bounds_of(rect);
        let c = corners(*radii);
        let alpha = self.alpha;
        match paint {
            Paint::Solid(col) => {
                self.stats.quads += 1;
                window.paint_quad(quad(
                    b,
                    c,
                    hsla(*col, alpha),
                    Edges::default(),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
            }
            Paint::Linear { start, end, stops } if stops.len() <= 2 => {
                let s = self.params.doc_to_window * *doc_t;
                let (p0, p1) = screen_linear(&s, *start, *end);
                let bg = linear_background(rect, p0, p1, stops, alpha);
                self.stats.quads += 1;
                window.paint_quad(quad(
                    b,
                    c,
                    bg,
                    Edges::default(),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
            }
            Paint::Linear { .. } | Paint::Radial { .. } | Paint::Angular { .. } => {
                // Multi-stop / radial / conic: bake a texture once (doc space) and draw it as an
                // image clipped by the rounded rect.
                let local = geom::shape_local_bounds(shape);
                let doc_rect = local.transformed(doc_t);
                let to_local = doc_t.inverse();
                let key = GradKey {
                    root: self.root,
                    version: self.epoch,
                    item: idx as u32,
                    fill: fi as u16,
                    alpha: alpha_bucket(alpha),
                };
                let a = key.alpha as f32 / 64.0;
                let img = self.textures.gradient(key, || {
                    let (w, h) = geom::gradient_texture_size(local.w, local.h);
                    (
                        w,
                        h,
                        geom::gradient_texture(paint, &doc_rect, &to_local, w, h, a),
                    )
                });
                if let Some(img) = img {
                    self.stats.images += 1;
                    window.paint_image(b, bounds_of(full), c, img);
                }
            }
            Paint::Image {
                image,
                fit,
                opacity,
                scale,
            } => {
                let a = alpha * opacity.clamp(0.0, 1.0);
                let Some(img) = self.textures.image(image, a) else {
                    return;
                };
                let (iw, ih) = (image.width as f32, image.height as f32);
                let zoom = geom::transform_scale(&(self.params.doc_to_window * *doc_t));
                if *fit == ImageFit::Tile {
                    let tile = geom::image_fit_rect(full, iw, ih, ImageFit::Tile, *scale, zoom);
                    let mask = intersect(rect, &rect_of(&window.content_mask().bounds));
                    if tile.w < 1.0 || tile.h < 1.0 || is_empty(&mask) {
                        let avg = self.textures.average(image);
                        window.paint_quad(quad(
                            b,
                            c,
                            hsla(avg, a),
                            Edges::default(),
                            transparent_black(),
                            BorderStyle::Solid,
                        ));
                        self.stats.approximated += 1;
                        return;
                    }
                    let x0 = ((mask.x - rect.x) / tile.w).floor().max(0.0) as i32;
                    let y0 = ((mask.y - rect.y) / tile.h).floor().max(0.0) as i32;
                    let x1 = ((mask.right() - rect.x) / tile.w).ceil() as i32;
                    let y1 = ((mask.bottom() - rect.y) / tile.h).ceil() as i32;
                    let mut n = 0;
                    'tiles: for ty in y0..y1 {
                        for tx in x0..x1 {
                            let t = Rect::new(
                                rect.x + tx as f32 * tile.w,
                                rect.y + ty as f32 * tile.h,
                                tile.w,
                                tile.h,
                            );
                            window.paint_image(b, bounds_of(&t), Corners::default(), img.clone());
                            n += 1;
                            if n >= 4096 {
                                break 'tiles;
                            }
                        }
                    }
                    self.stats.images += n;
                } else {
                    let ib = geom::image_fit_rect(full, iw, ih, *fit, *scale, zoom);
                    self.stats.images += 1;
                    window.paint_image(b, bounds_of(&ib), c, img);
                }
            }
        }
    }

    fn paint_quad_stroke<S: Surface>(
        &mut self,
        window: &mut S,
        s: &Affine2,
        rect: &Rect,
        radii: &[f32; 4],
        st: &Stroke,
    ) {
        let (sx, sy) = (s.matrix2.x_axis.x, s.matrix2.y_axis.y);
        // widths: top, right, bottom, left (local) → screen, honoring flips.
        let [t, r, b, l] = st.widths;
        let (mut top, mut bottom) = (t * sy.abs(), b * sy.abs());
        let (mut left, mut right) = (l * sx.abs(), r * sx.abs());
        if sy < 0.0 {
            std::mem::swap(&mut top, &mut bottom);
        }
        if sx < 0.0 {
            std::mem::swap(&mut left, &mut right);
        }
        let k = match st.align {
            pen_render::StrokeAlign::Inside => 0.0,
            pen_render::StrokeAlign::Center => 0.5,
            pen_render::StrokeAlign::Outside => 1.0,
        };
        let outer = Rect::new(
            rect.x - left * k,
            rect.y - top * k,
            rect.w + (left + right) * k,
            rect.h + (top + bottom) * k,
        );
        let avg = (top + bottom + left + right) * 0.25 * k;
        let r = radii.map(|v| if v > 0.0 { v + avg } else { 0.0 });
        let edges = Edges {
            top: px(top),
            right: px(right),
            bottom: px(bottom),
            left: px(left),
        };
        for p in &st.paints {
            if let Paint::Solid(col) = p {
                self.stats.quads += 1;
                window.paint_quad(quad(
                    bounds_of(&outer),
                    corners(r),
                    transparent_black(),
                    edges,
                    hsla(*col, self.alpha),
                    BorderStyle::Solid,
                ));
            }
        }
    }

    fn paint_path_stroke<S: Surface>(
        &mut self,
        window: &mut S,
        prep: &mut PreparedItem,
        s: &Affine2,
        scale: f32,
        shape: &Shape,
        st: &Stroke,
    ) {
        let width = if st.is_uniform() {
            st.widths[0]
        } else {
            st.widths.iter().copied().fold(0.0, f32::max)
        };
        if width <= 0.0 || st.paints.is_empty() {
            return;
        }
        if !st.is_uniform() {
            self.stats.approximated += 1;
        }
        if !prep
            .stroke
            .as_ref()
            .is_some_and(|t| geom::tess_valid(t.tol, scale))
        {
            let tol = geom::pick_tolerance(scale);
            let t = prep.stroke.get_or_insert_with(Tess::default);
            let path = geom::lyon_path(shape, geom::stroke_grow(shape, st, width));
            geom::tessellate_stroke(&path, st, width, tol, &mut t.tris);
            t.tol = tol;
            self.stats.retessellated += 1;
        }
        let Some(tess) = prep.stroke.as_ref() else {
            return;
        };
        for p in &st.paints {
            let bg = self.path_background(p, s);
            let alpha = self.alpha;
            self.emit_path(window, &tess.tris, s, |clip| bg.resolve(clip, alpha));
        }
    }

    /// Single representative color of a paint (LOD / unsupported cases).
    fn flat_color(&mut self, paint: &Paint) -> Color {
        match paint {
            Paint::Solid(c) => *c,
            Paint::Linear { stops, .. }
            | Paint::Radial { stops, .. }
            | Paint::Angular { stops, .. } => geom::average_stop_color(stops),
            Paint::Image { image, opacity, .. } => {
                self.textures.average(image).with_alpha_mul(*opacity)
            }
        }
    }

    fn path_background(&mut self, paint: &Paint, s: &Affine2) -> PathPaint {
        match paint {
            Paint::Solid(c) => PathPaint::Solid(*c),
            Paint::Linear { start, end, stops } => {
                if stops.len() > 2 {
                    self.stats.approximated += 1;
                }
                let (p0, p1) = screen_linear(s, *start, *end);
                PathPaint::Linear {
                    p0,
                    p1,
                    stops: stops.clone(),
                }
            }
            Paint::Radial { stops, .. } | Paint::Angular { stops, .. } => {
                self.stats.approximated += 1;
                PathPaint::Solid(geom::average_stop_color(stops))
            }
            Paint::Image { image, opacity, .. } => {
                self.stats.approximated += 1;
                PathPaint::Solid(self.textures.average(image).with_alpha_mul(*opacity))
            }
        }
    }

    /// Transform cached local triangles by `s` and paint them as one GPUI path. `bg` gets the
    /// clipped screen bounds (GPUI evaluates path gradients over them).
    fn emit_path<S: Surface>(
        &mut self,
        window: &mut S,
        tris: &[Vec2],
        s: &Affine2,
        bg: impl FnOnce(&Rect) -> Option<Background>,
    ) {
        if tris.len() < 3 {
            return;
        }
        let p = |v: Vec2| {
            let q = s.transform_point2(v);
            point(px(q.x), px(q.y))
        };
        let t0 = Instant::now();
        let mut path = Path::new(p(tris[0]));
        path.vertices.reserve_exact(tris.len());
        let st = point(0., 1.);
        for [a, b, c] in tris.as_chunks::<3>().0 {
            path.push_triangle((p(*a), p(*b), p(*c)), (st, st, st));
        }
        let clip = intersect(
            &rect_of(&path.bounds),
            &rect_of(&window.content_mask().bounds),
        );
        if is_empty(&clip) {
            return;
        }
        if let Some(bg) = bg(&clip) {
            self.stats.paths += 1;
            self.stats.path_triangles += (tris.len() / 3) as u32;
            window.paint_path(path, bg);
        }
        self.stats.path_time += t0.elapsed();
    }

    // -----------------------------------------------------------------------------------------
    // Text

    fn paint_text<S: Surface>(
        &mut self,
        window: &mut S,
        prep: &mut PreparedItem,
        s: &Affine2,
        block: &TextBlock,
    ) {
        let scale = geom::transform_scale(s);
        let max_size = block.runs.iter().map(|r| r.style.size).fold(0.0, f32::max);
        if max_size <= 0.0 || block.lines.is_empty() {
            return;
        }
        let clip = if block.clip {
            let r = Rect::new(0.0, 0.0, block.size.x, block.size.y).transformed(s);
            Some(ContentMask {
                bounds: bounds_of(&r),
            })
        } else {
            None
        };
        if self.params.policy.text_as_bars(max_size * scale) {
            window.with_content_mask(clip, |w| self.paint_text_bars(w, s, block));
            return;
        }
        if prep.text.is_none() {
            prep.text = Some(window.shape_block(block));
            self.stats.lines_shaped += block.lines.len() as u32;
        }
        let Some(shaped) = prep.text.as_ref() else {
            return;
        };
        // All glyphs of a block share one draw order (one GPUI layer): they never need to be
        // ordered among themselves, and this skips a bounds-tree insert per glyph.
        // Inside a chunk segment layer the glyphs already share its order; a nested layer would
        // lift them above later items of the segment.
        let layer = prep
            .bounds
            .filter(|_| !self.in_layer)
            .map(|b| bounds_of(&b.transformed(&self.params.doc_to_window)));
        window.with_content_mask(clip, |w| match layer {
            Some(lb) => w.paint_layer(lb, |w| self.paint_glyphs(w, s, scale, block, shaped)),
            None => self.paint_glyphs(w, s, scale, block, shaped),
        });
    }

    fn text_color(&mut self, paint: &Paint) -> Hsla {
        let c = match paint {
            Paint::Solid(c) => *c,
            Paint::Linear { stops, .. }
            | Paint::Radial { stops, .. }
            | Paint::Angular { stops, .. } => {
                self.stats.approximated += 1;
                geom::average_stop_color(stops)
            }
            Paint::Image { image, .. } => {
                self.stats.approximated += 1;
                self.textures.average(image)
            }
        };
        hsla(c, self.alpha)
    }

    fn paint_text_bars<S: Surface>(&mut self, window: &mut S, s: &Affine2, block: &TextBlock) {
        let style = &block.runs[0].style;
        let color = self.text_color(&style.paint);
        let color = Hsla {
            a: color.a * 0.45,
            ..color
        };
        for line in &block.lines {
            if line.width <= 0.0 {
                continue;
            }
            let r = Rect::new(
                line.x,
                line.baseline - style.size * 0.62,
                line.width,
                style.size * 0.62,
            )
            .transformed(s);
            self.stats.text_bars += 1;
            window.paint_quad(gpui_kit::fill(bounds_of(&r), color));
        }
    }

    fn paint_glyphs<S: Surface>(
        &mut self,
        window: &mut S,
        s: &Affine2,
        scale: f32,
        block: &TextBlock,
        shaped: &[TextLineShape],
    ) {
        let mask = rect_of(&window.content_mask().bounds);
        let k = match block.align {
            TextAlign::Center => 0.5,
            TextAlign::Right => 1.0,
            TextAlign::Left | TextAlign::Justify => 0.0,
        };
        for (line, shape) in block.lines.iter().zip(shaped) {
            // Re-apply alignment with our own measured width (shaper metrics may differ).
            let x0 = line.x + (line.width - shape.width) * k;
            let base = s.transform_point2(Vec2::new(x0, line.baseline));
            let line_px = block.runs.first().map_or(0.0, |r| r.style.size) * scale;
            if base.y - line_px * 1.5 > mask.bottom() || base.y + line_px < mask.y {
                continue;
            }
            for seg in &shape.segs {
                let style = &block.runs[seg.run].style;
                let color = self.text_color(&style.paint);
                let font_px = px(geom::quantize_font_px(style.size * scale));
                let ls = style.letter_spacing;
                let mut gi = 0usize;
                for run in &seg.layout.runs {
                    for g in &run.glyphs {
                        let local = Vec2::new(
                            x0 + seg.x + g.position.x.as_f32() + ls * gi as f32,
                            line.baseline + g.position.y.as_f32(),
                        );
                        gi += 1;
                        let q = s.transform_point2(local);
                        if q.x > mask.right() + line_px {
                            continue;
                        }
                        let origin = point(px(q.x), px(q.y));
                        self.stats.glyphs += 1;
                        if g.is_emoji {
                            window.paint_emoji(origin, run.font_id, g.id, font_px)
                        } else {
                            window.paint_glyph(origin, run.font_id, g.id, font_px, color)
                        }
                    }
                }
                if style.underline || style.strikethrough {
                    let w = seg.layout.width.as_f32() + ls * gi as f32;
                    let th = (style.size / 14.0).max(1.0 / scale);
                    let mut deco = |y: f32| {
                        let r = Rect::new(x0 + seg.x, line.baseline + y, w, th).transformed(s);
                        window.paint_quad(gpui_kit::fill(bounds_of(&r), color));
                    };
                    if style.underline {
                        deco(style.size * 0.1);
                    }
                    if style.strikethrough {
                        deco(-style.size * 0.3);
                    }
                }
            }
        }
    }
}

/// Paint resolved for a path; gradients are mapped to GPUI parameters once the clipped
/// screen bounds are known.
enum PathPaint {
    Solid(Color),
    Linear {
        p0: Vec2,
        p1: Vec2,
        stops: smallvec::SmallVec<[pen_render::GradientStop; 4]>,
    },
}

impl PathPaint {
    fn resolve(&self, clip: &Rect, alpha: f32) -> Option<Background> {
        match self {
            PathPaint::Solid(c) => (c.a * alpha > 0.0).then(|| hsla(*c, alpha).into()),
            PathPaint::Linear { p0, p1, stops } => {
                Some(linear_background(clip, *p0, *p1, stops, alpha))
            }
        }
    }
}

/// Two-stop GPUI gradient over `bounds` (screen). More stops are approximated by the first
/// and last stop.
fn linear_background(
    bounds: &Rect,
    p0: Vec2,
    p1: Vec2,
    stops: &[pen_render::GradientStop],
    alpha: f32,
) -> Background {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return transparent_black().into();
    };
    match geom::linear_to_gpui(bounds, p0, p1, first.offset, last.offset) {
        Some(g) => linear_gradient(
            g.angle_deg,
            linear_color_stop(hsla(first.color, alpha), g.pct0),
            linear_color_stop(hsla(last.color, alpha), g.pct1),
        ),
        None => hsla(last.color, alpha).into(),
    }
}

/// Shape every line of a text block, one GPUI layout per (line, run) segment, at the
/// document font size. Glyph positions are scaled at paint time.
fn shape_block(ts: &WindowTextSystem, block: &TextBlock) -> Vec<TextLineShape> {
    let mut out = Vec::with_capacity(block.lines.len());
    for line in &block.lines {
        let mut shape = TextLineShape::default();
        for (ri, run) in block.runs.iter().enumerate() {
            let a = run.range.start.max(line.range.start);
            let b = run.range.end.min(line.range.end).min(block.text.len());
            if a >= b || !block.text.is_char_boundary(a) || !block.text.is_char_boundary(b) {
                continue;
            }
            let text = block.text[a..b].trim_end_matches(['\n', '\r']);
            if text.is_empty() || run.style.size <= 0.0 {
                continue;
            }
            let font = Font {
                family: SharedString::from(run.style.family.to_string()),
                features: FontFeatures::default(),
                fallbacks: None,
                weight: FontWeight(run.style.weight as f32),
                style: if run.style.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
            };
            let runs = [TextRun {
                len: text.len(),
                font,
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }];
            let layout = ts.layout_line(text, px(run.style.size), &runs, None);
            let glyphs: usize = layout.runs.iter().map(|r| r.glyphs.len()).sum();
            let w = layout.width.as_f32() + run.style.letter_spacing * glyphs as f32;
            shape.segs.push(TextSeg {
                layout,
                x: shape.width,
                run: ri,
            });
            shape.width += w;
        }
        out.push(shape);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pen_render::BlendMode;

    #[test]
    fn screen_linear_conformal_and_skewed() {
        let s = Affine2::from_scale_angle_translation(Vec2::splat(2.0), 0.0, Vec2::new(10.0, 0.0));
        let (a, b) = screen_linear(&s, Vec2::ZERO, Vec2::new(5.0, 0.0));
        assert!((a - Vec2::new(10.0, 0.0)).length() < 1e-4);
        assert!((b - Vec2::new(20.0, 0.0)).length() < 1e-4);
        // Non-uniform scale: a diagonal gradient's isolines must map to isolines.
        let s = Affine2::from_scale(Vec2::new(4.0, 1.0));
        let (start, end) = (Vec2::ZERO, Vec2::new(1.0, 1.0));
        let (p0, p1) = screen_linear(&s, start, end);
        let d = p1 - p0;
        for p in [Vec2::new(0.3, 0.1), Vec2::new(0.9, 0.7)] {
            let want = (p - start).dot(end - start) / (end - start).length_squared();
            let q = s.transform_point2(p);
            let got = (q - p0).dot(d) / d.length_squared();
            assert!((want - got).abs() < 1e-4);
        }
    }

    #[test]
    fn image_conversion_unpremultiplies() {
        let data = ImageData {
            id: 1,
            width: 2,
            height: 1,
            rgba_premul: vec![128, 0, 0, 128, 0, 0, 0, 0],
        };
        let (w, h, px) = image_to_bgra(&data, 1.0, 4096);
        assert_eq!((w, h), (2, 1));
        assert_eq!(&px[..4], &[0, 0, 255, 128]);
        assert_eq!(&px[4..], &[0, 0, 0, 0]);
        let (_, _, half) = image_to_bgra(&data, 0.5, 4096);
        assert_eq!(half[3], 64);
        // Downsample to fit.
        let big = ImageData {
            id: 2,
            width: 10,
            height: 4,
            rgba_premul: vec![255; 160],
        };
        let (w, h, px) = image_to_bgra(&big, 1.0, 5);
        assert_eq!((w, h, px.len()), (5, 2, 40));
        let avg = image_average(&data);
        assert!((avg.r - 1.0).abs() < 1e-3 && (avg.a - 0.25).abs() < 0.01);
    }

    #[test]
    fn scope_skipping() {
        let items = vec![
            DisplayItem::PushClip {
                transform: Affine2::IDENTITY,
                shape: Shape::Ellipse { size: Vec2::ONE },
            },
            DisplayItem::PushClip {
                transform: Affine2::IDENTITY,
                shape: Shape::Ellipse { size: Vec2::ONE },
            },
            DisplayItem::PopClip,
            DisplayItem::PushLayer {
                opacity: 0.5,
                blend: BlendMode::Normal,
                blur: 0.0,
                backdrop_blur: 0.0,
                bounds: Rect::default(),
            },
            DisplayItem::PopLayer,
            DisplayItem::PopClip,
            DisplayItem::PopLayer,
        ];
        assert_eq!(skip_scope(&items, 1, true), 6);
        assert_eq!(skip_scope(&items, 4, false), 5);
        assert_eq!(skip_scope(&items, 5, false), 7);
    }

    // -----------------------------------------------------------------------------------------
    // Frame recording (no window): per-chunk caches across display-list updates.

    use pen_render::{Chunk, CornerRadii, DisplayList, GradientStop, TextLine};
    use smallvec::smallvec;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Op {
        Quad,
        Drop(Rect, usize),
        Inset,
        Path,
        Image,
        Glyph,
    }

    #[derive(Default)]
    struct Recorder {
        ops: Vec<Op>,
        masks: Vec<ContentMask<Pixels>>,
        shaped: u32,
    }

    impl Surface for Recorder {
        fn paint_quad(&mut self, _: PaintQuad) {
            self.ops.push(Op::Quad);
        }
        fn paint_drop_shadows(&mut self, b: Bounds<Pixels>, _: Corners<Pixels>, s: &[BoxShadow]) {
            self.ops.push(Op::Drop(rect_of(&b), s.len()));
        }
        fn paint_inset_shadows(&mut self, _: Bounds<Pixels>, _: Corners<Pixels>, _: &[BoxShadow]) {
            self.ops.push(Op::Inset);
        }
        fn paint_path(&mut self, _: Path<Pixels>, _: Background) {
            self.ops.push(Op::Path);
        }
        fn paint_image(
            &mut self,
            _: Bounds<Pixels>,
            _: Bounds<Pixels>,
            _: Corners<Pixels>,
            _: Arc<RenderImage>,
        ) {
            self.ops.push(Op::Image);
        }
        fn paint_glyph(&mut self, _: Point<Pixels>, _: FontId, _: GlyphId, _: Pixels, _: Hsla) {
            self.ops.push(Op::Glyph);
        }
        fn paint_emoji(&mut self, _: Point<Pixels>, _: FontId, _: GlyphId, _: Pixels) {
            self.ops.push(Op::Glyph);
        }
        fn drop_image(&mut self, _: Arc<RenderImage>) {}
        fn content_mask(&self) -> ContentMask<Pixels> {
            self.masks.last().cloned().unwrap_or(ContentMask {
                bounds: bounds_of(&Rect::new(-1e6, -1e6, 2e6, 2e6)),
            })
        }
        fn with_content_mask<R>(
            &mut self,
            mask: Option<ContentMask<Pixels>>,
            f: impl FnOnce(&mut Self) -> R,
        ) -> R {
            let pushed = mask.is_some();
            if let Some(m) = mask {
                self.masks.push(m);
            }
            let r = f(self);
            if pushed {
                self.masks.pop();
            }
            r
        }
        fn paint_layer<R>(&mut self, _: Bounds<Pixels>, f: impl FnOnce(&mut Self) -> R) -> R {
            f(self)
        }
        fn shape_block(&mut self, block: &TextBlock) -> Vec<TextLineShape> {
            self.shaped += 1;
            block
                .lines
                .iter()
                .map(|_| TextLineShape::default())
                .collect()
        }
    }

    fn params(zoom: f32) -> FrameParams {
        FrameParams {
            canvas: Rect::new(0.0, 0.0, 1600.0, 1000.0),
            doc_to_window: Affine2::from_scale(Vec2::splat(zoom)),
            zoom,
            policy: LodPolicy::default(),
            scale_factor: 1.0,
            background: Color::rgba(1.0, 1.0, 1.0, 1.0),
            raster_fallback: true,
            limits: RegionLimits::default(),
        }
    }

    fn frame(list: &DisplayList, cache: &mut RenderCache, zoom: f32) -> (Recorder, FrameStats) {
        let mut rec = Recorder::default();
        let mut stats = FrameStats::default();
        paint_list(list, cache, &params(zoom), &mut rec, &mut stats);
        (rec, stats)
    }

    /// The canvas receiving a new list (what `DesignCanvas::set_display_list` does).
    fn set_list(cache: &mut RenderCache, list: &DisplayList) {
        cache.retain_roots(list);
    }

    fn drops(rec: &Recorder) -> Vec<Op> {
        rec.ops
            .iter()
            .copied()
            .filter(|o| matches!(o, Op::Drop(..)))
            .collect()
    }

    fn rect(x: f32, y: f32, w: f32, h: f32, fill: Paint, shadow: bool) -> DisplayItem {
        DisplayItem::Shape {
            node: NodeKey(0),
            transform: Affine2::from_translation(Vec2::new(x, y)),
            shape: Shape::Rect {
                size: Vec2::new(w, h),
                radii: CornerRadii([12.0; 4]),
            },
            fills: smallvec![fill],
            stroke: None,
            shadows: if shadow {
                smallvec![Shadow::Drop {
                    offset: Vec2::new(0.0, 8.0),
                    blur: 24.0,
                    spread: 0.0,
                    color: Color::rgba(0.0, 0.0, 0.0, 0.4),
                }]
            } else {
                smallvec![]
            },
        }
    }

    fn solid(r: f32, g: f32, b: f32) -> Paint {
        Paint::Solid(Color::rgba(r, g, b, 1.0))
    }

    fn label(x: f32, y: f32) -> DisplayItem {
        DisplayItem::Text {
            node: NodeKey(0),
            transform: Affine2::from_translation(Vec2::new(x, y)),
            block: Arc::new(TextBlock {
                text: "Card".into(),
                runs: smallvec![pen_render::TextRun {
                    range: 0..4,
                    style: pen_render::TextStyle {
                        family: "x".into(),
                        size: 16.0,
                        weight: 400,
                        italic: false,
                        letter_spacing: 0.0,
                        underline: false,
                        strikethrough: false,
                        paint: Paint::Solid(Color::BLACK),
                    },
                }],
                lines: vec![TextLine {
                    range: 0..4,
                    x: 0.0,
                    baseline: 14.0,
                    width: 40.0,
                }],
                align: Default::default(),
                size: Vec2::new(40.0, 20.0),
                clip: false,
            }),
        }
    }

    fn chunk(root: u32, version: u64, items: Vec<DisplayItem>) -> Arc<Chunk> {
        let bounds = items
            .iter()
            .filter_map(geom::item_doc_bounds)
            .reduce(|a, b| a.union(&b))
            .unwrap_or_default();
        Arc::new(Chunk {
            root: NodeKey(root),
            bounds,
            version,
            items,
        })
    }

    /// A frame with two shadowed cards (and a label) over a background.
    fn board(version: u64, card_fill: Paint) -> Arc<Chunk> {
        chunk(
            1,
            version,
            vec![
                rect(0.0, 0.0, 900.0, 600.0, solid(0.9, 0.9, 0.95), false),
                rect(40.0, 40.0, 300.0, 200.0, card_fill, true),
                label(56.0, 56.0),
                rect(400.0, 40.0, 300.0, 200.0, solid(1.0, 1.0, 1.0), true),
            ],
        )
    }

    #[test]
    fn fill_edit_keeps_drop_shadows() {
        let lone = chunk(
            2,
            2,
            vec![rect(760.0, 300.0, 200.0, 140.0, solid(1.0, 1.0, 1.0), true)],
        );
        // Layered (< 50%), direct and zoomed-in regimes.
        for zoom in [0.3, 1.0, 2.0] {
            let mut cache = RenderCache::default();
            let list = DisplayList {
                chunks: vec![board(1, solid(1.0, 1.0, 1.0)), lone.clone()],
            };
            set_list(&mut cache, &list);
            let (before, st) = frame(&list, &mut cache, zoom);
            assert_eq!(st.chunks_prepared, 2);
            let want = drops(&before);
            assert_eq!(want.len(), 3, "zoom {zoom}: {:?}", before.ops);
            // Successive fill edits: only the edited chunk is rebuilt (new Arc, new version);
            // the other one stays the same shared Arc.
            for (v, fill) in [(3, solid(1.0, 0.0, 0.0)), (4, solid(0.2, 0.8, 0.2))] {
                let list = DisplayList {
                    chunks: vec![board(v, fill), lone.clone()],
                };
                set_list(&mut cache, &list);
                let (after, st) = frame(&list, &mut cache, zoom);
                assert_eq!(st.chunks_prepared, 1, "zoom {zoom}");
                assert_eq!(
                    drops(&after),
                    want,
                    "zoom {zoom}: shadows changed after fill edit"
                );
                // Repainting the same list reuses everything.
                let (again, st) = frame(&list, &mut cache, zoom);
                assert_eq!(st.chunks_prepared, 0);
                assert_eq!(drops(&again), want);
            }
        }
    }

    #[test]
    fn same_root_and_version_from_another_builder_is_reprepared() {
        // Builder A: root 1, version 7, three items far away and without shadows.
        let far = chunk(
            1,
            7,
            vec![
                rect(5000.0, 5000.0, 10.0, 10.0, solid(0.0, 0.0, 0.0), false),
                rect(5020.0, 5000.0, 10.0, 10.0, solid(0.0, 0.0, 0.0), false),
                rect(5040.0, 5000.0, 10.0, 10.0, solid(0.0, 0.0, 0.0), false),
            ],
        );
        // Builder B (a reloaded document, another pipeline): the same pair, other content.
        let near = chunk(
            1,
            7,
            vec![
                rect(0.0, 0.0, 900.0, 600.0, solid(0.9, 0.9, 0.95), false),
                rect(40.0, 40.0, 300.0, 200.0, solid(1.0, 0.0, 0.0), true),
                rect(400.0, 40.0, 300.0, 200.0, solid(1.0, 1.0, 1.0), true),
            ],
        );
        for zoom in [0.3, 1.0] {
            let mut cache = RenderCache::default();
            let a = DisplayList {
                chunks: vec![far.clone()],
            };
            set_list(&mut cache, &a);
            let _ = frame(&a, &mut cache, 1.0);
            let b = DisplayList {
                chunks: vec![near.clone()],
            };
            set_list(&mut cache, &b);
            let (rec, st) = frame(&b, &mut cache, zoom);
            // Stale bounds from `far` would cull both cards (and their shadows).
            assert_eq!(st.chunks_prepared, 1, "zoom {zoom}");
            assert_eq!(st.items_drawn, 3, "zoom {zoom}");
            assert_eq!(drops(&rec).len(), 2, "zoom {zoom}: {:?}", rec.ops);
        }
    }

    #[test]
    fn text_shaping_cache_follows_chunk_identity() {
        let mut cache = RenderCache::default();
        let l1 = DisplayList {
            chunks: vec![board(1, solid(1.0, 1.0, 1.0))],
        };
        set_list(&mut cache, &l1);
        let (rec, _) = frame(&l1, &mut cache, 1.0);
        assert_eq!(rec.shaped, 1);
        // A clone of the list (same Arcs): nothing is shaped again.
        let l2 = l1.clone();
        set_list(&mut cache, &l2);
        let (rec, _) = frame(&l2, &mut cache, 1.0);
        assert_eq!(rec.shaped, 0);
        // Same (root, version), new allocation: shaped again (the label may differ).
        let l3 = DisplayList {
            chunks: vec![board(1, solid(1.0, 1.0, 1.0))],
        };
        set_list(&mut cache, &l3);
        let (rec, _) = frame(&l3, &mut cache, 1.0);
        assert_eq!(rec.shaped, 1);
    }

    fn radial(c: Color) -> Paint {
        Paint::radial(
            Vec2::splat(0.5),
            Vec2::splat(0.5),
            smallvec![
                GradientStop {
                    offset: 0.0,
                    color: c
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::rgba(1.0, 1.0, 1.0, 1.0),
                },
            ],
        )
    }

    #[test]
    fn gradient_textures_are_keyed_by_chunk_identity() {
        let mut cache = RenderCache::default();
        let g = |c| chunk(1, 1, vec![rect(0.0, 0.0, 200.0, 100.0, radial(c), true)]);
        let l1 = DisplayList {
            chunks: vec![g(Color::rgba(1.0, 0.0, 0.0, 1.0))],
        };
        set_list(&mut cache, &l1);
        let (rec, _) = frame(&l1, &mut cache, 1.0);
        assert_eq!(drops(&rec).len(), 1);
        assert_eq!(cache.textures.gradients.len(), 1);
        // Same (root, version) but another gradient: the red texture must not be reused.
        let l2 = DisplayList {
            chunks: vec![g(Color::rgba(0.0, 0.0, 1.0, 1.0))],
        };
        set_list(&mut cache, &l2);
        let (rec, _) = frame(&l2, &mut cache, 1.0);
        assert_eq!(drops(&rec).len(), 1);
        assert_eq!(cache.textures.gradients.len(), 2);
        let _ = frame(&l2, &mut cache, 1.0);
        assert_eq!(cache.textures.gradients.len(), 2);
    }

    #[test]
    fn fallback_textures_are_keyed_by_chunk_identity() {
        let blurred = |x: f32| {
            chunk(
                1,
                1,
                vec![
                    DisplayItem::PushLayer {
                        opacity: 1.0,
                        blend: BlendMode::Normal,
                        blur: 4.0,
                        backdrop_blur: 0.0,
                        bounds: Rect::new(x - 20.0, -20.0, 240.0, 160.0),
                    },
                    rect(x, 0.0, 200.0, 100.0, solid(1.0, 0.0, 0.0), true),
                    DisplayItem::PopLayer,
                ],
            )
        };
        let mut cache = RenderCache::default();
        let l1 = DisplayList {
            chunks: vec![blurred(0.0)],
        };
        set_list(&mut cache, &l1);
        let _ = frame(&l1, &mut cache, 1.0);
        let k1 = std::mem::take(&mut cache.requests)
            .first()
            .expect("fallback request")
            .key;
        cache.fallback.mark_pending(k1);
        let img = || render_image_from_bgra(1, 1, vec![0; 4]).unwrap();
        // The chunk is replaced (same root and version, new content) while the job runs.
        let l2 = DisplayList {
            chunks: vec![blurred(50.0)],
        };
        set_list(&mut cache, &l2);
        assert_eq!(cache.fallback.pending_len(), 1, "job still in flight");
        cache.finish_fallback(k1, Some((Rect::new(0.0, 0.0, 1.0, 1.0), img(), 4)));
        assert_eq!(cache.fallback.len(), 0, "stale texture accepted");
        assert_eq!(cache.fallback.pending_len(), 0);
        let _ = frame(&l2, &mut cache, 1.0);
        let k2 = std::mem::take(&mut cache.requests)
            .first()
            .expect("fallback request")
            .key;
        assert_ne!(k1.id, k2.id);
        cache.fallback.mark_pending(k2);
        cache.finish_fallback(k2, Some((Rect::new(0.0, 0.0, 300.0, 200.0), img(), 4)));
        assert_eq!(cache.fallback.len(), 1);
        let (rec, st) = frame(&l2, &mut cache, 1.0);
        assert_eq!(st.fallback_drawn, 1);
        assert!(rec.ops.contains(&Op::Image));
    }
}
