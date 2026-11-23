//! DisplayList -> SVG 1.1.
//!
//! Mapping (see `docs/pen-export.md` for the full table):
//! - shapes: `<rect>` (uniform radii, CSS-clamped), `<ellipse>`, `<path>` (per-corner radii as
//!   arcs, fill rule);
//! - paints: solid (`fill` + `fill-opacity`), linear/radial gradients (`userSpaceOnUse`, radial
//!   as a unit circle under `gradientTransform`), images (`<image>` data URIs referenced with
//!   `<use>`; clipped to the shape, or a `<pattern>` for tiles/strokes), angular gradients
//!   rasterized with pen-raster into a `<pattern>`;
//! - strokes: center as SVG strokes; inside/outside as double-width strokes clipped to the shape
//!   (or its complement); rectangles with per-side widths or non-center alignment as an exact
//!   even-odd ring, like pen-raster;
//! - shadows: blurred copies (`feGaussianBlur`) under the fills (drop) or masked + clipped over
//!   them (inner);
//! - clips: `<clipPath>` chained through `clip-path` on the clip path itself and applied to the
//!   leaves (never to a wrapper group, so blend modes still see their backdrop);
//! - layers: `<g opacity mix-blend-mode filter>`; backdrop blur is rasterized with pen-raster;
//! - text: `<text>`/`<tspan>` per pre-broken line, or glyph outlines ([`TextMode::Outlines`]).
//!
//! Output is deterministic: ids are sequential (`c1`, `g2`, ...) in paint order.

use crate::fmt::{Id, alpha, attr, attr_num, attr_url, escape, hex, num, num_p, transform_attr};
use crate::{ExportOptions, TextMode};
use base64::Engine as _;
use pen_raster::RasterContext;
use pen_raster::geom::{blur_sigma, clamp_radii, grow_radii, path_is_closed, shape_bounds};
use pen_render::{
    Affine2, BlendMode, Chunk, Color, CornerRadii, DisplayItem, DisplayList, FillRule,
    GradientStop, ImageData, ImageFit, LineCap, LineJoin, NodeKey, Paint, PathEl, Rect, Shadow,
    Shape, Stroke, StrokeAlign, TextBlock, Vec2,
};
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

/// Largest side (px) of a rasterized fallback (angular gradient, backdrop blur).
const MAX_RASTER_SIDE: f32 = 4096.0;

/// Exports `list` as an SVG document, using pen-raster's shared context for text outlines and
/// rasterized fallbacks.
pub fn export(list: &DisplayList, opts: &ExportOptions) -> String {
    pen_raster::with_shared_context(|ctx| export_with(ctx, list, opts))
}

/// Like [`export`] with a caller-owned raster context.
pub fn export_with(ctx: &mut RasterContext, list: &DisplayList, opts: &ExportOptions) -> String {
    export_impl(ctx, list, opts, None)
}

/// `families` maps document family names to the canonical names of a font database (PDF).
pub(crate) fn export_impl(
    ctx: &mut RasterContext,
    list: &DisplayList,
    opts: &ExportOptions,
    families: Option<&HashMap<String, String>>,
) -> String {
    let region = opts.resolve_region(list);
    let scale = opts.effective_scale();
    let mut w = Writer {
        ctx,
        opts,
        list,
        region,
        families,
        defs: String::new(),
        body: String::with_capacity(64 * 1024),
        next_id: 0,
        images: HashMap::new(),
        filters: HashMap::new(),
        clips: Vec::new(),
        layers: Vec::new(),
    };
    for (ci, chunk) in list.chunks.iter().enumerate() {
        if !opts.selects(chunk.root) || !chunk.bounds.intersects(&region) {
            continue;
        }
        w.chunk(ci, chunk);
    }

    let mut out = String::with_capacity(w.defs.len() + w.body.len() + 512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" version=\"1.1\"",
    );
    attr_num(&mut out, "width", region.w * scale);
    attr_num(&mut out, "height", region.h * scale);
    out.push_str(" viewBox=\"");
    num(&mut out, region.x);
    out.push(' ');
    num(&mut out, region.y);
    out.push(' ');
    num(&mut out, region.w);
    out.push(' ');
    num(&mut out, region.h);
    out.push_str("\">\n");
    if !w.defs.is_empty() {
        out.push_str("<defs>\n");
        out.push_str(&w.defs);
        out.push_str("</defs>\n");
    }
    if let Some(bg) = opts.background.filter(|c| alpha(*c) > 0.0) {
        out.push_str("<rect");
        attr_num(&mut out, "x", region.x);
        attr_num(&mut out, "y", region.y);
        attr_num(&mut out, "width", region.w);
        attr_num(&mut out, "height", region.h);
        paint_attrs(&mut out, "fill", &PaintRef::Color(bg));
        out.push_str("/>\n");
    }
    out.push_str(&w.body);
    out.push_str("</svg>\n");
    out
}

/// A resolved paint, ready to be written as `fill`/`stroke` attributes.
enum PaintRef {
    Color(Color),
    /// Gradient or pattern, with an extra opacity (image paints).
    Url(Id, f32),
}

fn paint_attrs(out: &mut String, which: &str, p: &PaintRef) {
    out.push(' ');
    out.push_str(which);
    out.push_str("=\"");
    let a = match p {
        PaintRef::Color(c) => {
            hex(out, *c);
            alpha(*c)
        }
        PaintRef::Url(id, a) => {
            out.push_str("url(#");
            id.write(out);
            out.push(')');
            *a
        }
    };
    out.push('"');
    if a < 1.0 {
        out.push(' ');
        out.push_str(which);
        out.push_str("-opacity=\"");
        num_p(out, a, 4);
        out.push('"');
    }
}

struct LayerScope {
    /// Clip stack depth when the layer was pushed (its group carries that clip).
    clip_len: usize,
}

struct Writer<'a> {
    ctx: &'a mut RasterContext,
    opts: &'a ExportOptions,
    list: &'a DisplayList,
    region: Rect,
    families: Option<&'a HashMap<String, String>>,
    defs: String,
    body: String,
    next_id: u32,
    /// `ImageData::id` -> `<image>` def (None: invalid image).
    images: HashMap<u64, Option<Id>>,
    /// Blur filters by (region, sigma) bits.
    filters: HashMap<[u32; 5], Id>,
    /// Active clips; each `<clipPath>` already intersects its parent.
    clips: Vec<Id>,
    layers: Vec<LayerScope>,
}

