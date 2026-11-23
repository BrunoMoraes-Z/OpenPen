//! Editable contours of the primitive shapes, built the way Pen builds them
//! when a rectangle, ellipse, polygon or line enters vector edit mode
//! (`getEditableContours` in the editor bundle). Coordinates are node-local
//! (`0..w`, `0..h`); the node becomes a `path` with this geometry on the
//! first edit.

use crate::vector::{Anchor, Contour, VectorPath};
use pen_render::Vec2;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

/// Cubic handle length of a quarter circle (Pen's `AS`).
const KAPPA: f32 = 0.552_284_8;
/// Anchors closer than this are merged (Pen's `Vb`).
const MERGE_EPS: f32 = 1e-6;
/// Pen's arc epsilon (`Fse`).
const ARC_EPS: f32 = 1e-9;

/// Rectangle with per-corner radii (top-left, top-right, bottom-right,
/// bottom-left), scaled down like CSS when they do not fit.
pub fn rect(w: f32, h: f32, radii: [f32; 4]) -> VectorPath {
    let [a, r, s, i] = fit_radii(w, h, radii.map(|v| v.max(0.0)));
    let mut o = Vec::new();
    let corner = |o: &mut Vec<Anchor>, p: Vec2, c: Vec2, q: Vec2| {
        o.push(Anchor {
            p,
            handle_in: None,
            handle_out: Some(p + (c - p) * KAPPA),
        });
        o.push(Anchor {
            p: q,
            handle_in: Some(q + (c - q) * KAPPA),
            handle_out: None,
        });
    };
    let v = Vec2::new;
    if a > 0.0 {
        corner(&mut o, v(0.0, a), v(0.0, 0.0), v(a, 0.0));
    } else {
        o.push(Anchor::corner(v(0.0, 0.0)));
    }
    if r > 0.0 {
        corner(&mut o, v(w - r, 0.0), v(w, 0.0), v(w, r));
    } else {
        o.push(Anchor::corner(v(w, 0.0)));
    }
    if s > 0.0 {
        corner(&mut o, v(w, h - s), v(w, h), v(w - s, h));
    } else {
        o.push(Anchor::corner(v(w, h)));
    }
    if i > 0.0 {
        corner(&mut o, v(i, h), v(0.0, h), v(0.0, h - i));
    } else {
        o.push(Anchor::corner(v(0.0, h)));
    }
    merge_coincident(&mut o);
    path(vec![Contour::new(o, true)])
}

/// Pen's radius fitting (`SQ`): all radii scaled by the same factor so no two
/// neighbours exceed their side.
fn fit_radii(w: f32, h: f32, n: [f32; 4]) -> [f32; 4] {
    let [a, r, s, i] = n;
    let f = |sum: f32, side: f32| if sum > 0.0 { side / sum } else { f32::INFINITY };
    let g = f(a + r, w)
        .min(f(r + s, h))
        .min(f(s + i, w))
        .min(f(i + a, h));
    if g < 1.0 { n.map(|v| v * g) } else { n }
}

/// Straight line from the top-left to the bottom-right corner of the box.
pub fn line(w: f32, h: f32) -> VectorPath {
    path(vec![Contour::new(
        vec![Anchor::corner(Vec2::ZERO), Anchor::corner(Vec2::new(w, h))],
        false,
    )])
}

/// Ellipse, ring, arc or pie. `inner` is the inner radius ratio (0..1),
/// angles in degrees counter-clockwise from +x (the `.pen` semantics).
pub fn ellipse(w: f32, h: f32, inner: f32, start_deg: f32, sweep_deg: f32) -> VectorPath {
    let inner = inner.clamp(0.0, 1.0);
    let full = sweep_deg.abs() >= 360.0 - 1e-4;
    let ring = inner > 1e-4;
    let c = Vec2::new(w / 2.0, h / 2.0);
    let r = c;
    let ri = r * inner;
    if full {
        let mut v = vec![full_ellipse(c, r, TAU)];
        if ring {
            v.push(full_ellipse(c, ri, -TAU));
        }
        return path(v);
    }
    let f = (-start_deg).to_radians();
    let m = (-sweep_deg).to_radians();
    let mut g = Vec::new();
    arc(&mut g, c, r, f, m);
    if ring {
        arc(&mut g, c, ri, f + m, -m);
    } else {
        g.push(Anchor::corner(c));
    }
    path(vec![Contour::new(g, true)])
}

