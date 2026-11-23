//! The display-list interpreter: targets (root + offscreen layers), clip stacks, shapes, shadows,
//! strokes and text.

use crate::blur::{BlurScratch, Edge, blur_region};
use crate::geom::{
    IRect, blur_extent, blur_sigma, is_axis_aligned, map_rect, outset, path_is_closed, push_rrect,
    push_shape, shape_bounds, to_ts, ts_scale,
};
use crate::paint::{self, ImageCache, premul_u8};
use crate::text::TextEngine;
use cosmic_text::{CacheKey, CacheKeyFlags, Command, FontSystem, SwashContent, SwashImage};
use pen_render::{
    Affine2, BlendMode, Color, DisplayItem, FillRule, LineCap, LineJoin, Paint, Rect, Shadow,
    Shape, Stroke, StrokeAlign, TextAlign, TextBlock, Vec2,
};
use tiny_skia::{IntSize, Mask, Path, PathBuilder, Pixmap, PixmapPaint, Transform};

/// Long-lived caches and scratch buffers, owned by `RasterContext`.
pub struct Resources {
    pub images: ImageCache,
    pub text: Option<TextEngine>,
    /// Font system handed in before the text engine was created.
    pub pending_fonts: Option<FontSystem>,
    blur: BlurScratch,
    masks: Vec<(Mask, IRect)>,
    bufs: Vec<Vec<u8>>,
    builders: Vec<PathBuilder>,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            images: ImageCache::new(),
            text: None,
            pending_fonts: None,
            blur: BlurScratch::default(),
            masks: Vec::new(),
            bufs: Vec::new(),
            builders: Vec::new(),
        }
    }
}

const MAX_POOLED_MASKS: usize = 16;
const MAX_POOLED_BUFS: usize = 16;

impl Resources {
    pub fn text_engine(&mut self) -> &mut TextEngine {
        if self.text.is_none() {
            self.text = Some(match self.pending_fonts.take() {
                Some(fs) => TextEngine::new(fs),
                None => TextEngine::system(),
            });
        }
        self.text.as_mut().expect("text engine just created")
    }

    /// A zeroed mask of `w x h`, reused from the pool when possible.
    fn mask(&mut self, w: u32, h: u32) -> Option<Mask> {
        if let Some(i) = self
            .masks
            .iter()
            .position(|(m, _)| m.width() == w && m.height() == h)
        {
            let (mut m, dirty) = self.masks.swap_remove(i);
            clear_mask_rect(&mut m, dirty);
            return Some(m);
        }
        Mask::new(w, h)
    }

    /// Returns a mask to the pool; only `dirty` may contain non-zero values.
    fn recycle_mask(&mut self, m: Mask, dirty: IRect) {
        if self.masks.len() < MAX_POOLED_MASKS {
            self.masks.push((m, dirty));
        }
    }

    /// A zeroed (transparent) pixmap, reusing pooled buffers.
    fn pixmap(&mut self, w: u32, h: u32) -> Option<Pixmap> {
        let size = IntSize::from_wh(w, h)?;
        let len = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
        let mut v = self.bufs.pop().unwrap_or_default();
        v.clear();
        v.resize(len, 0);
        Pixmap::from_vec(v, size)
    }

    fn recycle_pixmap(&mut self, p: Pixmap) {
        if self.bufs.len() < MAX_POOLED_BUFS {
            self.bufs.push(p.take());
        }
    }

    fn path(&mut self, f: impl FnOnce(&mut PathBuilder)) -> Option<Path> {
        let mut b = self.builders.pop().unwrap_or_default();
        f(&mut b);
        b.finish()
    }

    fn recycle_path(&mut self, p: Path) {
        if self.builders.len() < 8 {
            self.builders.push(p.clear());
        }
    }
}

fn clear_mask_rect(m: &mut Mask, r: IRect) {
    let r = r.intersect(&IRect::from_size(m.width(), m.height()));
    if r.is_empty() {
        return;
    }
    let w = m.width() as usize;
    let data = m.data_mut();
    for y in r.y0..r.y1 {
        let row = y as usize * w;
        data[row + r.x0 as usize..row + r.x1 as usize].fill(0);
    }
}

#[inline]
fn mul255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

/// `dst[r] *= src[r]` for mask data (same dimensions).
fn mul_mask_rect(dst: &mut Mask, src: &Mask, r: IRect, invert_dst: bool) {
    let r = r.intersect(&IRect::from_size(dst.width(), dst.height()));
    let w = dst.width() as usize;
    let s = src.data();
    let d = dst.data_mut();
    for y in r.y0..r.y1 {
        let row = y as usize * w;
        for i in row + r.x0 as usize..row + r.x1 as usize {
            let v = if invert_dst { 255 - d[i] } else { d[i] } as u32;
            d[i] = mul255(v, s[i] as u32) as u8;
        }
    }
}

fn invert_mask_rect(m: &mut Mask, r: IRect) {
    let r = r.intersect(&IRect::from_size(m.width(), m.height()));
    let w = m.width() as usize;
    let d = m.data_mut();
    for y in r.y0..r.y1 {
        let row = y as usize * w;
        for v in &mut d[row + r.x0 as usize..row + r.x1 as usize] {
            *v = 255 - *v;
        }
    }
}

pub fn blend_mode(b: BlendMode) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode as T;
    match b {
        BlendMode::Normal => T::SourceOver,
        BlendMode::Multiply => T::Multiply,
        BlendMode::Screen => T::Screen,
        BlendMode::Overlay => T::Overlay,
        BlendMode::Darken => T::Darken,
        BlendMode::Lighten => T::Lighten,
        BlendMode::ColorDodge => T::ColorDodge,
        BlendMode::ColorBurn => T::ColorBurn,
        BlendMode::HardLight => T::HardLight,
        BlendMode::SoftLight => T::SoftLight,
        BlendMode::Difference => T::Difference,
        BlendMode::Exclusion => T::Exclusion,
        BlendMode::Hue => T::Hue,
        BlendMode::Saturation => T::Saturation,
        BlendMode::Color => T::Color,
        BlendMode::Luminosity => T::Luminosity,
    }
}