impl Writer<'_> {
    fn id(&mut self, prefix: char) -> Id {
        self.next_id += 1;
        Id(prefix, self.next_id)
    }

    fn chunk(&mut self, ci: usize, chunk: &Chunk) {
        self.clips.clear();
        self.layers.clear();
        for (ii, item) in chunk.items.iter().enumerate() {
            match item {
                DisplayItem::Shape {
                    transform,
                    shape,
                    fills,
                    stroke,
                    shadows,
                    ..
                } => self.shape_item(transform, shape, fills, stroke.as_ref(), shadows),
                DisplayItem::Text {
                    transform, block, ..
                } => self.text_item(transform, block),
                DisplayItem::PushClip { transform, shape } => self.push_clip(transform, shape),
                DisplayItem::PopClip => {
                    let floor = self.layers.last().map_or(0, |l| l.clip_len);
                    if self.clips.len() > floor {
                        self.clips.pop();
                    }
                }
                DisplayItem::PushLayer {
                    opacity,
                    blend,
                    blur,
                    backdrop_blur,
                    bounds,
                } => {
                    if *backdrop_blur > 0.0 && *opacity > 0.0 {
                        self.backdrop(ci, ii, *bounds, *backdrop_blur);
                    }
                    self.push_layer(*opacity, *blend, *blur, bounds);
                }
                DisplayItem::PopLayer => self.pop_layer(),
            }
        }
        while !self.layers.is_empty() {
            self.pop_layer();
        }
        self.clips.clear();
    }

    // ---------------------------------------------------------------- scopes

    /// The clip a leaf must apply itself (the enclosing layer group already applies the clip
    /// that was active when it was pushed).
    fn leaf_clip(&self) -> Option<Id> {
        let floor = self.layers.last().map_or(0, |l| l.clip_len);
        if self.clips.len() > floor {
            self.clips.last().copied()
        } else {
            None
        }
    }

    fn push_clip(&mut self, t: &Affine2, shape: &Shape) {
        let id = self.id('c');
        let d = &mut self.defs;
        d.push_str("<clipPath");
        id.attr(d);
        d.push_str(" clipPathUnits=\"userSpaceOnUse\"");
        if let Some(parent) = self.clips.last() {
            attr_url(d, "clip-path", *parent);
        }
        d.push('>');
        if t.is_finite() && geom_open(d, shape, 0.0, "clip-rule") {
            transform_attr(d, t);
            d.push_str("/>");
        }
        d.push_str("</clipPath>\n");
        self.clips.push(id);
    }

    fn push_layer(&mut self, opacity: f32, blend: BlendMode, blur: f32, bounds: &Rect) {
        let clip = self.leaf_clip();
        let b = &mut self.body;
        b.push_str("<g");
        if let Some(c) = clip {
            attr_url(b, "clip-path", c);
        }
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if opacity < 1.0 {
            b.push_str(" opacity=\"");
            num_p(b, opacity, 4);
            b.push('"');
        }
        if blend != BlendMode::Normal {
            attr(b, "style", blend_css(blend));
        }
        let sigma = blur_sigma(blur);
        if sigma > 0.0 {
            let region =
                (bounds.w > 0.0 && bounds.h > 0.0).then(|| outset(bounds, sigma * 3.0 + 2.0));
            let f = self.blur_filter(region, sigma);
            attr_url(&mut self.body, "filter", f);
        }
        self.body.push_str(">\n");
        self.layers.push(LayerScope {
            clip_len: self.clips.len(),
        });
    }

    fn pop_layer(&mut self) {
        if let Some(l) = self.layers.pop() {
            self.clips.truncate(l.clip_len);
            self.body.push_str("</g>\n");
        }
    }

    /// Opens the wrapper groups of a leaf item (clip, transform); returns how many to close.
    fn open_leaf(&mut self, t: &Affine2) -> usize {
        let mut n = 0;
        if let Some(c) = self.leaf_clip() {
            self.body.push_str("<g");
            attr_url(&mut self.body, "clip-path", c);
            self.body.push('>');
            n += 1;
        }
        if *t != Affine2::IDENTITY {
            self.body.push_str("<g");
            transform_attr(&mut self.body, t);
            self.body.push('>');
            n += 1;
        }
        n
    }

    fn close_leaf(&mut self, n: usize) {
        for _ in 0..n {
            self.body.push_str("</g>");
        }
        self.body.push('\n');
    }

    // ------------------------------------------------------------------ defs

    /// A `<clipPath>` of `shape` (local space, no parent clip).
    fn shape_clip(&mut self, shape: &Shape) -> Id {
        let id = self.id('c');
        let d = &mut self.defs;
        d.push_str("<clipPath");
        id.attr(d);
        d.push_str(" clipPathUnits=\"userSpaceOnUse\">");
        if geom_open(d, shape, 0.0, "clip-rule") {
            d.push_str("/>");
        }
        d.push_str("</clipPath>\n");
        id
    }

    fn blur_filter(&mut self, region: Option<Rect>, sigma: f32) -> Id {
        let key = match region {
            Some(r) => [r.x, r.y, r.w, r.h, sigma].map(|v| (v * 1000.0).round().to_bits()),
            None => [u32::MAX, 0, 0, 0, (sigma * 1000.0).round().to_bits()],
        };
        if let Some(id) = self.filters.get(&key) {
            return *id;
        }
        let id = self.id('f');
        let d = &mut self.defs;
        d.push_str("<filter");
        id.attr(d);
        match region {
            Some(r) => {
                d.push_str(" filterUnits=\"userSpaceOnUse\"");
                attr_num(d, "x", r.x);
                attr_num(d, "y", r.y);
                attr_num(d, "width", r.w);
                attr_num(d, "height", r.h);
            }
            None => d.push_str(" x=\"-50%\" y=\"-50%\" width=\"200%\" height=\"200%\""),
        }
        d.push_str(" color-interpolation-filters=\"sRGB\"><feGaussianBlur stdDeviation=\"");
        num(d, sigma);
        d.push_str("\"/></filter>\n");
        self.filters.insert(key, id);
        id
    }

    fn gradient_stops(&mut self, stops: &[GradientStop]) {
        let d = &mut self.defs;
        let mut last = 0.0f32;
        for s in stops {
            let off = if s.offset.is_finite() {
                s.offset.clamp(0.0, 1.0).max(last)
            } else {
                last
            };
            last = off;
            d.push_str("<stop offset=\"");
            num_p(d, off, 4);
            d.push_str("\" stop-color=\"");
            hex(d, s.color);
            d.push('"');
            let a = alpha(s.color);
            if a < 1.0 {
                d.push_str(" stop-opacity=\"");
                num_p(d, a, 4);
                d.push('"');
            }
            d.push_str("/>");
        }
    }

    /// The `<image>` def of `img` (PNG data URI), shared by every use.
    fn image_def(&mut self, img: &Arc<ImageData>) -> Option<Id> {
        if let Some(id) = self.images.get(&img.id) {
            return *id;
        }
        let png = encode_png(img.width, img.height, &img.rgba_premul);
        let id = png.map(|png| {
            let id = self.id('i');
            let d = &mut self.defs;
            d.push_str("<image");
            id.attr(d);
            attr_num(d, "width", img.width as f32);
            attr_num(d, "height", img.height as f32);
            d.push_str(" preserveAspectRatio=\"none\"");
            push_png_href(d, &png);
            d.push_str("/>\n");
            id
        });
        self.images.insert(img.id, id);
        id
    }

    /// A pattern drawing `img` mapped into `bounds` with `fit`; `area` is the region the paint
    /// must cover (non-tiled images are transparent outside their rectangle).
    fn image_pattern(
        &mut self,
        img: &Arc<ImageData>,
        fit: ImageFit,
        scale: f32,
        bounds: &Rect,
        area: &Rect,
    ) -> Option<Id> {
        let (iw, ih) = (img.width as f32, img.height as f32);
        let (sx, sy, tx, ty) = image_mapping(fit, scale, bounds, iw, ih);
        let iid = self.image_def(img)?;
        let tile = if fit == ImageFit::Tile {
            Rect::new(tx, ty, iw * sx, ih * sy)
        } else {
            area.union(&Rect::new(tx, ty, iw * sx, ih * sy))
        };
        if !(tile.w > 1e-3 && tile.h > 1e-3) || !(sx.is_finite() && sy.is_finite()) {
            return None;
        }
        let id = self.id('p');
        let d = &mut self.defs;
        d.push_str("<pattern");
        id.attr(d);
        d.push_str(" patternUnits=\"userSpaceOnUse\"");
        attr_num(d, "x", tile.x);
        attr_num(d, "y", tile.y);
        attr_num(d, "width", tile.w);
        attr_num(d, "height", tile.h);
        d.push('>');
        let t = Affine2::from_cols_array(&[sx, 0.0, 0.0, sy, tx - tile.x, ty - tile.y]);
        push_use(d, iid, &t);
        d.push_str("</pattern>\n");
        Some(id)
    }

    /// An angular gradient rasterized by pen-raster over `area` (local space), as a pattern.
    fn angular_pattern(
        &mut self,
        center: Vec2,
        start_angle: f32,
        stops: &[GradientStop],
        area: &Rect,
        device_scale: f32,
    ) -> Option<Id> {
        if !(area.w > 0.0 && area.h > 0.0 && area.w.is_finite() && area.h.is_finite()) {
            return None;
        }
        let k = (device_scale * self.opts.effective_scale() * self.opts.raster_scale)
            .clamp(0.01, 16.0)
            .min(MAX_RASTER_SIDE / area.w.max(area.h));
        let item = DisplayItem::Shape {
            node: NodeKey(0),
            transform: Affine2::from_translation(Vec2::new(area.x, area.y)),
            shape: Shape::Rect {
                size: Vec2::new(area.w, area.h),
                radii: CornerRadii::default(),
            },
            fills: smallvec::smallvec![Paint::Angular {
                center: center - Vec2::new(area.x, area.y),
                start_angle,
                stops: stops.iter().copied().collect(),
            }],
            stroke: None,
            shadows: smallvec::smallvec![],
        };
        let list = DisplayList::from_chunks([Chunk {
            root: NodeKey(0),
            bounds: *area,
            version: 0,
            items: vec![item],
        }]);
        let pix = self.ctx.render(&list, *area, k, Color::TRANSPARENT);
        let png = encode_png(pix.width(), pix.height(), pix.data())?;
        let id = self.id('p');
        let d = &mut self.defs;
        d.push_str("<pattern");
        id.attr(d);
        d.push_str(" patternUnits=\"userSpaceOnUse\"");
        attr_num(d, "x", area.x);
        attr_num(d, "y", area.y);
        attr_num(d, "width", area.w);
        attr_num(d, "height", area.h);
        d.push_str("><image");
        attr_num(d, "width", area.w);
        attr_num(d, "height", area.h);
        d.push_str(" preserveAspectRatio=\"none\"");
        push_png_href(d, &png);
        d.push_str("/></pattern>\n");
        Some(id)
    }

    /// Resolves `p` for an element whose paint box is `bounds` and whose painted area is
    /// `area` (both local). `None` = nothing to draw.
    fn paint_ref(
        &mut self,
        p: &Paint,
        bounds: &Rect,
        area: &Rect,
        device_scale: f32,
    ) -> Option<PaintRef> {
        match p {
            Paint::Solid(c) => (alpha(*c) > 0.0).then_some(PaintRef::Color(*c)),
            Paint::Linear { start, end, stops } => {
                let last = stops.last()?;
                if start.distance(*end) < 1e-6 || !(start.is_finite() && end.is_finite()) {
                    return Some(PaintRef::Color(last.color));
                }
                let id = self.id('g');
                let d = &mut self.defs;
                d.push_str("<linearGradient");
                id.attr(d);
                d.push_str(" gradientUnits=\"userSpaceOnUse\"");
                attr_num(d, "x1", start.x);
                attr_num(d, "y1", start.y);
                attr_num(d, "x2", end.x);
                attr_num(d, "y2", end.y);
                d.push('>');
                self.gradient_stops(stops);
                self.defs.push_str("</linearGradient>\n");
                Some(PaintRef::Url(id, 1.0))
            }
            Paint::Radial {
                center,
                radius,
                stops,
            } => {
                let last = stops.last()?;
                if radius.x.abs() < 1e-6 || radius.y.abs() < 1e-6 || !radius.is_finite() {
                    return Some(PaintRef::Color(last.color));
                }
                let id = self.id('g');
                let d = &mut self.defs;
                d.push_str("<radialGradient");
                id.attr(d);
                d.push_str(" gradientUnits=\"userSpaceOnUse\" cx=\"0\" cy=\"0\" r=\"1\"");
                transform_attr_named(
                    d,
                    "gradientTransform",
                    &Affine2::from_cols_array(&[radius.x, 0.0, 0.0, radius.y, center.x, center.y]),
                );
                d.push('>');
                self.gradient_stops(stops);
                self.defs.push_str("</radialGradient>\n");
                Some(PaintRef::Url(id, 1.0))
            }
            Paint::Angular {
                center,
                start_angle,
                stops,
            } => {
                if stops.is_empty() {
                    return None;
                }
                let id = self.angular_pattern(*center, *start_angle, stops, area, device_scale)?;
                Some(PaintRef::Url(id, 1.0))
            }
            Paint::Image {
                image,
                fit,
                opacity,
                scale,
            } => {
                if image.width == 0 || image.height == 0 || opacity.is_nan() || *opacity <= 0.0 {
                    return None;
                }
                let id = self.image_pattern(image, *fit, *scale, bounds, area)?;
                Some(PaintRef::Url(id, opacity.clamp(0.0, 1.0)))
            }
        }
    }

    // ---------------------------------------------------------------- shapes

    fn shape_item(
        &mut self,
        t: &Affine2,
        shape: &Shape,
        fills: &[Paint],
        stroke: Option<&Stroke>,
        shadows: &[Shadow],
    ) {
        if !t.is_finite() || shape_is_empty(shape) {
            return;
        }
        let visible = fills.iter().any(paint_maybe_visible)
            || stroke.is_some_and(|s| {
                s.widths.iter().any(|w| *w > 0.0) && s.paints.iter().any(paint_maybe_visible)
            })
            || shadows.iter().any(|s| match s {
                Shadow::Drop { color, .. } | Shadow::Inner { color, .. } => alpha(*color) > 0.0,
            });
        if !visible {
            return;
        }
        let ds = det_scale(t);
        let local = shape_bounds(shape);
        let n = self.open_leaf(t);
        for sh in shadows {
            if let Shadow::Drop {
                offset,
                blur,
                spread,
                color,
            } = sh
            {
                self.drop_shadow(shape, &local, *offset, *blur, *spread, *color);
            }
        }
        for f in fills {
            self.fill(shape, &local, f, ds);
        }
        for sh in shadows {
            if let Shadow::Inner {
                offset,
                blur,
                spread,
                color,
            } = sh
            {
                self.inner_shadow(shape, &local, *offset, *blur, *spread, *color);
            }
        }
        if let Some(s) = stroke {
            self.stroke(shape, &local, s, ds);
        }
        self.close_leaf(n);
    }

    fn fill(&mut self, shape: &Shape, local: &Rect, p: &Paint, ds: f32) {
        // Non-tiled images: the image clipped to the shape (exact, no pattern resampling).
        if let Paint::Image {
            image,
            fit,
            opacity,
            scale,
        } = p
            && *fit != ImageFit::Tile
        {
            if image.width == 0 || image.height == 0 || opacity.is_nan() || *opacity <= 0.0 {
                return;
            }
            let Some(iid) = self.image_def(image) else {
                return;
            };
            let (sx, sy, tx, ty) =
                image_mapping(*fit, *scale, local, image.width as f32, image.height as f32);
            let clip = self.shape_clip(shape);
            let b = &mut self.body;
            b.push_str("<g");
            attr_url(b, "clip-path", clip);
            if *opacity < 1.0 {
                b.push_str(" opacity=\"");
                num_p(b, *opacity, 4);
                b.push('"');
            }
            b.push('>');
            push_use(
                b,
                iid,
                &Affine2::from_cols_array(&[sx, 0.0, 0.0, sy, tx, ty]),
            );
            b.push_str("</g>");
            return;
        }
        let Some(pr) = self.paint_ref(p, local, local, ds) else {
            return;
        };
        let b = &mut self.body;
        if geom_open(b, shape, 0.0, "fill-rule") {
            paint_attrs(b, "fill", &pr);
            b.push_str("/>");
        }
    }

    fn drop_shadow(
        &mut self,
        shape: &Shape,
        local: &Rect,
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    ) {
        let a = alpha(color);
        if a <= 0.0 {
            return;
        }
        let sigma = blur_sigma(blur);
        let filter = (sigma >= 0.2).then(|| {
            self.blur_filter(
                Some(outset(local, spread.max(0.0) + sigma * 3.0 + 2.0)),
                sigma,
            )
        });
        let b = &mut self.body;
        b.push_str("<g");
        transform_attr(b, &Affine2::from_translation(offset));
        if let Some(f) = filter {
            attr_url(b, "filter", f);
        }
        if a < 1.0 {
            b.push_str(" opacity=\"");
            num_p(b, a, 4);
            b.push('"');
        }
        b.push('>');
        let opaque = Color { a: 1.0, ..color };
        match shape {
            Shape::Path(_) => {
                if geom_open(b, shape, 0.0, "fill-rule") {
                    paint_attrs(b, "fill", &PaintRef::Color(opaque));
                    if spread > 0.0 {
                        paint_attrs(b, "stroke", &PaintRef::Color(opaque));
                        attr_num(b, "stroke-width", spread * 2.0);
                        b.push_str(" stroke-linejoin=\"round\" stroke-linecap=\"round\"");
                    }
                    b.push_str("/>");
                }
            }
            _ => {
                if geom_open(b, shape, spread, "fill-rule") {
                    paint_attrs(b, "fill", &PaintRef::Color(opaque));
                    b.push_str("/>");
                }
            }
        }
        b.push_str("</g>");
    }

    fn inner_shadow(
        &mut self,
        shape: &Shape,
        local: &Rect,
        offset: Vec2,
        blur: f32,
        spread: f32,
        color: Color,
    ) {
        let a = alpha(color);
        if a <= 0.0 {
            return;
        }
        let sigma = blur_sigma(blur);
        let r = outset(
            local,
            sigma * 3.0 + 2.0 + offset.x.abs().max(offset.y.abs()) + spread.abs(),
        );
        // Mask: white everywhere except the shape shifted by `offset` and shrunk by `spread`.
        let m = self.id('m');
        let d = &mut self.defs;
        d.push_str("<mask");
        m.attr(d);
        d.push_str(" maskUnits=\"userSpaceOnUse\"");
        push_rect_attrs(d, &r);
        d.push_str("><rect");
        push_rect_attrs(d, &r);
        d.push_str(" fill=\"#fff\"/><g");
        transform_attr(d, &Affine2::from_translation(offset));
        d.push('>');
        match shape {
            Shape::Path(_) => {
                if geom_open(d, shape, 0.0, "fill-rule") {
                    d.push_str(" fill=\"#000\"");
                    if spread != 0.0 {
                        d.push_str(if spread > 0.0 {
                            " stroke=\"#fff\""
                        } else {
                            " stroke=\"#000\""
                        });
                        attr_num(d, "stroke-width", spread.abs() * 2.0);
                        d.push_str(" stroke-linejoin=\"round\" stroke-linecap=\"round\"");
                    }
                    d.push_str("/>");
                }
            }
            _ => {
                if geom_open(d, shape, -spread, "fill-rule") {
                    d.push_str(" fill=\"#000\"/>");
                }
            }
        }
        d.push_str("</g></mask>\n");
        let clip = self.shape_clip(shape);
        let filter = (sigma > 0.0).then(|| self.blur_filter(Some(r), sigma));
        let b = &mut self.body;
        b.push_str("<g");
        attr_url(b, "clip-path", clip);
        b.push_str("><g");
        if let Some(f) = filter {
            attr_url(b, "filter", f);
        }
        if a < 1.0 {
            b.push_str(" opacity=\"");
            num_p(b, a, 4);
            b.push('"');
        }
        b.push_str("><rect");
        push_rect_attrs(b, &r);
        paint_attrs(b, "fill", &PaintRef::Color(Color { a: 1.0, ..color }));
        attr_url(b, "mask", m);
        b.push_str("/></g></g>");
    }

    fn stroke(&mut self, shape: &Shape, local: &Rect, s: &Stroke, ds: f32) {
        let w_max = s.widths.iter().cloned().fold(0.0, f32::max);
        if w_max.is_nan() || w_max <= 0.0 || s.paints.is_empty() {
            return;
        }
        let closed = match shape {
            Shape::Path(p) => path_is_closed(p),
            _ => true,
        };
        let align = if closed { s.align } else { StrokeAlign::Center };
        let miter = if s.join == LineJoin::Miter {
            s.miter_limit.max(1.0)
        } else {
            1.0
        };
        let area = outset(local, w_max * 2.0 * miter + 1.0);

        // Rectangles: exact ring geometry (per-side widths, inside/outside), like pen-raster.
        if let Shape::Rect { size, radii } = shape {
            let bevel_sharp = s.join == LineJoin::Bevel && radii.is_zero();
            if s.dash.is_empty()
                && (!s.is_uniform() || align != StrokeAlign::Center)
                && !bevel_sharp
            {
                let mut ring = String::new();
                rect_ring_d(&mut ring, *size, radii.0, s.widths, align, s.join);
                if ring.is_empty() {
                    return;
                }
                for p in &s.paints {
                    let Some(pr) = self.paint_ref(p, local, &area, ds) else {
                        continue;
                    };
                    let b = &mut self.body;
                    b.push_str("<path d=\"");
                    b.push_str(&ring);
                    b.push_str("\" fill-rule=\"evenodd\"");
                    paint_attrs(b, "fill", &pr);
                    b.push_str("/>");
                }
                return;
            }
        }

        let w = if s.is_uniform() { s.widths[0] } else { w_max };
        let mut sa = String::new();
        attr_num(
            &mut sa,
            "stroke-width",
            if align == StrokeAlign::Center {
                w
            } else {
                w * 2.0
            },
        );
        match s.cap {
            LineCap::Butt => {}
            LineCap::Round => sa.push_str(" stroke-linecap=\"round\""),
            LineCap::Square => sa.push_str(" stroke-linecap=\"square\""),
        }
        match s.join {
            LineJoin::Miter => {
                let ml = if s.miter_limit > 0.0 {
                    s.miter_limit
                } else {
                    4.0
                };
                if ml != 4.0 {
                    attr_num(&mut sa, "stroke-miterlimit", ml.max(1.0));
                }
            }
            LineJoin::Round => sa.push_str(" stroke-linejoin=\"round\""),
            LineJoin::Bevel => sa.push_str(" stroke-linejoin=\"bevel\""),
        }
        if s.dash.iter().any(|v| *v > 0.0) {
            sa.push_str(" stroke-dasharray=\"");
            for (i, v) in s.dash.iter().enumerate() {
                if i > 0 {
                    sa.push(' ');
                }
                num(&mut sa, v.max(0.0));
            }
            sa.push('"');
        }

        let clip = match align {
            StrokeAlign::Center => None,
            StrokeAlign::Inside => Some(self.shape_clip(shape)),
            StrokeAlign::Outside => {
                let id = self.id('c');
                let big = outset(&area, 1.0);
                let d = &mut self.defs;
                d.push_str("<clipPath");
                id.attr(d);
                d.push_str(" clipPathUnits=\"userSpaceOnUse\"><path d=\"");
                rect_d(d, &big);
                d.push(' ');
                shape_d(d, shape, 0.0, Vec2::ZERO);
                d.push_str("\" clip-rule=\"evenodd\"/></clipPath>\n");
                Some(id)
            }
        };
        if let Some(c) = clip {
            self.body.push_str("<g");
            attr_url(&mut self.body, "clip-path", c);
            self.body.push('>');
        }
        for p in &s.paints {
            let Some(pr) = self.paint_ref(p, local, &area, ds) else {
                continue;
            };
            let b = &mut self.body;
            if geom_open(b, shape, 0.0, "fill-rule") {
                b.push_str(" fill=\"none\"");
                paint_attrs(b, "stroke", &pr);
                b.push_str(&sa);
                b.push_str("/>");
            }
        }
        if clip.is_some() {
            self.body.push_str("</g>");
        }
    }

    // ------------------------------------------------------------------ text

    fn text_item(&mut self, t: &Affine2, block: &TextBlock) {
        if block.lines.is_empty() || block.runs.is_empty() || !t.is_finite() {
            return;
        }
        let ds = det_scale(t);
        let bounds = Rect::new(0.0, 0.0, block.size.x, block.size.y);
        let max_size = block.runs.iter().map(|r| r.style.size).fold(0.0, f32::max);
        let area = outset(&bounds, max_size * 2.0).union(&outset(
            &block
                .lines
                .iter()
                .map(|l| Rect::new(l.x, l.baseline - max_size, l.width.max(0.0), max_size))
                .reduce(|a, b| a.union(&b))
                .unwrap_or(bounds),
            max_size,
        ));
        let n = self.open_leaf(t);
        let clip = block.clip.then(|| {
            self.shape_clip(&Shape::Rect {
                size: block.size,
                radii: CornerRadii::default(),
            })
        });
        if let Some(c) = clip {
            self.body.push_str("<g");
            attr_url(&mut self.body, "clip-path", c);
            self.body.push('>');
        }
        match self.opts.text {
            TextMode::Outlines => {
                let outlines = self.ctx.text_outlines(block);
                for o in &outlines {
                    let paint = &block.runs[o.run].style.paint;
                    let Some(pr) = self.paint_ref(paint, &bounds, &area, ds) else {
                        continue;
                    };
                    let b = &mut self.body;
                    b.push_str("<path d=\"");
                    path_d(b, &o.path.els, Vec2::ZERO);
                    b.push('"');
                    paint_attrs(b, "fill", &pr);
                    b.push_str("/>");
                }
            }
            TextMode::Text => {
                for line in &block.lines {
                    self.text_line(block, line, &bounds, &area, ds);
                }
            }
        }
        if clip.is_some() {
            self.body.push_str("</g>");
        }
        self.close_leaf(n);
    }

    fn text_line(
        &mut self,
        block: &TextBlock,
        line: &pen_render::TextLine,
        bounds: &Rect,
        area: &Rect,
        ds: f32,
    ) {
        let spans = line_spans(block, line);
        if spans.is_empty() {
            return;
        }
        let mut s = String::new();
        s.push_str("<text");
        attr_num(&mut s, "x", line.x);
        attr_num(&mut s, "y", line.baseline);
        s.push_str(" xml:space=\"preserve\">");
        let mut any = false;
        for (start, end, ri) in spans {
            let Some(text) = block.text.get(start..end) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let style = &block.runs[ri].style;
            let Some(pr) = self.paint_ref(&style.paint, bounds, area, ds) else {
                continue;
            };
            any = true;
            s.push_str("<tspan font-family=\"");
            self.font_family(&mut s, &style.family);
            s.push('"');
            attr_num(&mut s, "font-size", style.size.max(0.1));
            if style.weight != 400 {
                let _ = write!(s, " font-weight=\"{}\"", style.weight.clamp(1, 1000));
            }
            if style.italic {
                s.push_str(" font-style=\"italic\"");
            }
            if style.letter_spacing != 0.0 && style.letter_spacing.is_finite() {
                attr_num(&mut s, "letter-spacing", style.letter_spacing);
            }
            match (style.underline, style.strikethrough) {
                (true, true) => s.push_str(" text-decoration=\"underline line-through\""),
                (true, false) => s.push_str(" text-decoration=\"underline\""),
                (false, true) => s.push_str(" text-decoration=\"line-through\""),
                _ => {}
            }
            paint_attrs(&mut s, "fill", &pr);
            s.push('>');
            for ch in text.chars() {
                if ch != '\n' && ch != '\r' {
                    let mut buf = [0u8; 4];
                    escape(&mut s, ch.encode_utf8(&mut buf));
                }
            }
            s.push_str("</tspan>");
        }
        s.push_str("</text>");
        if any {
            self.body.push_str(&s);
        }
    }

    fn font_family(&self, out: &mut String, name: &str) {
        let lower = name.trim().to_ascii_lowercase();
        match lower.as_str() {
            "" | "sans-serif" | "system-ui" | "ui-sans-serif" => out.push_str("sans-serif"),
            "serif" | "ui-serif" => out.push_str("serif"),
            "monospace" | "ui-monospace" => out.push_str("monospace"),
            _ => {
                let name = self
                    .families
                    .and_then(|m| m.get(name))
                    .map_or(name.trim(), |s| s.as_str());
                out.push('\'');
                let clean: String = name.chars().filter(|c| *c != '\'' && *c != '"').collect();
                escape(out, &clean);
                out.push_str("', sans-serif");
            }
        }
    }

    // -------------------------------------------------------------- backdrop

    /// Backdrop blur: pen-raster renders everything painted before this layer (plus the blur)
    /// and the result is embedded as an image clipped to the layer bounds and the active clip.
    fn backdrop(&mut self, ci: usize, ii: usize, bounds: Rect, radius: f32) {
        if !(bounds.w > 0.0 && bounds.h > 0.0) {
            return;
        }
        let sigma = blur_sigma(radius);
        let area = outset(&bounds, sigma * 3.0 + 2.0);
        let list = self.list;
        let mut chunks: Vec<Arc<Chunk>> = list.chunks[..ci]
            .iter()
            .filter(|c| self.opts.selects(c.root) && c.bounds.intersects(&self.region))
            .cloned()
            .collect();
        let cur = &list.chunks[ci];
        let mut items = cur.items[..ii].to_vec();
        items.push(DisplayItem::PushLayer {
            opacity: 1.0,
            blend: BlendMode::Normal,
            blur: 0.0,
            backdrop_blur: radius,
            bounds,
        });
        items.push(DisplayItem::PopLayer);
        chunks.push(Arc::new(Chunk {
            root: cur.root,
            bounds: cur.bounds.union(&area),
            version: 0,
            items,
        }));
        // Only blurred pixels stay visible, so two samples per sigma are enough: backdrop
        // blurs are rendered (and blurred) at a reduced resolution.
        let k = (self.opts.effective_scale() * self.opts.raster_scale)
            .min(2.0 / sigma.max(1e-3))
            .clamp(0.01, 16.0)
            .min(MAX_RASTER_SIDE / area.w.max(area.h));
        let bg = self.opts.background.unwrap_or(Color::TRANSPARENT);
        let pix = self.ctx.render(&DisplayList { chunks }, area, k, bg);
        let Some(png) = encode_png(pix.width(), pix.height(), pix.data()) else {
            return;
        };
        let id = self.id('c');
        let d = &mut self.defs;
        d.push_str("<clipPath");
        id.attr(d);
        d.push_str(" clipPathUnits=\"userSpaceOnUse\"");
        if let Some(parent) = self.clips.last() {
            attr_url(d, "clip-path", *parent);
        }
        d.push_str("><rect");
        push_rect_attrs(d, &bounds);
        d.push_str("/></clipPath>\n");
        let b = &mut self.body;
        b.push_str("<g");
        attr_url(b, "clip-path", id);
        b.push_str("><image");
        push_rect_attrs(b, &area);
        b.push_str(" preserveAspectRatio=\"none\"");
        push_png_href(b, &png);
        b.push_str("/></g>\n");
    }
}

