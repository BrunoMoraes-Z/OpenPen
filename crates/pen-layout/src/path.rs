//! Vector geometry: SVG path data parsing, fitting paths into node boxes,
//! ellipse arcs/rings and (rounded) regular polygons.

use pen_render::{FillRule, Path, PathEl, Vec2};
use std::f32::consts::{FRAC_PI_2, TAU};

/// Parsed SVG path (absolute coordinates) with its tight bounds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedPath {
    pub els: Vec<PathEl>,
    /// min x, min y, max x, max y (zero when empty).
    pub bounds: [f32; 4],
}

pub(crate) struct Lexer<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) i: usize,
}

impl Lexer<'_> {
    pub(crate) fn skip_sep(&mut self) {
        while self.i < self.b.len()
            && (self.b[self.i].is_ascii_whitespace() || self.b[self.i] == b',')
        {
            self.i += 1;
        }
    }
    fn peek_cmd(&mut self) -> Option<u8> {
        self.skip_sep();
        let c = *self.b.get(self.i)?;
        c.is_ascii_alphabetic().then_some(c)
    }
    pub(crate) fn number(&mut self) -> Option<f32> {
        self.skip_sep();
        let start = self.i;
        let b = self.b;
        let mut i = self.i;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let mut digits = false;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits = true;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
                digits = true;
            }
        }
        if !digits {
            return None;
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        self.i = i;
        std::str::from_utf8(&b[start..i])
            .ok()?
            .parse()
            .ok()
            .filter(|v: &f32| v.is_finite())
    }
    fn flag(&mut self) -> Option<bool> {
        self.skip_sep();
        let c = *self.b.get(self.i)?;
        match c {
            b'0' | b'1' => {
                self.i += 1;
                Some(c == b'1')
            }
            _ => None,
        }
    }
}

/// Parses SVG path data (`M L H V C S Q T A Z`, absolute and relative). Parsing
/// stops at the first malformed segment, keeping what was parsed.
pub fn parse_svg_path(d: &str) -> ParsedPath {
    let (els, _) = parse_els(d);
    let bounds = path_bounds(&els);
    ParsedPath { els, bounds }
}

/// Absolute path elements of `d`, and whether the whole input was consumed
/// (`false`: parsing stopped at a malformed segment).
pub(crate) fn parse_els(d: &str) -> (Vec<PathEl>, bool) {
    let mut lx = Lexer {
        b: d.as_bytes(),
        i: 0,
    };
    let mut els = Vec::new();
    let mut cur = Vec2::ZERO;
    let mut start = Vec2::ZERO;
    let mut last_ctrl: Option<Vec2> = None;
    let mut last_quad: Option<Vec2> = None;
    let mut cmd = 0u8;
    loop {
        if let Some(c) = lx.peek_cmd() {
            cmd = c;
            lx.i += 1;
        } else if lx.i >= lx.b.len() || cmd == 0 {
            break;
        } else if cmd == b'M' {
            cmd = b'L';
        } else if cmd == b'm' {
            cmd = b'l';
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur } else { Vec2::ZERO };
        macro_rules! pt {
            () => {{
                let x = lx.number();
                let y = lx.number();
                match (x, y) {
                    (Some(x), Some(y)) => Vec2::new(x, y) + base,
                    _ => break,
                }
            }};
        }
        let mut ctrl = None;
        let mut quad = None;
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let p = pt!();
                els.push(PathEl::MoveTo(p));
                cur = p;
                start = p;
            }
            b'L' => {
                let p = pt!();
                els.push(PathEl::LineTo(p));
                cur = p;
            }
            b'H' => {
                let Some(x) = lx.number() else { break };
                cur = Vec2::new(if rel { cur.x + x } else { x }, cur.y);
                els.push(PathEl::LineTo(cur));
            }
            b'V' => {
                let Some(y) = lx.number() else { break };
                cur = Vec2::new(cur.x, if rel { cur.y + y } else { y });
                els.push(PathEl::LineTo(cur));
            }
            b'C' => {
                let c1 = pt!();
                let c2 = pt!();
                let p = pt!();
                els.push(PathEl::CubicTo(c1, c2, p));
                ctrl = Some(c2);
                cur = p;
            }
            b'S' => {
                let c1 = last_ctrl.map_or(cur, |c| cur * 2.0 - c);
                let c2 = pt!();
                let p = pt!();
                els.push(PathEl::CubicTo(c1, c2, p));
                ctrl = Some(c2);
                cur = p;
            }
            b'Q' => {
                let c = pt!();
                let p = pt!();
                els.push(PathEl::QuadTo(c, p));
                quad = Some(c);
                cur = p;
            }
            b'T' => {
                let c = last_quad.map_or(cur, |q| cur * 2.0 - q);
                let p = pt!();
                els.push(PathEl::QuadTo(c, p));
                quad = Some(c);
                cur = p;
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot)) = (lx.number(), lx.number(), lx.number())
                else {
                    break;
                };
                let (Some(large), Some(sweep)) = (lx.flag(), lx.flag()) else {
                    break;
                };
                let p = pt!();
                arc_to(&mut els, cur, p, rx, ry, rot, large, sweep);
                cur = p;
            }
            b'Z' => {
                els.push(PathEl::Close);
                cur = start;
                // Z takes no arguments: a following number is malformed.
                if lx.peek_cmd().is_none() {
                    lx.skip_sep();
                    if lx.i < lx.b.len() {
                        break;
                    }
                }
            }
            _ => break,
        }
        last_ctrl = ctrl;
        last_quad = quad;
    }
    lx.skip_sep();
    let complete = lx.i >= lx.b.len();
    (els, complete)
}

