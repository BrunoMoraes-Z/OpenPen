//! Pure geometry helpers (no GPUI): transform classification, rect/radius mapping, lyon
//! path building and tessellation, gradient parameter mapping, image fitting.

use lyon::math::{Box2D, point, vector};
use lyon::path::builder::BorderRadii;
use lyon::path::{Path as LyonPath, Winding};
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, LineCap as LCap, LineJoin as LJoin,
    StrokeOptions, StrokeTessellator, StrokeVertex, VertexBuffers,
};
use pen_render::{
    Affine2, Color, CornerRadii, DisplayItem, FillRule, GradientStop, ImageFit, LineCap, LineJoin,
    Paint, PathEl, Rect, Shadow, Shape, Stroke, Vec2,
};

const EPS: f32 = 1e-4;

/// Uniform scale factor of an affine transform (sqrt of |det|).
pub fn transform_scale(t: &Affine2) -> f32 {
    t.matrix2.determinant().abs().sqrt()
}

/// True if `t` maps axis-aligned rects to axis-aligned rects (scale, flips, 90° rotations).
pub fn is_axis_aligned(t: &Affine2) -> bool {
    let m = t.matrix2;
    let s = transform_scale(t).max(1e-12);
    let (a, b, c, d) = (
        m.x_axis.x / s,
        m.x_axis.y / s,
        m.y_axis.x / s,
        m.y_axis.y / s,
    );
    (b.abs() < EPS && c.abs() < EPS) || (a.abs() < EPS && d.abs() < EPS)
}

/// CSS-style radius clamping: scale all radii down so adjacent radii never exceed a side.
pub fn clamp_radii(w: f32, h: f32, r: [f32; 4]) -> [f32; 4] {
    let r = r.map(|v| v.max(0.0));
    let mut f: f32 = 1.0;
    let mut check = |side: f32, a: f32, b: f32| {
        if a + b > 0.0 {
            f = f.min(side.max(0.0) / (a + b));
        }
    };
    check(w, r[0], r[1]);
    check(w, r[3], r[2]);
    check(h, r[0], r[3]);
    check(h, r[1], r[2]);
    r.map(|v| v * f)
}

/// If `t` keeps the rect `(0,0,size)` axis-aligned, return its bounds in target space and the
/// per-corner radii (TL, TR, BR, BL in target space) after the transform. Non-uniform scales
/// map radii by the geometric mean scale (approximation of elliptical corners).
pub fn axis_aligned_rect(t: &Affine2, size: Vec2, radii: &CornerRadii) -> Option<(Rect, [f32; 4])> {
    if !is_axis_aligned(t) {
        return None;
    }
    let local = [
        Vec2::ZERO,
        Vec2::new(size.x, 0.0),
        size,
        Vec2::new(0.0, size.y),
    ];
    let pts = local.map(|p| t.transform_point2(p));
    let bounds = Rect::new(0.0, 0.0, size.x, size.y).transformed(t);
    let c = Vec2::new(bounds.x + bounds.w * 0.5, bounds.y + bounds.h * 0.5);
    let s = transform_scale(t);
    let r = clamp_radii(size.x, size.y, radii.0);
    let mut out = [0.0; 4];
    for (i, p) in pts.iter().enumerate() {
        let left = p.x <= c.x;
        let top = p.y <= c.y;
        let q = match (left, top) {
            (true, true) => 0,
            (false, true) => 1,
            (false, false) => 2,
            (true, false) => 3,
        };
        out[q] = r[i] * s;
    }
    Some((bounds, out))
}

/// Local-space bounding box of a shape's geometry.
/// Rect / circle shapes that stay axis-aligned under `t` and are drawn as one GPUI quad:
/// target-space rect and corner radii.
pub fn quad_geom(t: &Affine2, shape: &Shape) -> Option<(Rect, [f32; 4])> {
    match shape {
        Shape::Rect { size, radii } => axis_aligned_rect(t, *size, radii),
        Shape::Ellipse { size } if (size.x - size.y).abs() <= 1e-3 * size.x.abs().max(1.0) => {
            axis_aligned_rect(t, *size, &CornerRadii::uniform(size.x * 0.5))
        }
        _ => None,
    }
}