// ---------------------------------------------------------------- helpers

fn blend_css(b: BlendMode) -> &'static str {
    match b {
        BlendMode::Normal => "mix-blend-mode:normal",
        BlendMode::Multiply => "mix-blend-mode:multiply",
        BlendMode::Screen => "mix-blend-mode:screen",
        BlendMode::Overlay => "mix-blend-mode:overlay",
        BlendMode::Darken => "mix-blend-mode:darken",
        BlendMode::Lighten => "mix-blend-mode:lighten",
        BlendMode::ColorDodge => "mix-blend-mode:color-dodge",
        BlendMode::ColorBurn => "mix-blend-mode:color-burn",
        BlendMode::HardLight => "mix-blend-mode:hard-light",
        BlendMode::SoftLight => "mix-blend-mode:soft-light",
        BlendMode::Difference => "mix-blend-mode:difference",
        BlendMode::Exclusion => "mix-blend-mode:exclusion",
        BlendMode::Hue => "mix-blend-mode:hue",
        BlendMode::Saturation => "mix-blend-mode:saturation",
        BlendMode::Color => "mix-blend-mode:color",
        BlendMode::Luminosity => "mix-blend-mode:luminosity",
    }
}

fn paint_maybe_visible(p: &Paint) -> bool {
    match p {
        Paint::Solid(c) => alpha(*c) > 0.0,
        Paint::Image { opacity, .. } => *opacity > 0.0,
        Paint::Linear { stops, .. }
        | Paint::Radial { stops, .. }
        | Paint::Angular { stops, .. } => stops.iter().any(|s| alpha(s.color) > 0.0),
    }
}