/// SVG endpoint arc -> cubic Béziers (F.6.5 of the SVG spec).
#[allow(clippy::too_many_arguments)]
fn arc_to(
    els: &mut Vec<PathEl>,
    p0: Vec2,
    p1: Vec2,
    rx: f32,
    ry: f32,
    rot_deg: f32,
    large: bool,
    sweep: bool,
) {
    if p0 == p1 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < 1e-6 || ry < 1e-6 {
        els.push(PathEl::LineTo(p1));
        return;
    }
    let phi = rot_deg.to_radians();
    let (sp, cp) = phi.sin_cos();
    let d = (p0 - p1) * 0.5;
    let x1 = cp * d.x + sp * d.y;
    let y1 = -sp * d.x + cp * d.y;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = if den > 0.0 {
        (num / den).max(0.0).sqrt()
    } else {
        0.0
    };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1 / ry;
    let cyp = -coef * ry * x1 / rx;
    let mid = (p0 + p1) * 0.5;
    let c = Vec2::new(cp * cxp - sp * cyp + mid.x, sp * cxp + cp * cyp + mid.y);
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let theta1 = angle(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
    let mut dtheta = angle(
        (x1 - cxp) / rx,
        (y1 - cyp) / ry,
        (-x1 - cxp) / rx,
        (-y1 - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += TAU;
    }
    let segs = (dtheta.abs() / FRAC_PI_2).ceil().max(1.0) as usize;
    let step = dtheta / segs as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let map = |x: f32, y: f32| {
        Vec2::new(
            c.x + cp * rx * x - sp * ry * y,
            c.y + sp * rx * x + cp * ry * y,
        )
    };
    let mut t = theta1;
    for i in 0..segs {
        let t2 = t + step;
        let (s1, c1) = t.sin_cos();
        let (s2, c2) = t2.sin_cos();
        let a = map(c1 - k * s1, s1 + k * c1);
        let b = map(c2 + k * s2, s2 - k * c2);
        let end = if i + 1 == segs { p1 } else { map(c2, s2) };
        els.push(PathEl::CubicTo(a, b, end));
        t = t2;
    }
}

fn cubic_extrema(p0: f32, p1: f32, p2: f32, p3: f32, out: &mut [f32; 2]) -> usize {
    // Derivative: a t^2 + b t + c
    let a = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
    let b = 2.0 * (p0 - 2.0 * p1 + p2);
    let c = p1 - p0;
    let mut n = 0;
    if a.abs() < 1e-9 {
        if b.abs() > 1e-9 {
            out[0] = -c / b;
            n = 1;
        }
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let s = disc.sqrt();
            out[0] = (-b + s) / (2.0 * a);
            out[1] = (-b - s) / (2.0 * a);
            n = 2;
        }
    }
    n
}

fn cubic_at(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, t: f32) -> Vec2 {
    let mt = 1.0 - t;
    p0 * (mt * mt * mt) + p1 * (3.0 * mt * mt * t) + p2 * (3.0 * mt * t * t) + p3 * (t * t * t)
}

/// Tight bounds of a path (curve extrema, not control points).
pub fn path_bounds(els: &[PathEl]) -> [f32; 4] {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut add = |p: Vec2| {
        min = min.min(p);
        max = max.max(p);
    };
    let mut cur = Vec2::ZERO;
    let mut start = Vec2::ZERO;
    for el in els {
        match *el {
            PathEl::MoveTo(p) => {
                add(p);
                cur = p;
                start = p;
            }
            PathEl::LineTo(p) => {
                add(p);
                cur = p;
            }
            PathEl::QuadTo(c, p) => {
                let c1 = cur + (c - cur) * (2.0 / 3.0);
                let c2 = p + (c - p) * (2.0 / 3.0);
                cubic_bounds(cur, c1, c2, p, &mut add);
                cur = p;
            }
            PathEl::CubicTo(c1, c2, p) => {
                cubic_bounds(cur, c1, c2, p, &mut add);
                cur = p;
            }
            PathEl::Close => cur = start,
        }
    }
    if min.x > max.x {
        return [0.0; 4];
    }
    [min.x, min.y, max.x, max.y]
}

fn cubic_bounds(p0: Vec2, p1: Vec2, p2: Vec2, p3: Vec2, add: &mut impl FnMut(Vec2)) {
    add(p3);
    let mut ts = [0.0f32; 2];
    for axis in 0..2 {
        let g = |v: Vec2| if axis == 0 { v.x } else { v.y };
        let n = cubic_extrema(g(p0), g(p1), g(p2), g(p3), &mut ts);
        for &t in &ts[..n] {
            if t > 0.0 && t < 1.0 {
                add(cubic_at(p0, p1, p2, p3, t));
            }
        }
    }
}

/// Maps a parsed path into a `w x h` box: `view_box` (or the tight bounds)
/// is scaled to the box.
pub fn fit_path(
    p: &ParsedPath,
    view_box: Option<[f32; 4]>,
    w: f32,
    h: f32,
    even_odd: bool,
) -> Path {
    let (ox, oy, vw, vh) = match view_box {
        Some([x, y, vw, vh]) if vw > 0.0 && vh > 0.0 => (x, y, vw, vh),
        _ => (
            p.bounds[0],
            p.bounds[1],
            p.bounds[2] - p.bounds[0],
            p.bounds[3] - p.bounds[1],
        ),
    };
    let sx = if vw > 1e-6 { w / vw } else { 1.0 };
    let sy = if vh > 1e-6 { h / vh } else { 1.0 };
    let o = Vec2::new(ox, oy);
    let s = Vec2::new(sx, sy);
    let m = |v: Vec2| (v - o) * s;
    let els = p
        .els
        .iter()
        .map(|e| match *e {
            PathEl::MoveTo(a) => PathEl::MoveTo(m(a)),
            PathEl::LineTo(a) => PathEl::LineTo(m(a)),
            PathEl::QuadTo(a, b) => PathEl::QuadTo(m(a), m(b)),
            PathEl::CubicTo(a, b, c) => PathEl::CubicTo(m(a), m(b), m(c)),
            PathEl::Close => PathEl::Close,
        })
        .collect();
    Path {
        els,
        fill_rule: if even_odd {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        },
    }
}

/// Appends an elliptic arc (visual angles in radians, counter-clockwise
/// positive, y down) as cubics. Assumes the pen is at the arc start.
fn ellipse_arc(els: &mut Vec<PathEl>, c: Vec2, r: Vec2, a0: f32, sweep: f32) {
    let segs = (sweep.abs() / FRAC_PI_2).ceil().max(1.0) as usize;
    let step = sweep / segs as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let pt = |t: f32| Vec2::new(c.x + r.x * t.cos(), c.y - r.y * t.sin());
    let tan = |t: f32| Vec2::new(-r.x * t.sin(), -r.y * t.cos());
    let mut t = a0;
    for _ in 0..segs {
        let t2 = t + step;
        let p0 = pt(t);
        let p3 = pt(t2);
        els.push(PathEl::CubicTo(p0 + tan(t) * k, p3 - tan(t2) * k, p3));
        t = t2;
    }
}

/// Ellipse with optional inner radius (ring) and partial sweep (pie / arc band).
/// Angles in degrees, counter-clockwise from +x.
pub fn ellipse_path(w: f32, h: f32, inner: f32, start_deg: f32, sweep_deg: f32) -> Path {
    let c = Vec2::new(w * 0.5, h * 0.5);
    let r = c;
    let ri = r * inner.clamp(0.0, 1.0);
    let a0 = start_deg.to_radians();
    let sweep = sweep_deg.clamp(-360.0, 360.0).to_radians();
    let pt = |r: Vec2, t: f32| Vec2::new(c.x + r.x * t.cos(), c.y - r.y * t.sin());
    let mut els = Vec::new();
    let full = sweep.abs() >= TAU - 1e-4;
    if full {
        els.push(PathEl::MoveTo(pt(r, a0)));
        ellipse_arc(&mut els, c, r, a0, TAU);
        els.push(PathEl::Close);
        if inner > 0.0 {
            els.push(PathEl::MoveTo(pt(ri, a0)));
            ellipse_arc(&mut els, c, ri, a0, -TAU);
            els.push(PathEl::Close);
        }
        return Path {
            els,
            fill_rule: FillRule::EvenOdd,
        };
    }
    if inner > 0.0 {
        els.push(PathEl::MoveTo(pt(r, a0)));
        ellipse_arc(&mut els, c, r, a0, sweep);
        els.push(PathEl::LineTo(pt(ri, a0 + sweep)));
        ellipse_arc(&mut els, c, ri, a0 + sweep, -sweep);
        els.push(PathEl::Close);
    } else {
        els.push(PathEl::MoveTo(c));
        els.push(PathEl::LineTo(pt(r, a0)));
        ellipse_arc(&mut els, c, r, a0, sweep);
        els.push(PathEl::Close);
    }
    Path {
        els,
        fill_rule: FillRule::NonZero,
    }
}

/// Regular polygon with `n` sides inscribed in the ellipse of the `w x h`
/// box, first vertex at the top, corners rounded with circular arcs of
/// `radius` (Pen's `getEditableContours`: a triangle's base sits at 3/4 of
/// the height, it is not stretched to the box). Fewer than 3 sides draw
/// nothing, as in Pen.
pub fn polygon_path(w: f32, h: f32, n: u32, radius: f32) -> Path {
    crate::vector_shapes::polygon(w, h, n, radius).to_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commands() {
        let p = parse_svg_path("M7 0l0 14m7-7l-7 7-7-7");
        assert_eq!(p.els.len(), 5);
        assert_eq!(p.bounds, [0.0, 0.0, 14.0, 14.0]);
        let p = parse_svg_path("M10,10 h10 v10 H10 z");
        assert_eq!(p.bounds, [10.0, 10.0, 20.0, 20.0]);
        assert!(matches!(p.els.last(), Some(PathEl::Close)));
        let p = parse_svg_path("M0 0C0 10 10 10 10 0S20-10 20 0");
        assert_eq!(p.els.len(), 3);
        assert!((p.bounds[3] - 7.5).abs() < 1e-3, "{:?}", p.bounds);
        assert!((p.bounds[1] + 7.5).abs() < 1e-3, "{:?}", p.bounds);
        // Compact numbers and arcs with packed flags.
        let p = parse_svg_path("M.5.5a5 5 0 1010 0");
        assert!(p.els.len() >= 3);
        assert!((p.bounds[3] - 5.5).abs() < 0.05, "{:?}", p.bounds);
        let p = parse_svg_path("M0 0 L 1e1 0 Q 5 5 0 0 T 5 5");
        assert_eq!(p.els.len(), 4);
        // Garbage keeps the valid prefix.
        let p = parse_svg_path("M0 0 L 5 5 X 1 2");
        assert_eq!(p.els.len(), 2);
    }

    #[test]
    fn fit_and_shapes() {
        let p = parse_svg_path("M7 0l0 14");
        let f = fit_path(&p, None, 0.0, 100.0, false);
        assert_eq!(f.els[1], PathEl::LineTo(Vec2::new(0.0, 100.0)));
        let e = ellipse_path(10.0, 10.0, 0.5, 0.0, 360.0);
        assert_eq!(e.fill_rule, FillRule::EvenOdd);
        let b = path_bounds(&e.els);
        assert!((b[2] - 10.0).abs() < 1e-3 && b[0].abs() < 1e-3);
        let pie = ellipse_path(10.0, 10.0, 0.0, 0.0, 90.0);
        let b = path_bounds(&pie.els);
        // Quarter from +x going up (counter-clockwise).
        assert!((b[1]).abs() < 1e-3 && (b[3] - 5.0).abs() < 1e-3, "{b:?}");
        // Pen: vertices on the box's inscribed ellipse (base at 3/4 height).
        let tri = polygon_path(10.0, 10.0, 3, 0.0);
        let b = path_bounds(&tri.els);
        assert!(
            (b[1] - 0.0).abs() < 1e-4 && (b[3] - 7.5).abs() < 1e-4,
            "{b:?}"
        );
        let r = polygon_path(10.0, 10.0, 6, 2.0);
        assert!(r.els.iter().any(|e| matches!(e, PathEl::CubicTo(..))));
        assert!(polygon_path(10.0, 10.0, 2, 0.0).els.is_empty());
    }
}