fn fill_rule(shape: &Shape) -> tiny_skia::FillRule {
    match shape {
        Shape::Path(p) if p.fill_rule == FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
        _ => tiny_skia::FillRule::Winding,
    }
}

fn white_paint() -> tiny_skia::Paint<'static> {
    tiny_skia::Paint {
        shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::WHITE),
        ..Default::default()
    }
}

struct Clip {
    /// `None` = empty clip (nothing visible).
    mask: Option<Mask>,
    bounds: IRect,
    dirty: IRect,
}

struct LayerFx {
    opacity: f32,
    blend: tiny_skia::BlendMode,
    sigma: f32,
}

struct Target {
    pixmap: Pixmap,
    /// Root-device coordinates of this target's pixel (0, 0).
    origin: (i32, i32),
    /// Drawable area (`EMPTY` for culled layers).
    base: IRect,
    clips: Vec<Clip>,
    fx: LayerFx,
}

impl Target {
    fn clip_bounds(&self) -> IRect {
        self.clips.last().map_or(self.base, |c| c.bounds)
    }
    fn full(&self) -> IRect {
        IRect::from_size(self.pixmap.width(), self.pixmap.height())
    }
}

enum LayerFrame {
    Passthrough,
    Offscreen,
}

pub struct Renderer<'r> {
    res: &'r mut Resources,
    view: Transform,
    targets: Vec<Target>,
    layers: Vec<LayerFrame>,
}

impl<'r> Renderer<'r> {
    pub fn new(res: &'r mut Resources, pixmap: Pixmap, view: Transform) -> Self {
        let base = IRect::from_size(pixmap.width(), pixmap.height());
        let root = Target {
            pixmap,
            origin: (0, 0),
            base,
            clips: Vec::new(),
            fx: LayerFx {
                opacity: 1.0,
                blend: tiny_skia::BlendMode::SourceOver,
                sigma: 0.0,
            },
        };
        Self {
            res,
            view,
            targets: vec![root],
            layers: Vec::new(),
        }
    }

    /// Pops any unbalanced scopes and returns the root pixmap.
    pub fn finish(mut self) -> Pixmap {
        while !self.layers.is_empty() {
            self.pop_layer();
        }
        let mut root = self.targets.pop().expect("root target");
        for c in root.clips.drain(..) {
            if let Some(m) = c.mask {
                self.res.recycle_mask(m, c.dirty);
            }
        }
        root.pixmap
    }

    fn top(&self) -> &Target {
        self.targets.last().expect("root target")
    }

    /// Item transform -> current target pixels.
    fn dev(&self, t: &Affine2) -> Transform {
        let (ox, oy) = self.top().origin;
        Transform::from_translate(-ox as f32, -oy as f32)
            .pre_concat(self.view)
            .pre_concat(to_ts(t))
    }

    fn doc_to_target(&self) -> Transform {
        let (ox, oy) = self.top().origin;
        Transform::from_translate(-ox as f32, -oy as f32).pre_concat(self.view)
    }

    pub fn item(&mut self, item: &DisplayItem) {
        match item {
            DisplayItem::Shape {
                transform,
                shape,
                fills,
                stroke,
                shadows,
                ..
            } => self.shape(transform, shape, fills, stroke.as_ref(), shadows),
            DisplayItem::Text {
                transform, block, ..
            } => self.text(transform, block),
            DisplayItem::PushClip { transform, shape } => self.push_clip(transform, shape),
            DisplayItem::PopClip => self.pop_clip(),
            DisplayItem::PushLayer {
                opacity,
                blend,
                blur,
                backdrop_blur,
                bounds,
            } => self.push_layer(*opacity, *blend, *blur, *backdrop_blur, bounds),
            DisplayItem::PopLayer => self.pop_layer(),
        }
    }

    // ---------------------------------------------------------------- clips

    fn push_clip(&mut self, transform: &Affine2, shape: &Shape) {
        let ts = self.dev(transform);
        let top = self.top();
        let parent = top.clip_bounds();
        let full = top.full();
        let dev_b = IRect::round_out(&map_rect(&ts, &shape_bounds(shape)))
            .outset(1)
            .intersect(&full);
        let bounds = dev_b.intersect(&parent);
        let empty = Clip {
            mask: None,
            bounds: IRect::EMPTY,
            dirty: IRect::EMPTY,
        };
        if bounds.is_empty() || !ts.is_finite() {
            self.targets.last_mut().expect("target").clips.push(empty);
            return;
        }
        let (w, h) = (full.width() as u32, full.height() as u32);
        let path = self.res.path(|pb| push_shape(pb, shape, 0.0));
        let (Some(path), Some(mut mask)) = (path, self.res.mask(w, h)) else {
            self.targets.last_mut().expect("target").clips.push(empty);
            return;
        };
        mask.fill_path(&path, fill_rule(shape), true, ts);
        self.res.recycle_path(path);
        let t = self.targets.last_mut().expect("target");
        if let Some(pm) = t.clips.last().and_then(|c| c.mask.as_ref()) {
            mul_mask_rect(&mut mask, pm, dev_b, false);
        }
        t.clips.push(Clip {
            mask: Some(mask),
            bounds,
            dirty: dev_b,
        });
    }

    fn pop_clip(&mut self) {
        let t = self.targets.last_mut().expect("target");
        if let Some(c) = t.clips.pop()
            && let Some(m) = c.mask
        {
            self.res.recycle_mask(m, c.dirty);
        }
    }

    // --------------------------------------------------------------- layers