pub fn shape_local_bounds(shape: &Shape) -> Rect {
    match shape {
        Shape::Rect { size, .. } | Shape::Ellipse { size } => Rect::new(0.0, 0.0, size.x, size.y),
        Shape::Path(p) => {
            let mut min = Vec2::splat(f32::INFINITY);
            let mut max = Vec2::splat(f32::NEG_INFINITY);
            let mut add = |v: Vec2| {
                min = min.min(v);
                max = max.max(v);
            };
            for el in &p.els {
                match *el {
                    PathEl::MoveTo(a) | PathEl::LineTo(a) => add(a),
                    PathEl::QuadTo(a, b) => {
                        add(a);
                        add(b)
                    }
                    PathEl::CubicTo(a, b, c) => {
                        add(a);
                        add(b);
                        add(c)
                    }
                    PathEl::Close => {}
                }
            }
            if min.x > max.x {
                Rect::default()
            } else {
                Rect::new(min.x, min.y, max.x - min.x, max.y - min.y)
            }
        }
    }
}

fn outset(r: &Rect, d: f32) -> Rect {
    Rect::new(r.x - d, r.y - d, r.w + 2.0 * d, r.h + 2.0 * d)
}

/// Document-space bounds of a drawable item (strokes, shadows and text overflow included).
/// `None` for scope items (push/pop).
pub fn item_doc_bounds(item: &DisplayItem) -> Option<Rect> {
    match item {
        DisplayItem::Shape {
            transform,
            shape,
            stroke,
            shadows,
            ..
        } => {
            let mut local = shape_local_bounds(shape);
            if let Some(s) = stroke {
                let w = s.widths.iter().copied().fold(0.0, f32::max);
                let k = match s.align {
                    pen_render::StrokeAlign::Inside => 0.0,
                    pen_render::StrokeAlign::Center => 0.5,
                    pen_render::StrokeAlign::Outside => 1.0,
                };
                // Miter joins can poke out further on paths; be generous there.
                let extra = if matches!(shape, Shape::Path(_)) {
                    w * s.miter_limit.max(1.0) * 0.5
                } else {
                    w * k
                };
                local = outset(&local, extra.max(w * k));
            }
            let mut b = local;
            for sh in shadows {
                if let Shadow::Drop {
                    offset,
                    blur,
                    spread,
                    ..
                } = sh
                {
                    let mut r = outset(&local, spread.max(0.0) + blur * 1.5);
                    r.x += offset.x;
                    r.y += offset.y;
                    b = b.union(&r);
                }
            }
            Some(b.transformed(transform))
        }
        DisplayItem::Text {
            transform, block, ..
        } => {
            let mut b = Rect::new(0.0, 0.0, block.size.x, block.size.y);
            if !block.clip {
                let size = block.runs.iter().map(|r| r.style.size).fold(0.0, f32::max);
                for l in &block.lines {
                    let lr = Rect::new(l.x, l.baseline - size * 1.1, l.width.max(0.0), size * 1.4);
                    b = b.union(&lr);
                }
            }
            Some(b.transformed(transform))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Tessellation

/// Tessellated triangles in the item's local space (3 vertices per triangle).
#[derive(Debug, Default, Clone)]
pub struct Tess {
    pub tris: Vec<Vec2>,
    /// Local-space tolerance used.
    pub tol: f32,
}

/// Screen-space tolerance we aim for when (re)tessellating.
pub const TARGET_TOL_PX: f32 = 0.1;

/// Tolerance to use in local space given the local→screen scale.
pub fn pick_tolerance(scale: f32) -> f32 {
    TARGET_TOL_PX / scale.max(1e-6)
}

/// Whether a tessellation made with local tolerance `tol` is still good at `scale`.
/// Hysteresis band [0.02, 0.5] px (25x zoom range) avoids re-tessellating on every zoom step.
pub fn tess_valid(tol: f32, scale: f32) -> bool {
    let px = tol * scale;
    (0.02..=0.5).contains(&px)
}

fn lp(v: Vec2) -> lyon::math::Point {
    point(v.x, v.y)
}

/// Build a lyon path for `shape` in local space. `grow` outsets rect/ellipse geometry
/// (negative = inset), used for stroke alignment.
pub fn lyon_path(shape: &Shape, grow: f32) -> LyonPath {
    let mut b = LyonPath::builder();
    match shape {
        Shape::Rect { size, radii } => {
            let r = clamp_radii(size.x, size.y, radii.0);
            let min = point(-grow, -grow);
            let max = point(size.x + grow, size.y + grow);
            if max.x <= min.x || max.y <= min.y {
                return b.build();
            }
            let adj = |v: f32| if v > 0.0 { (v + grow).max(0.0) } else { 0.0 };
            if r.iter().all(|v| *v <= 0.0) {
                b.add_rectangle(&Box2D::new(min, max), Winding::Positive);
            } else {
                b.add_rounded_rectangle(
                    &Box2D::new(min, max),
                    &BorderRadii {
                        top_left: adj(r[0]),
                        top_right: adj(r[1]),
                        bottom_right: adj(r[2]),
                        bottom_left: adj(r[3]),
                    },
                    Winding::Positive,
                );
            }
        }
        Shape::Ellipse { size } => {
            let rx = size.x * 0.5 + grow;
            let ry = size.y * 0.5 + grow;
            if rx > 0.0 && ry > 0.0 {
                b.add_ellipse(
                    point(size.x * 0.5, size.y * 0.5),
                    vector(rx, ry),
                    lyon::math::Angle::radians(0.0),
                    Winding::Positive,
                );
            }
        }
        Shape::Path(p) => {
            let mut open = false;
            let mut cur = Vec2::ZERO;
            for el in &p.els {
                match *el {
                    PathEl::MoveTo(a) => {
                        if open {
                            b.end(false);
                        }
                        b.begin(lp(a));
                        open = true;
                        cur = a;
                    }
                    PathEl::LineTo(a) => {
                        if !open {
                            b.begin(lp(cur));
                            open = true;
                        }
                        b.line_to(lp(a));
                        cur = a;
                    }
                    PathEl::QuadTo(c, a) => {
                        if !open {
                            b.begin(lp(cur));
                            open = true;
                        }
                        b.quadratic_bezier_to(lp(c), lp(a));
                        cur = a;
                    }
                    PathEl::CubicTo(c1, c2, a) => {
                        if !open {
                            b.begin(lp(cur));
                            open = true;
                        }
                        b.cubic_bezier_to(lp(c1), lp(c2), lp(a));
                        cur = a;
                    }
                    PathEl::Close => {
                        if open {
                            b.end(true);
                            open = false;
                        }
                    }
                }
            }
            if open {
                b.end(false);
            }
        }
    }
    b.build()
}

pub fn fill_rule_of(shape: &Shape) -> FillRule {
    match shape {
        Shape::Path(p) => p.fill_rule,
        _ => FillRule::NonZero,
    }
}

/// Tessellate the interior of `path` into `out` (cleared first).
pub fn tessellate_fill(path: &LyonPath, rule: FillRule, tol: f32, out: &mut Vec<Vec2>) {
    out.clear();
    let mut buf: VertexBuffers<Vec2, u32> = VertexBuffers::new();
    let opts = FillOptions::tolerance(tol).with_fill_rule(match rule {
        FillRule::NonZero => lyon::tessellation::FillRule::NonZero,
        FillRule::EvenOdd => lyon::tessellation::FillRule::EvenOdd,
    });
    let res = FillTessellator::new().tessellate_path(
        path,
        &opts,
        &mut BuffersBuilder::new(&mut buf, |v: FillVertex| {
            let p = v.position();
            Vec2::new(p.x, p.y)
        }),
    );
    if res.is_ok() {
        out.extend(buf.indices.iter().map(|i| buf.vertices[*i as usize]));
    }
}

/// Upper bound on dash segments per path; beyond this the stroke is drawn solid.
const MAX_DASHES: f32 = 20_000.0;

/// Split `path` into dashes (lengths in local units). Returns `None` when the pattern is
/// degenerate or would produce too many segments.
pub fn dash_path(path: &LyonPath, dash: &[f32], tol: f32) -> Option<LyonPath> {
    use lyon::algorithms::measure::{PathMeasurements, SampleType};
    let period: f32 = dash.iter().map(|d| d.max(0.0)).sum();
    if dash.is_empty() || period <= 1e-3 {
        return None;
    }
    let m = PathMeasurements::from_path(path, tol.max(1e-3));
    let mut sampler = m.create_sampler(path, SampleType::Distance);
    let total = sampler.length();
    if total / period * dash.len() as f32 > MAX_DASHES {
        return None;
    }
    let mut b = LyonPath::builder();
    let (mut pos, mut i) = (0.0f32, 0usize);
    while pos < total {
        let len = dash[i % dash.len()].max(0.0);
        let next = (pos + len).min(total);
        if i % 2 == 0 && next > pos {
            sampler.split_range(pos..next, &mut b);
        }
        pos = next;
        i += 1;
    }
    Some(b.build())
}

/// Tessellate the outline of `path` (already aligned) with the stroke style into `out`.
pub fn tessellate_stroke(
    path: &LyonPath,
    stroke: &Stroke,
    width: f32,
    tol: f32,
    out: &mut Vec<Vec2>,
) {
    out.clear();
    if width <= 0.0 {
        return;
    }
    let dashed;
    let path = if stroke.dash.iter().any(|d| *d > 0.0) {
        match dash_path(path, &stroke.dash, tol) {
            Some(p) => {
                dashed = p;
                &dashed
            }
            None => path,
        }
    } else {
        path
    };
    let opts = StrokeOptions::tolerance(tol)
        .with_line_width(width)
        .with_line_cap(match stroke.cap {
            LineCap::Butt => LCap::Butt,
            LineCap::Round => LCap::Round,
            LineCap::Square => LCap::Square,
        })
        .with_line_join(match stroke.join {
            LineJoin::Miter => LJoin::Miter,
            LineJoin::Round => LJoin::Round,
            LineJoin::Bevel => LJoin::Bevel,
        })
        .with_miter_limit(stroke.miter_limit.max(1.0));
    let mut buf: VertexBuffers<Vec2, u32> = VertexBuffers::new();
    let res = StrokeTessellator::new().tessellate_path(
        path,
        &opts,
        &mut BuffersBuilder::new(&mut buf, |v: StrokeVertex| {
            let p = v.position();
            Vec2::new(p.x, p.y)
        }),
    );
    if res.is_ok() {
        out.extend(buf.indices.iter().map(|i| buf.vertices[*i as usize]));
    }
}

/// How much to grow rect/ellipse geometry so a centered lyon stroke matches `align`.
/// Paths cannot be offset cheaply, so they are always stroked centered (approximation).
pub fn stroke_grow(shape: &Shape, stroke: &Stroke, width: f32) -> f32 {
    if matches!(shape, Shape::Path(_)) {
        return 0.0;
    }
    match stroke.align {
        pen_render::StrokeAlign::Inside => -width * 0.5,
        pen_render::StrokeAlign::Center => 0.0,
        pen_render::StrokeAlign::Outside => width * 0.5,
    }
}

// ---------------------------------------------------------------------------------------------
// Gradients

/// GPUI linear gradient parameters: CSS angle in degrees + the two stop percentages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuiLinear {
    pub angle_deg: f32,
    pub pct0: f32,
    pub pct1: f32,
}

fn gpui_dir(bounds: &Rect, angle_deg: f32) -> Vec2 {
    let rad = ((angle_deg % 360.0) - 90.0).to_radians();
    let mut dir = Vec2::new(rad.cos(), rad.sin());
    if bounds.w > bounds.h {
        dir.y *= bounds.h / bounds.w;
    } else {
        dir.x *= bounds.w / bounds.h;
    }
    dir
}

/// Reimplementation of GPUI's shader `t` for a linear gradient (before stop remapping).
pub fn gpui_linear_t(bounds: &Rect, angle_deg: f32, p: Vec2) -> f32 {
    let dir = gpui_dir(bounds, angle_deg);
    let half = Vec2::new(bounds.w, bounds.h) * 0.5;
    let c = Vec2::new(bounds.x, bounds.y) + half;
    let t = (p - c).dot(dir) / dir.length();
    if dir.x.abs() > dir.y.abs() {
        (t + half.x) / bounds.w
    } else {
        (t + half.y) / bounds.h
    }
}

/// Map a gradient from `p0` to `p1` (same space as `bounds`), with stops at offsets `o0` and
/// `o1`, onto GPUI's angle-based two-stop linear gradient evaluated over `bounds`.
pub fn linear_to_gpui(bounds: &Rect, p0: Vec2, p1: Vec2, o0: f32, o1: f32) -> Option<GpuiLinear> {
    let d = p1 - p0;
    if d.length_squared() < 1e-12 || bounds.w <= 1e-6 || bounds.h <= 1e-6 {
        return None;
    }
    let (sx, sy) = if bounds.w > bounds.h {
        (1.0, bounds.h / bounds.w)
    } else {
        (bounds.w / bounds.h, 1.0)
    };
    let theta = (d.y / sy).atan2(d.x / sx);
    let angle_deg = (theta.to_degrees() + 90.0).rem_euclid(360.0);
    let pct0 = gpui_linear_t(bounds, angle_deg, p0 + d * o0);
    let pct1 = gpui_linear_t(bounds, angle_deg, p0 + d * o1);
    if (pct1 - pct0).abs() < 1e-7 {
        return None;
    }
    Some(GpuiLinear {
        angle_deg,
        pct0,
        pct1,
    })
}

pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    Color::rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

/// Evaluate sorted-or-not stops at `t` (pad extend mode).
pub fn sample_stops(stops: &[GradientStop], t: f32) -> Color {
    match stops.len() {
        0 => Color::TRANSPARENT,
        1 => stops[0].color,
        _ => {
            if t <= stops[0].offset {
                return stops[0].color;
            }
            for w in stops.windows(2) {
                if t <= w[1].offset {
                    let span = w[1].offset - w[0].offset;
                    let k = if span <= 1e-6 {
                        1.0
                    } else {
                        (t - w[0].offset) / span
                    };
                    return lerp_color(w[0].color, w[1].color, k.clamp(0.0, 1.0));
                }
            }
            stops[stops.len() - 1].color
        }
    }
}

/// Coverage-weighted average color of a gradient (used where gradients are unsupported).
pub fn average_stop_color(stops: &[GradientStop]) -> Color {
    if stops.is_empty() {
        return Color::TRANSPARENT;
    }
    let n = 16;
    let mut acc = [0.0f32; 4];
    for i in 0..n {
        let c = sample_stops(stops, (i as f32 + 0.5) / n as f32);
        acc[0] += c.r;
        acc[1] += c.g;
        acc[2] += c.b;
        acc[3] += c.a;
    }
    let k = 1.0 / n as f32;
    Color::rgba(acc[0] * k, acc[1] * k, acc[2] * k, acc[3] * k)
}

/// Gradient parameter `t` at local point `p` for non-image paints.
pub fn gradient_t(paint: &Paint, p: Vec2) -> f32 {
    match paint {
        Paint::Linear { start, end, .. } => {
            let d = *end - *start;
            let l2 = d.length_squared();
            if l2 < 1e-12 {
                0.0
            } else {
                (p - *start).dot(d) / l2
            }
        }
        Paint::Radial { center, radius, .. } => {
            let r = radius.max(Vec2::splat(1e-6));
            ((p - *center) / r).length()
        }
        Paint::Angular {
            center,
            start_angle,
            ..
        } => {
            let v = p - *center;
            let a = v.y.atan2(v.x) - start_angle;
            a.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
        }
        _ => 0.0,
    }
}

pub fn paint_stops(paint: &Paint) -> &[GradientStop] {
    match paint {
        Paint::Linear { stops, .. }
        | Paint::Radial { stops, .. }
        | Paint::Angular { stops, .. } => stops,
        _ => &[],
    }
}

/// Rasterize a gradient paint into a `w`×`h` BGRA (straight alpha) texture that covers
/// `target` (in the space of `to_local`'s input), multiplying alpha by `alpha`.
pub fn gradient_texture(
    paint: &Paint,
    target: &Rect,
    to_local: &Affine2,
    w: u32,
    h: u32,
    alpha: f32,
) -> Vec<u8> {
    let stops = paint_stops(paint);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for j in 0..h {
        for i in 0..w {
            let p = Vec2::new(
                target.x + (i as f32 + 0.5) / w as f32 * target.w,
                target.y + (j as f32 + 0.5) / h as f32 * target.h,
            );
            let c = sample_stops(stops, gradient_t(paint, to_local.transform_point2(p)));
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            out.extend_from_slice(&[q(c.b), q(c.g), q(c.r), q(c.a * alpha)]);
        }
    }
    out
}

/// Texture size for a gradient covering `screen_w`×`screen_h`: proportional, capped.
pub fn gradient_texture_size(local_w: f32, local_h: f32) -> (u32, u32) {
    let clamp = |v: f32| (v.ceil() as u32).clamp(2, 256);
    (clamp(local_w), clamp(local_h))
}

// ---------------------------------------------------------------------------------------------
// Images

/// Where the image lands for a given fit (same space as `bounds`). For `Tile`, returns the
/// first tile (origin at bounds origin, natural size × `scale` × `px_per_unit`).
pub fn image_fit_rect(
    bounds: &Rect,
    img_w: f32,
    img_h: f32,
    fit: ImageFit,
    tile_scale: f32,
    px_per_unit: f32,
) -> Rect {
    let (iw, ih) = (img_w.max(1.0), img_h.max(1.0));
    match fit {
        ImageFit::Stretch => *bounds,
        ImageFit::Cover | ImageFit::Contain => {
            let sx = bounds.w / iw;
            let sy = bounds.h / ih;
            let s = if fit == ImageFit::Cover {
                sx.max(sy)
            } else {
                sx.min(sy)
            };
            let (w, h) = (iw * s, ih * s);
            Rect::new(
                bounds.x + (bounds.w - w) * 0.5,
                bounds.y + (bounds.h - h) * 0.5,
                w,
                h,
            )
        }
        ImageFit::Tile => {
            let s = tile_scale.max(1e-3) * px_per_unit;
            Rect::new(bounds.x, bounds.y, iw * s, ih * s)
        }
    }
}

/// Font size quantization for glyph rasterization: exact below 4px steps of 0.25px, then
/// buckets of 1/24 octave (< 1.5% size error) so continuous zoom reuses atlas glyphs.
pub fn quantize_font_px(s: f32) -> f32 {
    if s <= 12.0 {
        (s * 4.0).round().max(1.0) / 4.0
    } else {
        let b = (s.log2() * 24.0).round();
        (b / 24.0).exp2()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[test]
    fn axis_aligned_detection() {
        assert!(is_axis_aligned(&Affine2::IDENTITY));
        assert!(is_axis_aligned(&Affine2::from_scale(Vec2::new(2.0, -3.0))));
        assert!(is_axis_aligned(&Affine2::from_angle(FRAC_PI_2)));
        assert!(!is_axis_aligned(&Affine2::from_angle(0.3)));
    }

    #[test]
    fn rect_mapping_permutes_radii() {
        let radii = CornerRadii([1.0, 2.0, 3.0, 4.0]);
        let size = Vec2::new(100.0, 50.0);
        let t = Affine2::from_translation(Vec2::new(10.0, 20.0));
        let (r, rr) = axis_aligned_rect(&t, size, &radii).unwrap();
        assert_eq!(r, Rect::new(10.0, 20.0, 100.0, 50.0));
        assert_eq!(rr, [1.0, 2.0, 3.0, 4.0]);
        // 90° clockwise (y down): local TL goes to screen TR.
        let t = Affine2::from_angle(FRAC_PI_2);
        let (r, rr) = axis_aligned_rect(&t, size, &radii).unwrap();
        assert!((r.w - 50.0).abs() < 1e-3 && (r.h - 100.0).abs() < 1e-3);
        for (a, b) in rr.iter().zip([4.0, 1.0, 2.0, 3.0]) {
            assert!((a - b).abs() < 1e-3, "{rr:?}");
        }
        // Horizontal flip swaps left/right corners; scale 2 doubles radii.
        let t = Affine2::from_scale(Vec2::new(-2.0, 2.0));
        let (_, rr) = axis_aligned_rect(&t, size, &radii).unwrap();
        assert_eq!(rr, [4.0, 2.0, 8.0, 6.0]);
        assert!(axis_aligned_rect(&Affine2::from_angle(0.5), size, &radii).is_none());
    }

    #[test]
    fn radii_clamp_like_css() {
        assert_eq!(clamp_radii(10.0, 10.0, [10.0; 4]), [5.0; 4]);
        assert_eq!(
            clamp_radii(100.0, 100.0, [5.0, 0.0, 5.0, 0.0]),
            [5.0, 0.0, 5.0, 0.0]
        );
    }

    fn tri_area(t: &[Vec2]) -> f32 {
        t.chunks(3)
            .map(|c| ((c[1] - c[0]).perp_dot(c[2] - c[0]) * 0.5).abs())
            .sum()
    }

    #[test]
    fn fill_tessellation_area() {
        let mut out = Vec::new();
        let rect = Shape::Rect {
            size: Vec2::new(10.0, 20.0),
            radii: CornerRadii::default(),
        };
        tessellate_fill(&lyon_path(&rect, 0.0), FillRule::NonZero, 0.01, &mut out);
        assert!((tri_area(&out) - 200.0).abs() < 1e-2);
        let el = Shape::Ellipse {
            size: Vec2::new(20.0, 20.0),
        };
        tessellate_fill(&lyon_path(&el, 0.0), FillRule::NonZero, 0.01, &mut out);
        let a = tri_area(&out);
        assert!(
            (a - std::f32::consts::PI * 100.0).abs() < 1.0,
            "ellipse area {a} tris {}",
            out.len() / 3
        );
        // Even-odd: two nested squares leave a hole.
        let mut p = pen_render::Path {
            fill_rule: FillRule::EvenOdd,
            ..Default::default()
        };
        for (o, s) in [(0.0, 10.0), (2.0, 6.0)] {
            p.els.push(PathEl::MoveTo(Vec2::new(o, o)));
            p.els.push(PathEl::LineTo(Vec2::new(o + s, o)));
            p.els.push(PathEl::LineTo(Vec2::new(o + s, o + s)));
            p.els.push(PathEl::LineTo(Vec2::new(o, o + s)));
            p.els.push(PathEl::Close);
        }
        let shape = Shape::Path(std::sync::Arc::new(p));
        tessellate_fill(
            &lyon_path(&shape, 0.0),
            fill_rule_of(&shape),
            0.01,
            &mut out,
        );
        assert!((tri_area(&out) - 64.0).abs() < 1e-2);
        // Implicit begin without MoveTo must not panic.
        let p = pen_render::Path {
            els: vec![
                PathEl::LineTo(Vec2::new(5.0, 0.0)),
                PathEl::LineTo(Vec2::new(5.0, 5.0)),
                PathEl::Close,
            ],
            ..Default::default()
        };
        tessellate_fill(
            &lyon_path(&Shape::Path(std::sync::Arc::new(p)), 0.0),
            FillRule::NonZero,
            0.01,
            &mut out,
        );
        assert!((tri_area(&out) - 12.5).abs() < 1e-2);
    }

    fn stroke(w: f32) -> Stroke {
        Stroke {
            paints: Default::default(),
            widths: [w; 4],
            align: pen_render::StrokeAlign::Center,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 4.0,
            dash: Default::default(),
        }
    }

    #[test]
    fn stroke_alignment_and_dashes() {
        let mut out = Vec::new();
        let rect = Shape::Rect {
            size: Vec2::new(10.0, 10.0),
            radii: CornerRadii::default(),
        };
        let mut s = stroke(2.0);
        tessellate_stroke(
            &lyon_path(&rect, stroke_grow(&rect, &s, 2.0)),
            &s,
            2.0,
            0.01,
            &mut out,
        );
        // Centered: outer 12x12 minus inner 8x8.
        assert!((tri_area(&out) - 80.0).abs() < 0.1, "{}", tri_area(&out));
        s.align = pen_render::StrokeAlign::Inside;
        tessellate_stroke(
            &lyon_path(&rect, stroke_grow(&rect, &s, 2.0)),
            &s,
            2.0,
            0.01,
            &mut out,
        );
        assert!((tri_area(&out) - 64.0).abs() < 0.1); // 100 - 36
        s.align = pen_render::StrokeAlign::Outside;
        tessellate_stroke(
            &lyon_path(&rect, stroke_grow(&rect, &s, 2.0)),
            &s,
            2.0,
            0.01,
            &mut out,
        );
        assert!((tri_area(&out) - 96.0).abs() < 0.1); // 196 - 100
        // A 10-unit line dashed 2 on / 2 off covers ~half the length.
        let line = pen_render::Path {
            els: vec![
                PathEl::MoveTo(Vec2::ZERO),
                PathEl::LineTo(Vec2::new(10.0, 0.0)),
            ],
            ..Default::default()
        };
        let line = Shape::Path(std::sync::Arc::new(line));
        let mut s = stroke(1.0);
        s.dash = smallvec::smallvec![2.0, 2.0];
        tessellate_stroke(&lyon_path(&line, 0.0), &s, 1.0, 0.01, &mut out);
        assert!((tri_area(&out) - 6.0).abs() < 0.1, "{}", tri_area(&out)); // dashes at 0,4,8 (last is 2)
        // Degenerate pattern falls back to a solid stroke.
        s.dash = smallvec::smallvec![0.0, 0.0];
        tessellate_stroke(&lyon_path(&line, 0.0), &s, 1.0, 0.01, &mut out);
        assert!((tri_area(&out) - 10.0).abs() < 0.1);
    }

    #[test]
    fn tolerance_hysteresis() {
        let tol = pick_tolerance(2.0);
        assert!(tess_valid(tol, 2.0));
        assert!(tess_valid(tol, 5.0));
        assert!(!tess_valid(tol, 12.0));
        assert!(!tess_valid(tol, 0.3));
    }

    #[test]
    fn linear_gradient_mapping_matches_shader() {
        let cases = [
            (
                Rect::new(10.0, 20.0, 200.0, 100.0),
                Vec2::new(10.0, 20.0),
                Vec2::new(210.0, 120.0),
                0.0,
                1.0,
            ),
            (
                Rect::new(0.0, 0.0, 50.0, 300.0),
                Vec2::new(25.0, 0.0),
                Vec2::new(25.0, 300.0),
                0.2,
                0.7,
            ),
            (
                Rect::new(0.0, 0.0, 100.0, 100.0),
                Vec2::new(100.0, 30.0),
                Vec2::new(0.0, 60.0),
                0.0,
                1.0,
            ),
            (
                Rect::new(-5.0, 7.0, 80.0, 40.0),
                Vec2::new(70.0, 40.0),
                Vec2::new(-10.0, 0.0),
                0.1,
                0.9,
            ),
        ];
        for (b, p0, p1, o0, o1) in cases {
            let g = linear_to_gpui(&b, p0, p1, o0, o1).unwrap();
            let d = p1 - p0;
            for (u, v) in [(0.1, 0.2), (0.5, 0.5), (0.9, 0.3), (0.3, 0.95), (0.0, 1.0)] {
                let p = Vec2::new(b.x + b.w * u, b.y + b.h * v);
                let want =
                    (((p - p0).dot(d) / d.length_squared() - o0) / (o1 - o0)).clamp(0.0, 1.0);
                let got = ((gpui_linear_t(&b, g.angle_deg, p) - g.pct0) / (g.pct1 - g.pct0))
                    .clamp(0.0, 1.0);
                assert!(
                    (want - got).abs() < 1e-3,
                    "{b:?} {p:?}: want {want} got {got}"
                );
            }
        }
        assert!(
            linear_to_gpui(
                &Rect::new(0.0, 0.0, 10.0, 10.0),
                Vec2::ONE,
                Vec2::ONE,
                0.0,
                1.0
            )
            .is_none()
        );
    }

    #[test]
    fn stops_sampling() {
        let stops = [
            GradientStop {
                offset: 0.0,
                color: Color::BLACK,
            },
            GradientStop {
                offset: 0.5,
                color: Color::WHITE,
            },
            GradientStop {
                offset: 1.0,
                color: Color::BLACK,
            },
        ];
        assert_eq!(sample_stops(&stops, -1.0), Color::BLACK);
        assert_eq!(sample_stops(&stops, 0.5), Color::WHITE);
        assert!((sample_stops(&stops, 0.25).r - 0.5).abs() < 1e-5);
        assert!((average_stop_color(&stops).r - 0.5).abs() < 0.05);
        let radial = Paint::Radial {
            center: Vec2::splat(5.0),
            radius: Vec2::splat(5.0),
            stops: stops.iter().copied().collect(),
        };
        assert!((gradient_t(&radial, Vec2::new(10.0, 5.0)) - 1.0).abs() < 1e-5);
        let tex = gradient_texture(
            &radial,
            &Rect::new(0.0, 0.0, 10.0, 10.0),
            &Affine2::IDENTITY,
            4,
            4,
            1.0,
        );
        assert_eq!(tex.len(), 64);
        let angular = Paint::Angular {
            center: Vec2::ZERO,
            start_angle: 0.0,
            stops: stops.iter().copied().collect(),
        };
        assert!((gradient_t(&angular, Vec2::new(0.0, 1.0)) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn image_fit() {
        let b = Rect::new(0.0, 0.0, 100.0, 50.0);
        let c = image_fit_rect(&b, 200.0, 200.0, ImageFit::Cover, 1.0, 1.0);
        assert_eq!(c, Rect::new(0.0, -25.0, 100.0, 100.0));
        let c = image_fit_rect(&b, 200.0, 200.0, ImageFit::Contain, 1.0, 1.0);
        assert_eq!(c, Rect::new(25.0, 0.0, 50.0, 50.0));
        assert_eq!(image_fit_rect(&b, 3.0, 7.0, ImageFit::Stretch, 1.0, 1.0), b);
        let t = image_fit_rect(&b, 10.0, 20.0, ImageFit::Tile, 0.5, 2.0);
        assert_eq!(t, Rect::new(0.0, 0.0, 10.0, 20.0));
    }

    #[test]
    fn font_quantization() {
        assert_eq!(quantize_font_px(10.1), 10.0);
        for s in [13.0f32, 20.0, 47.3, 300.0] {
            let q = quantize_font_px(s);
            assert!((q / s - 1.0).abs() < 0.015, "{s} -> {q}");
        }
        assert_eq!(quantize_font_px(20.0), quantize_font_px(20.1));
    }

    #[test]
    fn item_bounds_include_shadow_and_stroke() {
        let item = DisplayItem::Shape {
            node: pen_render::NodeKey(1),
            transform: Affine2::from_translation(Vec2::new(100.0, 0.0)),
            shape: Shape::Rect {
                size: Vec2::new(10.0, 10.0),
                radii: CornerRadii::default(),
            },
            fills: Default::default(),
            stroke: Some(stroke(2.0)),
            shadows: smallvec::smallvec![Shadow::Drop {
                offset: Vec2::new(0.0, 10.0),
                blur: 0.0,
                spread: 0.0,
                color: Color::BLACK
            }],
        };
        let b = item_doc_bounds(&item).unwrap();
        assert_eq!(b, Rect::new(99.0, -1.0, 12.0, 22.0));
        assert!(item_doc_bounds(&DisplayItem::PopClip).is_none());
    }
}