/// Regular polygon inscribed in the box, first vertex at the top, with
/// rounded corners of radius `corner` (clamped to half the shortest side).
pub fn polygon(w: f32, h: f32, count: u32, corner: f32) -> VectorPath {
    if count < 3 {
        return VectorPath::default();
    }
    let n = count as usize;
    let c = Vec2::new(w / 2.0, h / 2.0);
    let step = TAU / n as f32;
    let u: Vec<Vec2> = (0..n)
        .map(|m| {
            let g = -FRAC_PI_2 + m as f32 * step;
            Vec2::new(c.x + c.x * g.cos(), c.y + c.y * g.sin())
        })
        .collect();
    let d = (0..n)
        .map(|m| u[m].distance(u[(m + 1) % n]))
        .fold(f32::INFINITY, f32::min);
    if corner <= 0.0 || d < 1e-9 {
        return path(vec![Contour::new(
            u.into_iter().map(Anchor::corner).collect(),
            true,
        )]);
    }
    let hr = corner.min(d / 2.0);
    let mut f = Vec::new();
    for m in 0..n {
        let g = u[(m + n - 1) % n];
        let v = u[m];
        let wv = u[(m + 1) % n];
        let a = (g - v).normalize_or_zero();
        let s = (wv - v).normalize_or_zero();
        let t = a.dot(s).clamp(-1.0, 1.0).acos() / 2.0;
        if !(1e-6..=FRAC_PI_2 - 1e-6).contains(&t) {
            f.push(Anchor::corner(v));
            continue;
        }
        let along = hr / t.tan();
        let p = v + a * along;
        let q = v + s * along;
        let bis = (a + s).normalize_or_zero();
        let l = v + bis * (hr / t.sin());
        let b = (p.y - l.y).atan2(p.x - l.x);
        let mut o = (q.y - l.y).atan2(q.x - l.x) - b;
        if o > PI {
            o -= TAU;
        }
        if o < -PI {
            o += TAU;
        }
        rounded_arc(&mut f, l, Vec2::splat(hr), b, o);
    }
    merge_coincident(&mut f);
    path(vec![Contour::new(f, true)])
}

fn path(contours: Vec<Contour>) -> VectorPath {
    VectorPath { contours }
}

fn at(c: Vec2, r: Vec2, a: f32) -> Vec2 {
    Vec2::new(c.x + r.x * a.cos(), c.y + r.y * a.sin())
}

/// Pen's `_pe`: an elliptical arc from angle `a` sweeping `r` (radians,
/// y down), split at the quadrant boundaries, appended to `t`.
fn arc(t: &mut Vec<Anchor>, e: Vec2, n: Vec2, a: f32, r: f32) {
    if r.abs() < ARC_EPS {
        t.push(Anchor::corner(at(e, n, a)));
        return;
    }
    let s = a + r;
    let i = if r > 0.0 { FRAC_PI_2 } else { -FRAC_PI_2 };
    let o = (a / i + ARC_EPS).floor() + 1.0;
    let l = (s / i - ARC_EPS).ceil() - 1.0;
    let count = (l - o + 1.0).max(0.0) as usize + 1;
    let mut u = a;
    let mut d = Anchor::corner(at(e, n, u));
    for h in 1..=count {
        let f = if h == count {
            s
        } else {
            (o + h as f32 - 1.0) * i
        };
        let m = 4.0 / 3.0 * ((f - u) / 4.0).tan();
        let (sv, cv) = u.sin_cos();
        d.handle_out = Some(Vec2::new(d.p.x - m * n.x * sv, d.p.y + m * n.y * cv));
        t.push(d);
        let (sf, cf) = f.sin_cos();
        let mut next = Anchor::corner(at(e, n, f));
        next.handle_in = Some(Vec2::new(next.p.x + m * n.x * sf, next.p.y - m * n.y * cf));
        d = next;
        u = f;
    }
    t.push(d);
}

/// Pen's `WAe`: a closed ellipse starting at the top.
fn full_ellipse(c: Vec2, r: Vec2, sweep: f32) -> Contour {
    let mut a = Vec::new();
    arc(&mut a, c, r, -FRAC_PI_2, sweep);
    let last = a.pop().expect("an arc has anchors");
    a[0].handle_in = last.handle_in;
    Contour::new(a, true)
}

/// Pen's `gmn`: the corner arc of a rounded polygon, in equal parts of at
/// most a quarter turn.
fn rounded_arc(t: &mut Vec<Anchor>, e: Vec2, n: Vec2, a: f32, r: f32) {
    if r.abs() < 1e-9 {
        t.push(Anchor::corner(at(e, n, a)));
        return;
    }
    // A hair of tolerance: a right angle computed in f32 must stay one part.
    let s = (r.abs() / FRAC_PI_2 - 1e-4).ceil().max(1.0) as usize;
    let i = r / s as f32;
    let o = 4.0 / 3.0 * (i / 4.0).tan();
    for l in 0..=s {
        let c = a + l as f32 * i;
        let (sc, cc) = c.sin_cos();
        let p = at(e, n, c);
        let d = Vec2::new(-o * n.x * sc, o * n.y * cc);
        t.push(Anchor {
            p,
            handle_in: (l > 0).then_some(p - d),
            handle_out: (l < s).then_some(p + d),
        });
    }
}