    fn push_layer(
        &mut self,
        opacity: f32,
        blend: BlendMode,
        blur: f32,
        backdrop_blur: f32,
        bounds: &Rect,
    ) {
        let top = self.top();
        let clip_b = top.clip_bounds();
        let scale = ts_scale(&self.view);
        let culled = clip_b.is_empty() || opacity <= 0.0;
        if !culled
            && opacity >= 1.0
            && blend == BlendMode::Normal
            && blur <= 0.0
            && backdrop_blur <= 0.0
        {
            self.layers.push(LayerFrame::Passthrough);
            return;
        }
        let sigma = blur_sigma(blur) * scale;
        let margin = blur_extent(sigma);
        let to_target = self.doc_to_target();
        let has_bounds = bounds.w > 0.0 && bounds.h > 0.0;
        let area = if has_bounds {
            IRect::round_out(&map_rect(&to_target, bounds)).outset(1)
        } else {
            top.full()
        };
        let region = area.outset(margin).intersect(&clip_b.outset(margin));
        if culled || region.is_empty() {
            self.push_culled_layer();
            return;
        }
        if backdrop_blur > 0.0 {
            let b_area = area.intersect(&clip_b);
            self.backdrop_blur(b_area, blur_sigma(backdrop_blur) * scale);
        }
        let Some(pixmap) = self
            .res
            .pixmap(region.width() as u32, region.height() as u32)
        else {
            self.push_culled_layer();
            return;
        };
        let (ox, oy) = self.top().origin;
        let base = IRect::from_size(pixmap.width(), pixmap.height());
        self.targets.push(Target {
            pixmap,
            origin: (ox + region.x0, oy + region.y0),
            base,
            clips: Vec::new(),
            fx: LayerFx {
                opacity: opacity.clamp(0.0, 1.0),
                blend: blend_mode(blend),
                sigma,
            },
        });
        self.layers.push(LayerFrame::Offscreen);
    }

    fn push_culled_layer(&mut self) {
        let pixmap = self.res.pixmap(1, 1).expect("1x1 pixmap");
        self.targets.push(Target {
            pixmap,
            origin: self.top().origin,
            base: IRect::EMPTY,
            clips: Vec::new(),
            fx: LayerFx {
                opacity: 0.0,
                blend: tiny_skia::BlendMode::SourceOver,
                sigma: 0.0,
            },
        });
        self.layers.push(LayerFrame::Offscreen);
    }

    fn pop_layer(&mut self) {
        match self.layers.pop() {
            None | Some(LayerFrame::Passthrough) => {}
            Some(LayerFrame::Offscreen) => {
                let Some(mut child) = self.targets.pop() else {
                    return;
                };
                for c in child.clips.drain(..) {
                    if let Some(m) = c.mask {
                        self.res.recycle_mask(m, c.dirty);
                    }
                }
                if !child.base.is_empty() {
                    if child.fx.sigma > 0.0 {
                        let (w, h) = (child.pixmap.width(), child.pixmap.height());
                        blur_region(
                            child.pixmap.data_mut(),
                            w as usize * 4,
                            4,
                            0,
                            4,
                            IRect::from_size(w, h),
                            child.fx.sigma,
                            Edge::Transparent,
                            &mut self.res.blur,
                        );
                    }
                    let parent = self.targets.last_mut().expect("parent target");
                    let paint = PixmapPaint {
                        opacity: child.fx.opacity,
                        blend_mode: child.fx.blend,
                        quality: tiny_skia::FilterQuality::Nearest,
                    };
                    let mask = parent.clips.last().and_then(|c| c.mask.as_ref());
                    if !parent.clip_bounds().is_empty() {
                        parent.pixmap.draw_pixmap(
                            child.origin.0 - parent.origin.0,
                            child.origin.1 - parent.origin.1,
                            child.pixmap.as_ref(),
                            &paint,
                            Transform::identity(),
                            mask,
                        );
                    }
                }
                self.res.recycle_pixmap(child.pixmap);
            }
        }
    }