fn shape_is_empty(shape: &Shape) -> bool {
    match shape {
        Shape::Rect { size, .. } | Shape::Ellipse { size } => !(size.x > 0.0 && size.y > 0.0),
        Shape::Path(p) => p.els.is_empty(),
    }
}

/// Geometric-mean scale of a transform.
fn det_scale(t: &Affine2) -> f32 {
    t.matrix2.determinant().abs().sqrt()
}

fn outset(r: &Rect, d: f32) -> Rect {
    Rect::new(r.x - d, r.y - d, r.w + 2.0 * d, r.h + 2.0 * d)
}

fn push_rect_attrs(out: &mut String, r: &Rect) {
    attr_num(out, "x", r.x);
    attr_num(out, "y", r.y);
    attr_num(out, "width", r.w);
    attr_num(out, "height", r.h);
}

fn push_png_href(out: &mut String, png: &[u8]) {
    out.push_str(" xlink:href=\"data:image/png;base64,");
    base64::engine::general_purpose::STANDARD.encode_string(png, out);
    out.push('"');
}

/// PNG (straight alpha; RGB when fully opaque) of premultiplied RGBA8 pixels, with fast
/// deflate: exports embed large rasters and the default level dominates their cost.
fn encode_png(w: u32, h: u32, premul: &[u8]) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || premul.len() != w as usize * h as usize * 4 {
        return None;
    }
    let px = premul.as_chunks::<4>().0;
    let opaque = px.iter().all(|p| p[3] == 255);
    let mut data = Vec::with_capacity(if opaque {
        premul.len() / 4 * 3
    } else {
        premul.len()
    });
    for p in px {
        let a = p[3] as u32;
        if opaque {
            data.extend_from_slice(&p[..3]);
        } else if a == 0 {
            data.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let un = |c: u8| ((c as u32 * 255 + a / 2) / a).min(255) as u8;
            data.extend_from_slice(&[un(p[0]), un(p[1]), un(p[2]), p[3]]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(if opaque {
            png::ColorType::Rgb
        } else {
            png::ColorType::Rgba
        });
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&data).ok()?;
    }
    Some(out)
}

