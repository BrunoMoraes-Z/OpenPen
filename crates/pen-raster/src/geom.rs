//! Geometry helpers: transform conversion, device rects, shape -> path building.

use pen_render::{Affine2, CornerRadii, Path as PenPath, PathEl, Rect, Shape, Vec2};
use tiny_skia::{PathBuilder, Transform};

/// Cubic Bezier circle constant (quarter arc).
const KAPPA: f32 = 0.552_284_8;

/// Converts a glam affine into a tiny-skia transform.
pub fn to_ts(a: &Affine2) -> Transform {
    let m = a.matrix2;
    Transform::from_row(
        m.x_axis.x,
        m.x_axis.y,
        m.y_axis.x,
        m.y_axis.y,
        a.translation.x,
        a.translation.y,
    )
}

/// Document space -> device space for a viewport at `scale`.
pub fn view_transform(viewport: &Rect, scale: f32) -> Transform {
    Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        -viewport.x * scale,
        -viewport.y * scale,
    )
}

/// Geometric mean scale factor of a transform (sqrt of |det|).
pub fn ts_scale(ts: &Transform) -> f32 {
    (ts.sx * ts.sy - ts.kx * ts.ky).abs().sqrt()
}

/// True when the transform has no rotation/skew.
pub fn is_axis_aligned(ts: &Transform) -> bool {
    ts.kx.abs() < 1e-6 && ts.ky.abs() < 1e-6
}

/// Axis-aligned bounding box of `r` after applying `ts`.
pub fn map_rect(ts: &Transform, r: &Rect) -> Rect {
    let pts = [
        (r.x, r.y),
        (r.right(), r.y),
        (r.x, r.bottom()),
        (r.right(), r.bottom()),
    ];
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (x, y) in pts {
        let px = ts.sx * x + ts.kx * y + ts.tx;
        let py = ts.ky * x + ts.sy * y + ts.ty;
        min = (min.0.min(px), min.1.min(py));
        max = (max.0.max(px), max.1.max(py));
    }
    Rect::new(min.0, min.1, max.0 - min.0, max.1 - min.1)
}

pub fn outset(r: &Rect, d: f32) -> Rect {
    Rect::new(r.x - d, r.y - d, r.w + 2.0 * d, r.h + 2.0 * d)
}

/// Integer device rectangle, half-open `[x0, x1) x [y0, y1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub const EMPTY: IRect = IRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }
    pub const fn from_size(w: u32, h: u32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: w as i32,
            y1: h as i32,
        }
    }
    /// Smallest integer rect covering `r` (rounded out), saturating on huge values.
    pub fn round_out(r: &Rect) -> Self {
        if !(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite())
            || r.w < 0.0
            || r.h < 0.0
        {
            return Self::EMPTY;
        }
        let c = |v: f32| v.clamp(-1.0e8, 1.0e8);
        Self {
            x0: c(r.x.floor()) as i32,
            y0: c(r.y.floor()) as i32,
            x1: c(r.right().ceil()) as i32,
            y1: c(r.bottom().ceil()) as i32,
        }
    }
    pub fn width(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }
    pub fn height(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
    pub fn intersect(&self, o: &IRect) -> IRect {
        let r = IRect::new(
            self.x0.max(o.x0),
            self.y0.max(o.y0),
            self.x1.min(o.x1),
            self.y1.min(o.y1),
        );
        if r.is_empty() { IRect::EMPTY } else { r }
    }
    pub fn union(&self, o: &IRect) -> IRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        IRect::new(
            self.x0.min(o.x0),
            self.y0.min(o.y0),
            self.x1.max(o.x1),
            self.y1.max(o.y1),
        )
    }
    pub fn outset(&self, d: i32) -> IRect {
        if self.is_empty() {
            return *self;
        }
        IRect::new(self.x0 - d, self.y0 - d, self.x1 + d, self.y1 + d)
    }
    pub fn translate(&self, dx: i32, dy: i32) -> IRect {
        IRect::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }
}

/// Clamps per-corner radii so adjacent radii never exceed the side they share (CSS rule).
pub fn clamp_radii(w: f32, h: f32, radii: [f32; 4]) -> [f32; 4] {
    let r = radii.map(|r| if r.is_finite() { r.max(0.0) } else { 0.0 });
    let [tl, tr, br, bl] = r;
    let mut f: f32 = 1.0;
    for (side, sum) in [(w, tl + tr), (h, tr + br), (w, br + bl), (h, bl + tl)] {
        if sum > 0.0 && sum > side {
            f = f.min(side.max(0.0) / sum);
        }
    }
    r.map(|v| v * f)
}