    /// Blurs the current target's pixels inside `area` (target-local) in place.
    fn backdrop_blur(&mut self, area: IRect, sigma: f32) {
        if area.is_empty() || sigma <= 0.0 {
            return;
        }
        let margin = blur_extent(sigma);
        let full = self.top().full();
        let src = area.outset(margin).intersect(&full);
        let Some(mut tmp) = self.res.pixmap(src.width() as u32, src.height() as u32) else {
            return;
        };
        let t = self.targets.last_mut().expect("target");
        let pw = t.pixmap.width() as usize;
        let tw = tmp.width() as usize;
        {
            let s = t.pixmap.data();
            let d = tmp.data_mut();
            for y in 0..src.height() as usize {
                let so = ((src.y0 as usize + y) * pw + src.x0 as usize) * 4;
                d[y * tw * 4..(y + 1) * tw * 4].copy_from_slice(&s[so..so + tw * 4]);
            }
        }
        let tw_u = tmp.width();
        let th_u = tmp.height();
        blur_region(
            tmp.data_mut(),
            tw * 4,
            4,
            0,
            4,
            IRect::from_size(tw_u, th_u),
            sigma,
            Edge::Clamp,
            &mut self.res.blur,
        );
        let mask = t.clips.last().and_then(|c| c.mask.as_ref());
        let s = tmp.data();
        let d = t.pixmap.data_mut();
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let di = (y as usize * pw + x as usize) * 4;
                let si = (((y - src.y0) as usize) * tw + (x - src.x0) as usize) * 4;
                let k = mask.map_or(255, |m| m.data()[y as usize * pw + x as usize] as u32);
                if k == 255 {
                    d[di..di + 4].copy_from_slice(&s[si..si + 4]);
                } else if k > 0 {
                    for c in 0..4 {
                        let (a, b) = (d[di + c] as u32, s[si + c] as u32);
                        d[di + c] = ((a * (255 - k) + b * k + 127) / 255) as u8;
                    }
                }
            }
        }
        self.res.recycle_pixmap(tmp);
    }

    // --------------------------------------------------------------- shapes

    fn shape(
        &mut self,
        transform: &Affine2,
        shape: &Shape,
        fills: &[Paint],
        stroke: Option<&Stroke>,
        shadows: &[Shadow],
    ) {
        let ts = self.dev(transform);
        let clip_b = self.top().clip_bounds();
        if clip_b.is_empty() || !ts.is_finite() || ts_scale(&ts) < 1e-9 {
            return;
        }
        let local = shape_bounds(shape);
        // Conservative local outset covering strokes and shadows, for culling.
        let mut out: f32 = 1.0;
        if let Some(s) = stroke {
            let w = s.widths.iter().cloned().fold(0.0, f32::max);
            let miter = if s.join == LineJoin::Miter {
                s.miter_limit.max(1.0)
            } else {
                1.0
            };
            out = out.max(w * 2.0 * miter);
        }
        for sh in shadows {
            let (Shadow::Drop {
                offset,
                blur,
                spread,
                ..
            }
            | Shadow::Inner {
                offset,
                blur,
                spread,
                ..
            }) = sh;
            out = out.max(offset.x.abs().max(offset.y.abs()) + spread.max(0.0) + blur * 1.5 + 2.0);
        }
        let dev_bounds = IRect::round_out(&map_rect(&ts, &outset(&local, out)));
        if dev_bounds.intersect(&clip_b).is_empty() {
            return;
        }
        let rule = fill_rule(shape);
        let Some(path) = self.res.path(|pb| push_shape(pb, shape, 0.0)) else {
            return;
        };

        for sh in shadows {
            if let Shadow::Drop {
                offset,
                blur,
                spread,
                color,
            } = sh
            {
                self.drop_shadow(
                    shape, &path, rule, &ts, &local, *offset, *blur, *spread, *color,
                );
            }
        }
        let plain_rect =
            matches!(shape, Shape::Rect { radii, .. } if radii.is_zero()) && is_axis_aligned(&ts);
        for f in fills {
            self.fill_paint(&path, rule, f, &ts, &local, plain_rect.then_some(&local));
        }
        for sh in shadows {
            if let Shadow::Inner {
                offset,
                blur,
                spread,
                color,
            } = sh
            {
                self.inner_shadow(
                    shape, &path, rule, &ts, &local, *offset, *blur, *spread, *color,
                );
            }
        }
        if let Some(s) = stroke {
            self.stroke(shape, &path, rule, &ts, &local, s);
        }
        self.res.recycle_path(path);
    }

    /// Fills `path` (local space, drawn with `ts`) with one paint, honoring the current clip.
    /// `rect_fast` enables the axis-aligned rectangle fast path.
    fn fill_paint(
        &mut self,
        path: &Path,
        rule: tiny_skia::FillRule,
        p: &Paint,
        ts: &Transform,
        bounds: &Rect,
        rect_fast: Option<&Rect>,
    ) {
        self.fill_paint_masked(path, rule, p, ts, bounds, rect_fast, None);
    }

    /// Like `fill_paint` but with an explicit target-sized mask replacing the clip mask.
    #[allow(clippy::too_many_arguments)]
    fn fill_paint_masked(
        &mut self,
        path: &Path,
        rule: tiny_skia::FillRule,
        p: &Paint,
        ts: &Transform,
        bounds: &Rect,
        rect_fast: Option<&Rect>,
        override_mask: Option<&Mask>,
    ) {
        if let Paint::Angular {
            center,
            start_angle,
            stops,
        } = p
        {
            self.fill_conic(path, rule, ts, *center, *start_angle, stops, override_mask);
            return;
        }
        let Some(level) = paint::prepare(p, bounds, ts_scale(ts), &mut self.res.images) else {
            return;
        };
        let Some(shader) = paint::shader(p, bounds, level, &self.res.images) else {
            return;
        };
        let t = self.targets.last_mut().expect("target");
        let mask = override_mask.or_else(|| t.clips.last().and_then(|c| c.mask.as_ref()));
        let mut paint = tiny_skia::Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };
        if let Some(r) = rect_fast {
            let d = map_rect(ts, r);
            // tiny-skia's anti-aliased fill_rect hits a debug_assert (and drops the inner
            // span) when a rect spans less than ~2 device pixels; use the path route then.
            if d.w >= 2.0 && d.h >= 2.0 {
                if let Some(dr) = tiny_skia::Rect::from_xywh(d.x, d.y, d.w, d.h) {
                    paint.shader.transform(*ts);
                    t.pixmap.fill_rect(dr, &paint, Transform::identity(), mask);
                }
                return;
            }
        }
        t.pixmap.fill_path(path, &paint, rule, *ts, mask);
    }

    #[allow(clippy::too_many_arguments)]
    fn fill_conic(
        &mut self,
        path: &Path,
        rule: tiny_skia::FillRule,
        ts: &Transform,
        center: Vec2,
        start_angle: f32,
        stops: &[pen_render::GradientStop],
        override_mask: Option<&Mask>,
    ) {
        if stops.is_empty() {
            return;
        }
        let Some(inv) = ts.invert() else { return };
        let pb = path.bounds();
        let local = Rect::new(pb.x(), pb.y(), pb.width(), pb.height());
        let region = IRect::round_out(&map_rect(ts, &local))
            .outset(1)
            .intersect(&self.top().clip_bounds());
        if region.is_empty() {
            return;
        }
        let Some(mut tex) = self
            .res
            .pixmap(region.width() as u32, region.height() as u32)
        else {
            return;
        };
        paint::render_conic(&mut tex, region, &inv, center, start_angle, stops);
        {
            let shader = tiny_skia::Pattern::new(
                tex.as_ref(),
                tiny_skia::SpreadMode::Pad,
                tiny_skia::FilterQuality::Nearest,
                1.0,
                inv.pre_translate(region.x0 as f32, region.y0 as f32),
            );
            let paint = tiny_skia::Paint {
                shader,
                anti_alias: true,
                ..Default::default()
            };
            let t = self.targets.last_mut().expect("target");
            let mask = override_mask.or_else(|| t.clips.last().and_then(|c| c.mask.as_ref()));
            t.pixmap.fill_path(path, &paint, rule, *ts, mask);
        }
        self.res.recycle_pixmap(tex);
    }

    /// Path of `shape` offset by `grow` (local units). Paths use stroke outlines (`spread_outline`).
    fn grown_path(&mut self, shape: &Shape, grow: f32) -> Option<Path> {
        self.res.path(|pb| push_shape(pb, shape, grow))
    }

    /// Rasterizes the silhouette of `shape` grown by `grow` into the alpha of `dst` (white, src-over).
    #[allow(clippy::too_many_arguments)]
    fn silhouette(
        &mut self,
        dst: &mut Pixmap,
        shape: &Shape,
        path: &Path,
        rule: tiny_skia::FillRule,
        ts: Transform,
        grow: f32,
    ) {
        let paint = white_paint();
        match shape {
            Shape::Path(_) if grow != 0.0 => {
                dst.fill_path(path, &paint, rule, ts, None);
                let st = tiny_skia::Stroke {
                    width: grow.abs() * 2.0,
                    line_join: tiny_skia::LineJoin::Round,
                    line_cap: tiny_skia::LineCap::Round,
                    ..Default::default()
                };
                if let Some(outline) = path.stroke(&st, ts_scale(&ts).max(0.01)) {
                    let mut p = paint.clone();
                    if grow < 0.0 {
                        p.blend_mode = tiny_skia::BlendMode::DestinationOut;
                    }
                    dst.fill_path(&outline, &p, tiny_skia::FillRule::Winding, ts, None);
                }
            }
            _ if grow != 0.0 => {
                if let Some(g) = self.grown_path(shape, grow) {
                    dst.fill_path(&g, &paint, rule, ts, None);
                    self.res.recycle_path(g);
                }
            }
            _ => dst.fill_path(path, &paint, rule, ts, None),
        }
    }

    /// Recolors the alpha of `p` with `color` and composites it at `at` with the current clip.
    /// Composites `color` with the coverage stored in the alpha of `p` at `at` (target pixels),
    /// honoring the current clip. `k` is the downscale factor of `p` (upsampled bilinearly).
    fn composite_alpha(&mut self, p: Pixmap, at: (i32, i32), k: u32, color: Color) {
        let t = self.targets.last_mut().expect("target");
        let clip_b = t.clip_bounds();
        let mask = t.clips.last().and_then(|c| c.mask.as_ref());
        composite_coverage(&mut t.pixmap, mask, clip_b, &p, at, k, premul_u8(color));
        self.res.recycle_pixmap(p);
    }

    #[allow(clippy::too_many_arguments)]
    fn drop_shadow(
        &mut self,
        shape: &Shape,
        path: &Path,
        rule: tiny_skia::FillRule,
        ts: &Transform,
        local: &Rect,
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    ) {
        if color.a <= 0.0 {
            return;
        }
        let sts = ts.pre_translate(offset.x, offset.y);
        let sigma = blur_sigma(blur) * ts_scale(ts);
        if sigma < 0.2 && spread == 0.0 {
            self.fill_paint(path, rule, &Paint::Solid(color), &sts, local, None);
            return;
        }
        let margin = blur_extent(sigma) + 1;
        let clip_b = self.top().clip_bounds();
        let region = IRect::round_out(&map_rect(&sts, &outset(local, spread.max(0.0))))
            .outset(margin)
            .intersect(&clip_b.outset(margin));
        if region.intersect(&clip_b).is_empty() {
            return;
        }
        // Large blurs are computed at reduced resolution (like Skia): the result is smooth enough
        // to be upsampled bilinearly.
        let mut k = 1u32;
        while k < 8 && sigma / (2 * k) as f32 >= 2.0 {
            k *= 2;
        }
        let (w, h) = (
            (region.width() as u32).div_ceil(k) + 1,
            (region.height() as u32).div_ceil(k) + 1,
        );
        let Some(mut p) = self.res.pixmap(w, h) else {
            return;
        };
        let inv_k = 1.0 / k as f32;
        let pts = Transform::from_row(inv_k, 0.0, 0.0, inv_k, 0.0, 0.0)
            .pre_translate(-region.x0 as f32, -region.y0 as f32)
            .pre_concat(sts);
        self.silhouette(&mut p, shape, path, rule, pts, spread);
        let s = sigma / k as f32;
        blur_region(
            p.data_mut(),
            w as usize * 4,
            4,
            3,
            1,
            IRect::from_size(w, h),
            s,
            Edge::Transparent,
            &mut self.res.blur,
        );
        self.composite_alpha(p, (region.x0, region.y0), k, color);
    }

    #[allow(clippy::too_many_arguments)]
    fn inner_shadow(
        &mut self,
        shape: &Shape,
        path: &Path,
        rule: tiny_skia::FillRule,
        ts: &Transform,
        local: &Rect,
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    ) {
        if color.a <= 0.0 {
            return;
        }
        let sigma = blur_sigma(blur) * ts_scale(ts);
        let margin = blur_extent(sigma) + 1;
        let clip_b = self.top().clip_bounds();
        let draw = IRect::round_out(&map_rect(ts, local))
            .outset(1)
            .intersect(&clip_b);
        if draw.is_empty() {
            return;
        }
        let work = draw.outset(margin);
        let (w, h) = (work.width() as u32, work.height() as u32);
        let Some(mut hole) = self.res.pixmap(w, h) else {
            return;
        };
        let to_work = Transform::from_translate(-work.x0 as f32, -work.y0 as f32);
        let hts = to_work.pre_concat(*ts).pre_translate(offset.x, offset.y);
        self.silhouette(&mut hole, shape, path, rule, hts, -spread);
        for px in hole.data_mut().as_chunks_mut::<4>().0 {
            px[3] = 255 - px[3];
        }
        blur_region(
            hole.data_mut(),
            w as usize * 4,
            4,
            3,
            1,
            IRect::from_size(w, h),
            sigma,
            Edge::Clamp,
            &mut self.res.blur,
        );
        let Some(mut inside) = self.res.pixmap(w, h) else {
            return;
        };
        inside.fill_path(path, &white_paint(), rule, to_work.pre_concat(*ts), None);
        for (a, b) in hole
            .data_mut()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(inside.data().as_chunks::<4>().0)
        {
            a[3] = mul255(a[3] as u32, b[3] as u32) as u8;
        }
        self.res.recycle_pixmap(inside);
        self.composite_alpha(hole, (work.x0, work.y0), 1, color);
    }

    fn stroke(
        &mut self,
        shape: &Shape,
        path: &Path,
        rule: tiny_skia::FillRule,
        ts: &Transform,
        local: &Rect,
        s: &Stroke,
    ) {
        let w_max = s.widths.iter().cloned().fold(0.0, f32::max);
        if w_max.is_nan() || w_max <= 0.0 || s.paints.is_empty() {
            return;
        }
        let closed = match shape {
            Shape::Path(p) => path_is_closed(p),
            _ => true,
        };
        let align = if closed { s.align } else { StrokeAlign::Center };

        // Rectangles: exact ring geometry (supports per-side widths).
        if let Shape::Rect { size, radii } = shape {
            let uniform = s.is_uniform();
            let bevel_sharp = s.join == LineJoin::Bevel && radii.is_zero();
            if s.dash.is_empty() && (!uniform || align != StrokeAlign::Center) && !bevel_sharp {
                if let Some(ring) = self.rect_ring(*size, radii.0, s.widths, align, s.join) {
                    for p in &s.paints {
                        self.fill_paint(&ring, tiny_skia::FillRule::EvenOdd, p, ts, local, None);
                    }
                    self.res.recycle_path(ring);
                }
                return;
            }
        }

        let w = if s.is_uniform() { s.widths[0] } else { w_max };
        let dash = if s.dash.is_empty() {
            None
        } else {
            let mut d: Vec<f32> = s.dash.iter().map(|v| v.max(0.0)).collect();
            if d.len() % 2 == 1 {
                d.extend_from_within(..);
            }
            tiny_skia::StrokeDash::new(d, 0.0)
        };
        let st = tiny_skia::Stroke {
            width: if align == StrokeAlign::Center {
                w
            } else {
                w * 2.0
            },
            miter_limit: if s.miter_limit > 0.0 {
                s.miter_limit
            } else {
                4.0
            },
            line_cap: match s.cap {
                LineCap::Butt => tiny_skia::LineCap::Butt,
                LineCap::Round => tiny_skia::LineCap::Round,
                LineCap::Square => tiny_skia::LineCap::Square,
            },
            line_join: match s.join {
                LineJoin::Miter => tiny_skia::LineJoin::Miter,
                LineJoin::Round => tiny_skia::LineJoin::Round,
                LineJoin::Bevel => tiny_skia::LineJoin::Bevel,
            },
            dash,
        };

        if align == StrokeAlign::Center {
            for p in &s.paints {
                self.stroke_paint(path, &st, p, ts, local, None);
            }
            return;
        }

        // Inside / outside: stroke at double width through a mask of the shape (or its complement).
        let t = self.top();
        let full = t.full();
        let clip_b = t.clip_bounds();
        let miter = if s.join == LineJoin::Miter {
            st.miter_limit.max(1.0)
        } else {
            1.0
        };
        let region =
            IRect::round_out(&map_rect(ts, &outset(local, w * 2.0 * miter + 1.0))).intersect(&full);
        if region.intersect(&clip_b).is_empty() {
            return;
        }
        let Some(mut mask) = self.res.mask(full.width() as u32, full.height() as u32) else {
            return;
        };
        mask.fill_path(path, rule, true, *ts);
        if align == StrokeAlign::Outside {
            invert_mask_rect(&mut mask, region);
        }
        // Targets without a clip mask have no clip beyond their full extent.
        if let Some(cm) = self.top().clips.last().and_then(|c| c.mask.as_ref()) {
            mul_mask_rect(&mut mask, cm, region, false);
        }
        for p in &s.paints {
            self.stroke_paint(path, &st, p, ts, local, Some(&mask));
        }
        self.res.recycle_mask(mask, region);
    }

    fn stroke_paint(
        &mut self,
        path: &Path,
        st: &tiny_skia::Stroke,
        p: &Paint,
        ts: &Transform,
        bounds: &Rect,
        override_mask: Option<&Mask>,
    ) {
        if let Paint::Angular { .. } = p {
            // Conic strokes: fill the stroke outline with the conic shader.
            if let Some(outline) = path.stroke(st, ts_scale(ts).max(0.01)) {
                self.fill_paint_masked(
                    &outline,
                    tiny_skia::FillRule::Winding,
                    p,
                    ts,
                    bounds,
                    None,
                    override_mask,
                );
            }
            return;
        }
        let Some(level) = paint::prepare(p, bounds, ts_scale(ts), &mut self.res.images) else {
            return;
        };
        let Some(shader) = paint::shader(p, bounds, level, &self.res.images) else {
            return;
        };
        let t = self.targets.last_mut().expect("target");
        let mask = override_mask.or_else(|| t.clips.last().and_then(|c| c.mask.as_ref()));
        let paint = tiny_skia::Paint {
            shader,
            anti_alias: true,
            ..Default::default()
        };
        t.pixmap.stroke_path(path, &paint, st, *ts, mask);
    }

    /// Ring between the outer and inner edge of a rect stroke (even-odd).
    fn rect_ring(
        &mut self,
        size: Vec2,
        radii: [f32; 4],
        widths: [f32; 4],
        align: StrokeAlign,
        join: LineJoin,
    ) -> Option<Path> {
        let [t, r, b, l] = widths.map(|w| w.max(0.0));
        let (out_k, in_k) = match align {
            StrokeAlign::Inside => (0.0, 1.0),
            StrokeAlign::Center => (0.5, 0.5),
            StrokeAlign::Outside => (1.0, 0.0),
        };
        let (ot, or, ob, ol) = (t * out_k, r * out_k, b * out_k, l * out_k);
        let (it, ir, ib, il) = (t * in_k, r * in_k, b * in_k, l * in_k);
        // Corners: tl (top, left), tr (top, right), br (bottom, right), bl (bottom, left).
        let adj_out = [ot.max(ol), ot.max(or), ob.max(or), ob.max(ol)];
        let adj_in = [it.max(il), it.max(ir), ib.max(ir), ib.max(il)];
        let mut outer_r = [0.0f32; 4];
        let mut inner_r = [0.0f32; 4];
        for i in 0..4 {
            outer_r[i] = if radii[i] > 0.0 {
                radii[i] + adj_out[i]
            } else if join == LineJoin::Round {
                adj_out[i]
            } else {
                0.0
            };
            inner_r[i] = (radii[i] - adj_in[i]).max(0.0);
        }
        let (iw, ih) = (size.x - il - ir, size.y - it - ib);
        self.res.path(|pb| {
            push_rrect(pb, -ol, -ot, size.x + ol + or, size.y + ot + ob, outer_r);
            if iw > 0.0 && ih > 0.0 {
                push_rrect(pb, il, it, iw, ih, inner_r);
            }
        })
    }

    // ----------------------------------------------------------------- text

    fn text(&mut self, transform: &Affine2, block: &TextBlock) {
        if block.lines.is_empty() || block.runs.is_empty() {
            return;
        }
        let ts = self.dev(transform);
        let clip_b = self.top().clip_bounds();
        if clip_b.is_empty() || !ts.is_finite() {
            return;
        }
        // Cull with a generous box around the lines.
        let max_size = block.runs.iter().map(|r| r.style.size).fold(0.0, f32::max);
        let mut b = Rect::new(0.0, 0.0, block.size.x, block.size.y);
        for l in &block.lines {
            b = b.union(&Rect::new(
                l.x,
                l.baseline - max_size * 1.5,
                l.width.max(0.0),
                max_size * 2.0,
            ));
        }
        if IRect::round_out(&map_rect(&ts, &outset(&b, max_size)))
            .intersect(&clip_b)
            .is_empty()
        {
            return;
        }
        if block.clip {
            self.push_clip(
                transform,
                &Shape::Rect {
                    size: block.size,
                    radii: Default::default(),
                },
            );
        }
        self.text_lines(&ts, block);
        if block.clip {
            self.pop_clip();
        }
    }

    fn text_lines(&mut self, ts: &Transform, block: &TextBlock) {
        let bounds = Rect::new(0.0, 0.0, block.size.x, block.size.y);
        let bitmap_ok =
            is_axis_aligned(ts) && ts.sx > 0.0 && ts.sy > 0.0 && (ts.sx - ts.sy).abs() < 1e-4;
        let dev_scale = ts.sx;
        let n_lines = block.lines.len();
        for (li, line) in block.lines.iter().enumerate() {
            let Some(shaped) = self.res.text_engine().shape_line(block, line) else {
                continue;
            };
            let spaces = shaped.glyphs.iter().filter(|g| g.is_space).count();
            let extra = if block.align == TextAlign::Justify
                && li + 1 < n_lines
                && spaces > 0
                && line.width > shaped.width
            {
                (line.width - shaped.width) / spaces as f32
            } else {
                0.0
            };

            let mut pending: Option<(u32, PathBuilder)> = None;
            let mut shift = 0.0;
            for g in &shaped.glyphs {
                let run = (g.run as usize).min(block.runs.len() - 1);
                let paint = &block.runs[run].style.paint;
                let x = line.x + g.x + shift;
                let y = line.baseline + g.y;
                if g.is_space {
                    shift += extra;
                }
                if bitmap_ok && let Paint::Solid(c) = paint {
                    self.flush_glyph_path(&mut pending, block, ts, &bounds);
                    self.blit_glyph(ts, g, x, y, dev_scale, *c);
                    continue;
                }
                if pending.as_ref().is_some_and(|(r, _)| *r != g.run) {
                    self.flush_glyph_path(&mut pending, block, ts, &bounds);
                }
                let pb = &mut pending.get_or_insert_with(|| (g.run, PathBuilder::new())).1;
                let engine = self.res.text_engine();
                let (key, _, _) = CacheKey::new(
                    g.font_id,
                    g.glyph_id,
                    g.size,
                    (0.0, 0.0),
                    g.weight,
                    g.flags | CacheKeyFlags::DISABLE_HINTING,
                );
                if let Some(cmds) = engine.swash.get_outline_commands(&mut engine.fs, key) {
                    append_outline(pb, cmds, x, y);
                }
            }
            self.flush_glyph_path(&mut pending, block, ts, &bounds);

            for d in &shaped.decos {
                let run = (d.run as usize).min(block.runs.len() - 1);
                let paint = block.runs[run].style.paint.clone();
                let r = Rect::new(
                    line.x + d.x0,
                    line.baseline + d.y - d.thickness * 0.5,
                    d.x1 - d.x0,
                    d.thickness,
                );
                if let Some(path) = self.res.path(|pb| {
                    if let Some(rr) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h) {
                        pb.push_rect(rr);
                    }
                }) {
                    self.fill_paint(
                        &path,
                        tiny_skia::FillRule::Winding,
                        &paint,
                        ts,
                        &bounds,
                        None,
                    );
                    self.res.recycle_path(path);
                }
            }
        }
    }

    fn flush_glyph_path(
        &mut self,
        pending: &mut Option<(u32, PathBuilder)>,
        block: &TextBlock,
        ts: &Transform,
        bounds: &Rect,
    ) {
        let Some((run, pb)) = pending.take() else {
            return;
        };
        let Some(path) = pb.finish() else { return };
        let run = (run as usize).min(block.runs.len() - 1);
        let paint = &block.runs[run].style.paint;
        self.fill_paint(&path, tiny_skia::FillRule::Winding, paint, ts, bounds, None);
    }

    /// Draws one glyph from the swash bitmap cache (axis-aligned transforms, solid paint).
    fn blit_glyph(
        &mut self,
        ts: &Transform,
        g: &crate::text::ShapedGlyph,
        x: f32,
        y: f32,
        scale: f32,
        color: Color,
    ) {
        let px = ts.sx * x + ts.tx;
        let py = ts.sy * y + ts.ty;
        let size = g.size * scale;
        if size.is_nan() || size <= 0.0 || !px.is_finite() || !py.is_finite() {
            return;
        }
        let (key, ix, iy) = CacheKey::new(
            g.font_id,
            g.glyph_id,
            size,
            (px, py.round()),
            g.weight,
            g.flags | CacheKeyFlags::DISABLE_HINTING,
        );
        let engine = self.res.text.as_mut().expect("text engine");
        let Some(img) = engine.swash.get_image(&mut engine.fs, key).as_ref() else {
            return;
        };
        let t = self.targets.last_mut().expect("target");
        let clip_b = t.clips.last().map_or(t.base, |c| c.bounds);
        let mask = t.clips.last().and_then(|c| c.mask.as_ref());
        blit(&mut t.pixmap, mask, clip_b, img, ix, iy, color);
    }
}