/// Pen's `iQe`: merges consecutive anchors at the same spot (and the last
/// with the first), keeping the outer handles.
fn merge_coincident(t: &mut Vec<Anchor>) {
    let same = |a: &Anchor, b: &Anchor| {
        (a.p.x - b.p.x).abs() < MERGE_EPS && (a.p.y - b.p.y).abs() < MERGE_EPS
    };
    let mut e = t.len();
    while e > 1 {
        e -= 1;
        if same(&t[e - 1], &t[e]) {
            let a = t.remove(e);
            let n = &mut t[e - 1];
            n.handle_in = n.handle_in.or(a.handle_in);
            n.handle_out = a.handle_out.or(n.handle_out);
        }
    }
    if t.len() > 1 && same(&t[0], &t[t.len() - 1]) {
        let last = t.pop().unwrap();
        t[0].handle_in = t[0].handle_in.or(last.handle_in);
        t[0].handle_out = t[0].handle_out.or(last.handle_out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::parse_svg_path;

    fn close(a: Vec2, b: Vec2) -> bool {
        a.distance(b) < 1e-3
    }

    #[test]
    fn plain_rect_and_line() {
        let p = rect(100.0, 50.0, [0.0; 4]);
        assert_eq!(p.to_svg(), "M0 0 L100 0 L100 50 L0 50 L0 0 Z");
        assert_eq!(line(30.0, 40.0).to_svg(), "M0 0 L30 40");
    }

    #[test]
    fn rounded_rect_like_pen() {
        let p = rect(100.0, 50.0, [10.0, 0.0, 0.0, 0.0]);
        let c = &p.contours[0];
        assert_eq!(c.anchors.len(), 5);
        assert!(close(c.anchors[0].p, Vec2::new(0.0, 10.0)));
        assert!(close(
            c.anchors[0].handle_out.unwrap(),
            Vec2::new(0.0, 10.0 - 10.0 * KAPPA)
        ));
        assert!(close(c.anchors[1].p, Vec2::new(10.0, 0.0)));
        // Radii larger than the box are scaled down (pill): the two corner
        // arcs of each short side meet, and the meeting anchors merge.
        let pill = rect(100.0, 20.0, [50.0; 4]);
        assert_eq!(pill.contours[0].anchors.len(), 6);
        let b = parse_svg_path(&pill.to_svg()).bounds;
        assert!(
            (b[2] - 100.0).abs() < 1e-3 && (b[3] - 20.0).abs() < 1e-3,
            "{b:?}"
        );
    }

    #[test]
    fn ellipse_like_pen() {
        let p = ellipse(100.0, 60.0, 0.0, 0.0, 360.0);
        let c = &p.contours[0];
        assert!(c.closed);
        assert_eq!(c.anchors.len(), 4);
        // Starts at the top, every anchor has both handles.
        assert!(close(c.anchors[0].p, Vec2::new(50.0, 0.0)));
        assert!(
            c.anchors
                .iter()
                .all(|a| a.handle_in.is_some() && a.handle_out.is_some())
        );
        let b = parse_svg_path(&p.to_svg()).bounds;
        assert!(
            b.iter()
                .zip([0.0, 0.0, 100.0, 60.0])
                .all(|(a, e)| (a - e).abs() < 1e-3)
        );
        // Ring: a second, reversed contour.
        let ring = ellipse(100.0, 100.0, 0.5, 0.0, 360.0);
        assert_eq!(ring.contours.len(), 2);
        // Pie: arc + center.
        let pie = ellipse(100.0, 100.0, 0.0, 0.0, 90.0);
        let a = &pie.contours[0].anchors;
        assert!(close(a[0].p, Vec2::new(100.0, 50.0)));
        assert!(close(a[a.len() - 2].p, Vec2::new(50.0, 0.0)), "{a:?}");
        assert!(close(a[a.len() - 1].p, Vec2::new(50.0, 50.0)));
    }

    #[test]
    fn polygon_like_pen() {
        let p = polygon(100.0, 100.0, 3, 0.0);
        let a = &p.contours[0].anchors;
        assert_eq!(a.len(), 3);
        assert!(close(a[0].p, Vec2::new(50.0, 0.0)));
        let r = polygon(100.0, 100.0, 4, 5.0);
        // Each corner becomes an arc of two anchors.
        assert_eq!(r.contours[0].anchors.len(), 8);
        assert!(polygon(10.0, 10.0, 2, 0.0).contours.is_empty());
    }
}