fn push_use(out: &mut String, id: Id, t: &Affine2) {
    out.push_str("<use xlink:href=\"#");
    id.write(out);
    out.push('"');
    transform_attr(out, t);
    out.push_str("/>");
}

fn transform_attr_named(out: &mut String, name: &str, t: &Affine2) {
    let start = out.len();
    transform_attr(out, t);
    if out.len() > start {
        // transform_attr writes ` transform="..."`; rename the attribute.
        out.replace_range(start + 1..start + 1 + "transform".len(), name);
    } else {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"matrix(1 0 0 1 0 0)\"");
    }
}

/// Local-space mapping (scale x/y, translate) of an `iw x ih` image into `b` (same as pen-raster).
fn image_mapping(fit: ImageFit, scale: f32, b: &Rect, iw: f32, ih: f32) -> (f32, f32, f32, f32) {
    match fit {
        ImageFit::Stretch => (b.w / iw, b.h / ih, b.x, b.y),
        ImageFit::Cover | ImageFit::Contain => {
            let (sx, sy) = (b.w / iw, b.h / ih);
            let s = if fit == ImageFit::Cover {
                sx.max(sy)
            } else {
                sx.min(sy)
            };
            (s, s, b.x + (b.w - iw * s) * 0.5, b.y + (b.h - ih * s) * 0.5)
        }
        ImageFit::Tile => {
            let s = if scale > 0.0 && scale.is_finite() {
                scale
            } else {
                1.0
            };
            (s, s, b.x, b.y)
        }
    }
}