fn append_outline(pb: &mut PathBuilder, cmds: &[Command], x: f32, y: f32) {
    for c in cmds {
        match *c {
            Command::MoveTo(p) => pb.move_to(x + p.x, y - p.y),
            Command::LineTo(p) => pb.line_to(x + p.x, y - p.y),
            Command::QuadTo(c1, p) => pb.quad_to(x + c1.x, y - c1.y, x + p.x, y - p.y),
            Command::CurveTo(c1, c2, p) => {
                pb.cubic_to(x + c1.x, y - c1.y, x + c2.x, y - c2.y, x + p.x, y - p.y)
            }
            Command::Close => pb.close(),
        }
    }
}

#[inline]
fn blend_over(d: &mut [u8], src: [u32; 4]) {
    let inv = 255 - src[3];
    for c in 0..4 {
        d[c] = (src[c] + mul255(d[c] as u32, inv)).min(255) as u8;
    }
}

/// Src-over of a solid premultiplied `color` modulated by the alpha of `src` (scaled by `k`, bilinear).
fn composite_coverage(
    dst: &mut Pixmap,
    mask: Option<&Mask>,
    clip_b: IRect,
    src: &Pixmap,
    at: (i32, i32),
    k: u32,
    color: [u8; 4],
) {
    let (sw, sh) = (src.width() as i32, src.height() as i32);
    let k_i = k.max(1) as i32;
    let area = IRect::new(at.0, at.1, at.0 + sw * k_i, at.1 + sh * k_i)
        .intersect(&clip_b)
        .intersect(&IRect::from_size(dst.width(), dst.height()));
    if area.is_empty() {
        return;
    }
    let col = color.map(|c| c as u32);
    let dw = dst.width() as usize;
    let s = src.data();
    let (swu, shu) = (sw as usize, sh as usize);
    // Source sample position (fixed point, 8 fractional bits) for a destination offset.
    let pos = |d: i32| -> (usize, usize, u32) {
        let v = ((2 * d + 1) * 256) / (2 * k_i) - 128;
        let (i, f) = (v >> 8, (v & 255) as u32);
        (i.max(0) as usize, (i + 1).max(0) as usize, f)
    };
    // Horizontal sample table for the area (indices clamped to the source width).
    let xs: Vec<(usize, usize, u32)> = (area.x0..area.x1)
        .map(|x| {
            let (a, b, f) = pos(x - at.0);
            (a.min(swu - 1), b.min(swu - 1), f)
        })
        .collect();
    let mut row = vec![0u32; swu];
    let d = dst.data_mut();
    for y in area.y0..area.y1 {
        let (r0, r1, fy) = pos(y - at.1);
        let (r0, r1) = (r0.min(shu - 1), r1.min(shu - 1));
        for (sx, v) in row.iter_mut().enumerate() {
            let a = s[(r0 * swu + sx) * 4 + 3] as u32;
            let b = s[(r1 * swu + sx) * 4 + 3] as u32;
            *v = a * (256 - fy) + b * fy;
        }
        let drow = y as usize * dw;
        for (x, &(x0, x1, fx)) in (area.x0..area.x1).zip(&xs) {
            let a = (row[x0] * (256 - fx) + row[x1] * fx + (1 << 15)) >> 16;
            if a == 0 {
                continue;
            }
            let di = drow + x as usize;
            let cov = match mask {
                Some(m) => mul255(a, m.data()[di] as u32),
                None => a,
            };
            if cov == 0 {
                continue;
            }
            let src = [
                mul255(col[0], cov),
                mul255(col[1], cov),
                mul255(col[2], cov),
                mul255(col[3], cov),
            ];
            blend_over(&mut d[di * 4..di * 4 + 4], src);
        }
    }
}