/// Appends a rounded rect (clockwise) with per-corner radii `[tl, tr, br, bl]`.
pub fn push_rrect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, radii: [f32; 4]) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let [tl, tr, br, bl] = clamp_radii(w, h, radii);
    if tl + tr + br + bl <= 0.0 {
        if let Some(r) = tiny_skia::Rect::from_xywh(x, y, w, h) {
            pb.push_rect(r);
        }
        return;
    }
    let (r, b) = (x + w, y + h);
    let k = 1.0 - KAPPA;
    pb.move_to(x + tl, y);
    pb.line_to(r - tr, y);
    if tr > 0.0 {
        pb.cubic_to(r - tr * k, y, r, y + tr * k, r, y + tr);
    }
    pb.line_to(r, b - br);
    if br > 0.0 {
        pb.cubic_to(r, b - br * k, r - br * k, b, r - br, b);
    }
    pb.line_to(x + bl, b);
    if bl > 0.0 {
        pb.cubic_to(x + bl * k, b, x, b - bl * k, x, b - bl);
    }
    pb.line_to(x, y + tl);
    if tl > 0.0 {
        pb.cubic_to(x, y + tl * k, x + tl * k, y, x + tl, y);
    }
    pb.close();
}

pub fn push_ellipse(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    if let Some(r) = tiny_skia::Rect::from_xywh(x, y, w, h) {
        pb.push_oval(r);
    }
}

pub fn push_pen_path(pb: &mut PathBuilder, path: &PenPath) {
    for el in &path.els {
        match *el {
            PathEl::MoveTo(p) => pb.move_to(p.x, p.y),
            PathEl::LineTo(p) => pb.line_to(p.x, p.y),
            PathEl::QuadTo(c, p) => pb.quad_to(c.x, c.y, p.x, p.y),
            PathEl::CubicTo(c1, c2, p) => pb.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y),
            PathEl::Close => pb.close(),
        }
    }
}

/// True when every subpath of `path` is closed (inside/outside strokes make sense).
pub fn path_is_closed(path: &PenPath) -> bool {
    let mut open = false;
    let mut any = false;
    for el in &path.els {
        match el {
            PathEl::MoveTo(_) => {
                if open {
                    return false;
                }
                open = true;
            }
            PathEl::Close => {
                open = false;
                any = true;
            }
            _ => {}
        }
    }
    any && !open
}

/// Appends `shape` (local space) to `pb`, grown by `grow` on every side (negative shrinks).
pub fn push_shape(pb: &mut PathBuilder, shape: &Shape, grow: f32) {
    match shape {
        Shape::Rect { size, radii } => {
            let r = grow_radii(radii, grow);
            push_rrect(
                pb,
                -grow,
                -grow,
                size.x + 2.0 * grow,
                size.y + 2.0 * grow,
                r,
            );
        }
        Shape::Ellipse { size } => {
            push_ellipse(pb, -grow, -grow, size.x + 2.0 * grow, size.y + 2.0 * grow)
        }
        Shape::Path(p) => push_pen_path(pb, p),
    }
}

/// Radii of a rect grown by `grow`: rounded corners grow/shrink with the offset, sharp corners stay sharp.
pub fn grow_radii(radii: &CornerRadii, grow: f32) -> [f32; 4] {
    radii
        .0
        .map(|r| if r > 0.0 { (r + grow).max(0.0) } else { 0.0 })
}

/// Conservative local bounds of a shape.
pub fn shape_bounds(shape: &Shape) -> Rect {
    match shape {
        Shape::Rect { size, .. } | Shape::Ellipse { size } => Rect::new(0.0, 0.0, size.x, size.y),
        Shape::Path(p) => path_bounds(p),
    }
}

/// Control-point bounds of a path (conservative).
pub fn path_bounds(p: &PenPath) -> Rect {
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
                add(b);
            }
            PathEl::CubicTo(a, b, c) => {
                add(a);
                add(b);
                add(c);
            }
            PathEl::Close => {}
        }
    }
    if min.x > max.x {
        return Rect::default();
    }
    Rect::new(min.x, min.y, max.x - min.x, max.y - min.y)
}

/// Box sizes for approximating a gaussian of `sigma` with `n` successive box blurs
/// (Kovesi, "Fast almost-Gaussian filtering"). Returns odd box widths.
pub fn box_sizes(sigma: f32, out: &mut [usize]) {
    let n = out.len() as f32;
    if sigma <= 0.0 {
        out.fill(1);
        return;
    }
    let w_ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let wlf = wl as f32;
    let m_ideal =
        (12.0 * sigma * sigma - n * wlf * wlf - 4.0 * n * wlf - 3.0 * n) / (-4.0 * wlf - 4.0);
    let m = m_ideal.round().max(0.0) as usize;
    for (i, o) in out.iter_mut().enumerate() {
        *o = if i < m { wl as usize } else { wu as usize };
    }
}