/// Writes the start of an element drawing `shape` grown by `grow` (without `/>`). Returns
/// false (writing nothing) when the geometry is empty. `rule` names the fill-rule attribute
/// (`fill-rule` or `clip-rule`).
fn geom_open(out: &mut String, shape: &Shape, grow: f32, rule: &str) -> bool {
    match shape {
        Shape::Rect { size, radii } => {
            let (x, y, w, h) = (-grow, -grow, size.x + 2.0 * grow, size.y + 2.0 * grow);
            if !(w > 0.0 && h > 0.0) {
                return false;
            }
            let r = clamp_radii(w, h, grow_radii(radii, grow));
            if r.iter().all(|v| *v == r[0]) {
                out.push_str("<rect");
                if x != 0.0 {
                    attr_num(out, "x", x);
                }
                if y != 0.0 {
                    attr_num(out, "y", y);
                }
                attr_num(out, "width", w);
                attr_num(out, "height", h);
                if r[0] > 0.0 {
                    attr_num(out, "rx", r[0]);
                }
            } else {
                out.push_str("<path d=\"");
                rrect_d(out, x, y, w, h, r);
                out.push('"');
            }
            true
        }
        Shape::Ellipse { size } => {
            let (rx, ry) = (size.x * 0.5 + grow, size.y * 0.5 + grow);
            if !(rx > 0.0 && ry > 0.0) {
                return false;
            }
            out.push_str("<ellipse");
            attr_num(out, "cx", size.x * 0.5);
            attr_num(out, "cy", size.y * 0.5);
            attr_num(out, "rx", rx);
            attr_num(out, "ry", ry);
            true
        }
        Shape::Path(p) => {
            if p.els.is_empty() {
                return false;
            }
            out.push_str("<path d=\"");
            path_d(out, &p.els, Vec2::ZERO);
            out.push('"');
            if p.fill_rule == FillRule::EvenOdd {
                attr(out, rule, "evenodd");
            }
            true
        }
    }
}