/// Src-over blend of a glyph image (coverage or color) into premultiplied RGBA.
fn blit(
    pixmap: &mut Pixmap,
    mask: Option<&Mask>,
    clip_b: IRect,
    img: &SwashImage,
    gx: i32,
    gy: i32,
    color: Color,
) {
    let p = img.placement;
    let (x0, y0) = (gx + p.left, gy - p.top);
    let (w, h) = (p.width as i32, p.height as i32);
    let r = IRect::new(x0, y0, x0 + w, y0 + h).intersect(&clip_b);
    if r.is_empty() {
        return;
    }
    let pw = pixmap.width() as usize;
    let [cr, cg, cb, ca] = premul_u8(color);
    let alpha_k = (color.a.clamp(0.0, 1.0) * 255.0).round() as u32;
    let data = pixmap.data_mut();
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let si = ((y - y0) * w + (x - x0)) as usize;
            let di = y as usize * pw + x as usize;
            let clip = mask.map_or(255, |m| m.data()[di] as u32);
            if clip == 0 {
                continue;
            }
            let src = match img.content {
                SwashContent::Mask => {
                    let cov = mul255(img.data[si] as u32, clip);
                    if cov == 0 {
                        continue;
                    }
                    [
                        mul255(cr as u32, cov),
                        mul255(cg as u32, cov),
                        mul255(cb as u32, cov),
                        mul255(ca as u32, cov),
                    ]
                }
                SwashContent::SubpixelMask => {
                    let s = &img.data[si * 4..si * 4 + 3];
                    let cov = mul255((s[0] as u32 + s[1] as u32 + s[2] as u32) / 3, clip);
                    [
                        mul255(cr as u32, cov),
                        mul255(cg as u32, cov),
                        mul255(cb as u32, cov),
                        mul255(ca as u32, cov),
                    ]
                }
                SwashContent::Color => {
                    let s = &img.data[si * 4..si * 4 + 4];
                    let a = mul255(mul255(s[3] as u32, alpha_k), clip);
                    [
                        mul255(s[0] as u32, a),
                        mul255(s[1] as u32, a),
                        mul255(s[2] as u32, a),
                        a,
                    ]
                }
            };
            blend_over(&mut data[di * 4..di * 4 + 4], src);
        }
    }
}