/// Pixel extent of a blur with `sigma` (how far it spreads).
pub fn blur_extent(sigma: f32) -> i32 {
    if sigma <= 0.0 {
        0
    } else {
        (sigma * 3.0).ceil() as i32 + 1
    }
}

/// Converts a design "blur radius" into a gaussian sigma (CSS / Figma convention: radius = 2 sigma).
pub fn blur_sigma(radius: f32) -> f32 {
    (radius * 0.5).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_conversion_maps_points_identically() {
        let a =
            Affine2::from_scale_angle_translation(Vec2::new(2.0, 3.0), 0.3, Vec2::new(5.0, -7.0));
        let ts = to_ts(&a);
        let p = Vec2::new(1.5, -2.0);
        let q = a.transform_point2(p);
        let mut pt = [tiny_skia::Point::from_xy(p.x, p.y)];
        ts.map_points(&mut pt);
        assert!((pt[0].x - q.x).abs() < 1e-4 && (pt[0].y - q.y).abs() < 1e-4);
    }

    #[test]
    fn view_transform_maps_viewport_origin_to_zero() {
        let ts = view_transform(&Rect::new(10.0, 20.0, 100.0, 100.0), 2.0);
        let r = map_rect(&ts, &Rect::new(10.0, 20.0, 5.0, 5.0));
        assert_eq!(r, Rect::new(0.0, 0.0, 10.0, 10.0));
        assert!((ts_scale(&ts) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn radii_clamped_like_css() {
        assert_eq!(clamp_radii(100.0, 100.0, [10.0; 4]), [10.0; 4]);
        let r = clamp_radii(100.0, 40.0, [40.0; 4]);
        assert!(r.iter().all(|v| (*v - 20.0).abs() < 1e-4));
        assert_eq!(
            clamp_radii(10.0, 10.0, [-1.0, f32::NAN, 0.0, 0.0]),
            [0.0; 4]
        );
    }

    #[test]
    fn irect_ops() {
        let a = IRect::round_out(&Rect::new(0.5, 0.5, 2.0, 2.0));
        assert_eq!(a, IRect::new(0, 0, 3, 3));
        let b = IRect::new(2, 2, 10, 10);
        assert_eq!(a.intersect(&b), IRect::new(2, 2, 3, 3));
        assert!(a.intersect(&IRect::new(5, 5, 6, 6)).is_empty());
        assert_eq!(a.union(&b), IRect::new(0, 0, 10, 10));
        assert_eq!(
            IRect::round_out(&Rect::new(f32::NAN, 0.0, 1.0, 1.0)),
            IRect::EMPTY
        );
    }

    #[test]
    fn box_sizes_approximate_sigma() {
        for sigma in [0.5f32, 1.0, 2.0, 5.0, 12.5, 40.0] {
            let mut s = [0usize; 3];
            box_sizes(sigma, &mut s);
            assert!(s.iter().all(|w| w % 2 == 1));
            // Variance of a box of width w is (w^2 - 1) / 12; variances add.
            let var: f32 = s.iter().map(|w| ((*w * *w) as f32 - 1.0) / 12.0).sum();
            let err = (var.sqrt() - sigma).abs() / sigma;
            assert!(
                err < 0.25 || sigma < 1.0,
                "sigma {sigma}: got {}",
                var.sqrt()
            );
        }
    }

    #[test]
    fn path_closedness() {
        let mut p = PenPath::default();
        p.els.push(PathEl::MoveTo(Vec2::ZERO));
        p.els.push(PathEl::LineTo(Vec2::new(1.0, 0.0)));
        assert!(!path_is_closed(&p));
        p.els.push(PathEl::LineTo(Vec2::new(1.0, 1.0)));
        p.els.push(PathEl::Close);
        assert!(path_is_closed(&p));
        assert_eq!(path_bounds(&p), Rect::new(0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn rrect_path_bounds() {
        let mut pb = PathBuilder::new();
        push_rrect(&mut pb, 1.0, 2.0, 10.0, 20.0, [3.0, 0.0, 5.0, 2.0]);
        let p = pb.finish().unwrap();
        let b = p.bounds();
        assert!((b.left() - 1.0).abs() < 1e-4 && (b.bottom() - 22.0).abs() < 1e-4);
    }
}