fn pt(out: &mut String, p: Vec2) {
    num(out, p.x);
    out.push(' ');
    num(out, p.y);
}

/// Path data of `els` translated by `off`.
fn path_d(out: &mut String, els: &[PathEl], off: Vec2) {
    for (i, el) in els.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match *el {
            PathEl::MoveTo(p) => {
                out.push('M');
                pt(out, p + off);
            }
            PathEl::LineTo(p) => {
                out.push('L');
                pt(out, p + off);
            }
            PathEl::QuadTo(c, p) => {
                out.push('Q');
                pt(out, c + off);
                out.push(' ');
                pt(out, p + off);
            }
            PathEl::CubicTo(c1, c2, p) => {
                out.push('C');
                pt(out, c1 + off);
                out.push(' ');
                pt(out, c2 + off);
                out.push(' ');
                pt(out, p + off);
            }
            PathEl::Close => out.push('Z'),
        }
    }
}

fn rect_d(out: &mut String, r: &Rect) {
    out.push('M');
    pt(out, Vec2::new(r.x, r.y));
    out.push('H');
    num(out, r.right());
    out.push('V');
    num(out, r.bottom());
    out.push('H');
    num(out, r.x);
    out.push('Z');
}

fn arc(out: &mut String, r: f32, to: Vec2) {
    out.push('A');
    num(out, r);
    out.push(' ');
    num(out, r);
    out.push_str(" 0 0 1 ");
    pt(out, to);
}

/// Rounded rect path (clockwise) with per-corner radii `[tl, tr, br, bl]`, CSS-clamped.
fn rrect_d(out: &mut String, x: f32, y: f32, w: f32, h: f32, radii: [f32; 4]) {
    if !(w > 0.0 && h > 0.0) {
        return;
    }
    let [tl, tr, br, bl] = clamp_radii(w, h, radii);
    let (r, b) = (x + w, y + h);
    out.push('M');
    pt(out, Vec2::new(x + tl, y));
    out.push('H');
    num(out, r - tr);
    if tr > 0.0 {
        arc(out, tr, Vec2::new(r, y + tr));
    }
    out.push('V');
    num(out, b - br);
    if br > 0.0 {
        arc(out, br, Vec2::new(r - br, b));
    }
    out.push('H');
    num(out, x + bl);
    if bl > 0.0 {
        arc(out, bl, Vec2::new(x, b - bl));
    }
    out.push('V');
    num(out, y + tl);
    if tl > 0.0 {
        arc(out, tl, Vec2::new(x + tl, y));
    }
    out.push('Z');
}

/// Path data of any shape grown by `grow` (paths ignore `grow`) and translated by `off`.
fn shape_d(out: &mut String, shape: &Shape, grow: f32, off: Vec2) {
    match shape {
        Shape::Rect { size, radii } => rrect_d(
            out,
            off.x - grow,
            off.y - grow,
            size.x + 2.0 * grow,
            size.y + 2.0 * grow,
            grow_radii(radii, grow),
        ),
        Shape::Ellipse { size } => {
            let (rx, ry) = (size.x * 0.5 + grow, size.y * 0.5 + grow);
            if !(rx > 0.0 && ry > 0.0) {
                return;
            }
            let (cx, cy) = (off.x + size.x * 0.5, off.y + size.y * 0.5);
            out.push('M');
            pt(out, Vec2::new(cx - rx, cy));
            for dest in [Vec2::new(cx + rx, cy), Vec2::new(cx - rx, cy)] {
                out.push('A');
                num(out, rx);
                out.push(' ');
                num(out, ry);
                out.push_str(" 0 1 1 ");
                pt(out, dest);
            }
            out.push('Z');
        }
        Shape::Path(p) => path_d(out, &p.els, off),
    }
}

/// Even-odd ring between the outer and inner edge of a rect stroke (port of pen-raster's).
fn rect_ring_d(
    out: &mut String,
    size: Vec2,
    radii: [f32; 4],
    widths: [f32; 4],
    align: StrokeAlign,
    join: LineJoin,
) {
    let [t, r, b, l] = widths.map(|w| if w.is_finite() { w.max(0.0) } else { 0.0 });
    let (out_k, in_k) = match align {
        StrokeAlign::Inside => (0.0, 1.0),
        StrokeAlign::Center => (0.5, 0.5),
        StrokeAlign::Outside => (1.0, 0.0),
    };
    let (ot, or, ob, ol) = (t * out_k, r * out_k, b * out_k, l * out_k);
    let (it, ir, ib, il) = (t * in_k, r * in_k, b * in_k, l * in_k);
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
    rrect_d(out, -ol, -ot, size.x + ol + or, size.y + ot + ob, outer_r);
    let (iw, ih) = (size.x - il - ir, size.y - it - ib);
    if iw > 0.0 && ih > 0.0 && !out.is_empty() {
        out.push(' ');
        rrect_d(out, il, it, iw, ih, inner_r);
    }
}

/// Styled spans `(start, end, run)` of one line: gaps between runs take the previous run's
/// style (same rule as pen-raster's shaper).
fn line_spans(block: &TextBlock, line: &pen_render::TextLine) -> Vec<(usize, usize, usize)> {
    let mut spans = Vec::new();
    if block.text.get(line.range.clone()).is_none() {
        return spans;
    }
    let mut cursor = line.range.start;
    let mut last_run = 0usize;
    for (i, run) in block.runs.iter().enumerate() {
        let s = run.range.start.max(line.range.start);
        let e = run.range.end.min(line.range.end);
        if s >= e {
            if run.range.end <= line.range.start {
                last_run = i;
            }
            continue;
        }
        if s > cursor {
            spans.push((cursor, s, last_run));
        }
        if s >= cursor {
            spans.push((s, e, i));
            cursor = e;
        }
        last_run = i;
    }
    if cursor < line.range.end {
        spans.push((cursor, line.range.end, last_run));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrect_uses_arcs_and_clamps() {
        let mut s = String::new();
        rrect_d(&mut s, 0.0, 0.0, 10.0, 4.0, [10.0, 0.0, 0.0, 0.0]);
        assert_eq!(s, "M4 0H10V4H0V4A4 4 0 0 1 4 0Z");
    }

    #[test]
    fn spans_fill_gaps_with_previous_run() {
        let style = pen_render::TextStyle {
            family: "Inter".into(),
            size: 12.0,
            weight: 400,
            italic: false,
            letter_spacing: 0.0,
            underline: false,
            strikethrough: false,
            paint: Paint::Solid(Color::BLACK),
        };
        let block = TextBlock {
            text: "abcdef".into(),
            runs: smallvec::smallvec![
                pen_render::TextRun {
                    range: 0..2,
                    style: style.clone()
                },
                pen_render::TextRun { range: 4..6, style },
            ],
            lines: vec![],
            align: Default::default(),
            size: Vec2::ZERO,
            clip: false,
        };
        let line = pen_render::TextLine {
            range: 0..6,
            x: 0.0,
            baseline: 0.0,
            width: 0.0,
        };
        assert_eq!(
            line_spans(&block, &line),
            vec![(0, 2, 0), (2, 4, 0), (4, 6, 1)]
        );
    }
}
