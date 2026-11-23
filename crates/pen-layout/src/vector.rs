//! Editable vector paths (pen tool and vector edit mode): SVG path data <->
//! contours of anchors with Bézier handles, Pen's serialization, hit testing,
//! segment splitting and bounds. Pure geometry, no GPUI.
//!
//! The model is Pen's: a contour is a list of anchors, open or closed. The
//! segment between two consecutive anchors is a cubic when either side has a
//! handle (`from.handle_out` / `to.handle_in`) and a line otherwise. A closed
//! contour also has the segment from the last anchor back to the first.
//!
//! Round trip: [`VectorPath::parse`] + [`VectorPath::to_svg`] writes every
//! contour that was not edited with its original text (arcs, quadratics and
//! relative commands included). Edited contours are written the way Pen writes
//! them: absolute `M`/`L`/`C`/`Z`, coordinates rounded to 3 decimals, commands
//! joined by spaces (`M0 0 L10 0 C12 0 14 2 14 4 Z`).

use crate::path::{Lexer, parse_els};
use pen_render::{Affine2, PathEl, Vec2};

/// Handles closer than this to their anchor are dropped when parsing (Pen).
const HANDLE_EPS: f32 = 1e-4;

/// Which handle of an anchor: towards the previous anchor (`In`) or the next
/// one (`Out`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    In,
    Out,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::In => Side::Out,
            Side::Out => Side::In,
        }
    }
}

/// How the opposite handle follows when one handle is dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mirror {
    /// Only the dragged handle moves (corner / broken symmetry).
    None,
    /// The opposite handle keeps its length and stays collinear (smooth).
    Angle,
    /// The opposite handle mirrors angle and length (symmetric).
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub p: Vec2,
    pub handle_in: Option<Vec2>,
    pub handle_out: Option<Vec2>,
}

impl Anchor {
    pub fn corner(p: Vec2) -> Anchor {
        Anchor {
            p,
            handle_in: None,
            handle_out: None,
        }
    }

    pub fn handle(&self, side: Side) -> Option<Vec2> {
        match side {
            Side::In => self.handle_in,
            Side::Out => self.handle_out,
        }
    }

    pub fn set_handle(&mut self, side: Side, h: Option<Vec2>) {
        match side {
            Side::In => self.handle_in = h,
            Side::Out => self.handle_out = h,
        }
    }

    pub fn has_handles(&self) -> bool {
        self.handle_in.is_some() || self.handle_out.is_some()
    }

    /// Moves the anchor and its handles.
    pub fn translate(&mut self, d: Vec2) {
        self.p += d;
        if let Some(h) = &mut self.handle_in {
            *h += d;
        }
        if let Some(h) = &mut self.handle_out {
            *h += d;
        }
    }

    pub fn transform(&mut self, m: &Affine2) {
        self.p = m.transform_point2(self.p);
        self.handle_in = self.handle_in.map(|h| m.transform_point2(h));
        self.handle_out = self.handle_out.map(|h| m.transform_point2(h));
    }

    fn arms(&self) -> Option<(Vec2, Vec2, f32, f32)> {
        let a = self.handle_in? - self.p;
        let b = self.handle_out? - self.p;
        let (la, lb) = (a.length(), b.length());
        (la > 1e-6 && lb > 1e-6).then_some((a, b, la, lb))
    }

    /// Both handles present, opposite and of the same length (Pen: 2% slack).
    pub fn is_symmetric(&self) -> bool {
        self.arms()
            .is_some_and(|(a, b, la, lb)| (a + b).abs().max_element() < 0.02 * la.max(lb))
    }

    /// Both handles present and collinear on opposite sides of the anchor.
    pub fn is_smooth(&self) -> bool {
        self.arms().is_some_and(|(a, b, la, lb)| {
            (a.perp_dot(b) / (la * lb)).abs() < 0.05 && a.dot(b) < 0.0
        })
    }

    /// Sets the handle on `side` to `h` and updates the opposite one.
    pub fn drag_handle(&mut self, side: Side, h: Vec2, mirror: Mirror) {
        self.set_handle(side, Some(h));
        let arm = h - self.p;
        match mirror {
            Mirror::None => {}
            Mirror::Full => self.set_handle(side.opposite(), Some(self.p - arm)),
            Mirror::Angle => {
                if let Some(o) = self.handle(side.opposite()) {
                    let len = (o - self.p).length();
                    let dir = arm.normalize_or_zero();
                    if dir != Vec2::ZERO {
                        self.set_handle(side.opposite(), Some(self.p - dir * len));
                    }
                }
            }
        }
    }
}

/// One Bézier segment (a line has `c1 == p0` and `c2 == p3`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub p0: Vec2,
    pub c1: Vec2,
    pub c2: Vec2,
    pub p3: Vec2,
    pub curve: bool,
}

impl Segment {
    fn between(a: &Anchor, b: &Anchor) -> Segment {
        Segment {
            p0: a.p,
            c1: a.handle_out.unwrap_or(a.p),
            c2: b.handle_in.unwrap_or(b.p),
            p3: b.p,
            curve: a.handle_out.is_some() || b.handle_in.is_some(),
        }
    }

    pub fn at(&self, t: f32) -> Vec2 {
        if !self.curve {
            return self.p0.lerp(self.p3, t);
        }
        let mt = 1.0 - t;
        self.p0 * (mt * mt * mt)
            + self.c1 * (3.0 * mt * mt * t)
            + self.c2 * (3.0 * mt * t * t)
            + self.p3 * (t * t * t)
    }

    /// The same segment under an affine map (Béziers are affine invariant).
    pub fn transformed(&self, m: &Affine2) -> Segment {
        Segment {
            p0: m.transform_point2(self.p0),
            c1: m.transform_point2(self.c1),
            c2: m.transform_point2(self.c2),
            p3: m.transform_point2(self.p3),
            curve: self.curve,
        }
    }

    /// Parameter and distance of the point of the segment nearest to `p`.
    pub fn nearest(&self, p: Vec2) -> (f32, f32) {
        if !self.curve {
            let d = self.p3 - self.p0;
            let l2 = d.length_squared();
            let t = if l2 > 0.0 {
                ((p - self.p0).dot(d) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return (t, self.at(t).distance(p));
        }
        // Coarse sampling, then a shrinking local search around the best one.
        const N: usize = 32;
        let mut best_t = 0.0;
        let mut best = f32::INFINITY;
        for i in 0..=N {
            let t = i as f32 / N as f32;
            let d = self.at(t).distance_squared(p);
            if d < best {
                best = d;
                best_t = t;
            }
        }
        let mut step = 1.0 / N as f32;
        for _ in 0..20 {
            step *= 0.5;
            for t in [best_t - step, best_t + step] {
                let t = t.clamp(0.0, 1.0);
                let d = self.at(t).distance_squared(p);
                if d < best {
                    best = d;
                    best_t = t;
                }
            }
        }
        (best_t, best.sqrt())
    }

    /// Splits at `t` (de Casteljau): left and right halves.
    pub fn split(&self, t: f32) -> (Segment, Segment) {
        let q0 = self.p0.lerp(self.c1, t);
        let q1 = self.c1.lerp(self.c2, t);
        let q2 = self.c2.lerp(self.p3, t);
        let r0 = q0.lerp(q1, t);
        let r1 = q1.lerp(q2, t);
        let s = r0.lerp(r1, t);
        (
            Segment {
                p0: self.p0,
                c1: q0,
                c2: r0,
                p3: s,
                curve: self.curve,
            },
            Segment {
                p0: s,
                c1: r1,
                c2: q2,
                p3: self.p3,
                curve: self.curve,
            },
        )
    }

    fn add_bounds(&self, min: &mut Vec2, max: &mut Vec2) {
        let mut add = |v: Vec2| {
            *min = min.min(v);
            *max = max.max(v);
        };
        add(self.p0);
        add(self.p3);
        if !self.curve {
            return;
        }
        for axis in 0..2 {
            let g = |v: Vec2| if axis == 0 { v.x } else { v.y };
            let (p0, p1, p2, p3) = (g(self.p0), g(self.c1), g(self.c2), g(self.p3));
            let a = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
            let b = 2.0 * (p0 - 2.0 * p1 + p2);
            let c = p1 - p0;
            let mut ts = [f32::NAN; 2];
            if a.abs() < 1e-9 {
                if b.abs() > 1e-9 {
                    ts[0] = -c / b;
                }
            } else {
                let disc = b * b - 4.0 * a * c;
                if disc >= 0.0 {
                    let s = disc.sqrt();
                    ts = [(-b + s) / (2.0 * a), (-b - s) / (2.0 * a)];
                }
            }
            for t in ts {
                if t > 0.0 && t < 1.0 {
                    add(self.at(t));
                }
            }
        }
    }
}

/// Original text of a parsed contour, written back while it is unchanged.
#[derive(Clone, Debug)]
struct Source {
    text: String,
    anchors: Vec<Anchor>,
    closed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Contour {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
    source: Option<Box<Source>>,
}

impl PartialEq for Contour {
    fn eq(&self, o: &Contour) -> bool {
        self.closed == o.closed && self.anchors == o.anchors
    }
}

impl Contour {
    pub fn new(anchors: Vec<Anchor>, closed: bool) -> Contour {
        Contour {
            anchors,
            closed,
            source: None,
        }
    }

    pub fn segment_count(&self) -> usize {
        let n = self.anchors.len();
        match (n, self.closed) {
            (0, _) => 0,
            (1, true) if self.anchors[0].has_handles() => 1,
            (1, _) => 0,
            (n, true) => n,
            (n, false) => n - 1,
        }
    }

    /// Segment starting at anchor `i` (to `i + 1`, wrapping when closed).
    pub fn segment(&self, i: usize) -> Option<Segment> {
        if i >= self.segment_count() {
            return None;
        }
        let n = self.anchors.len();
        Some(Segment::between(
            &self.anchors[i],
            &self.anchors[(i + 1) % n],
        ))
    }

    /// Draws something: two anchors, or one closed anchor looping on itself.
    pub fn is_drawable(&self) -> bool {
        self.segment_count() > 0
    }

    /// Previous and next anchor of `i` (wrapping when closed).
    pub fn neighbors(&self, i: usize) -> (Option<usize>, Option<usize>) {
        let n = self.anchors.len();
        if n < 2 || i >= n {
            return (None, None);
        }
        let prev = if i > 0 {
            Some(i - 1)
        } else {
            self.closed.then_some(n - 1)
        };
        let next = if i + 1 < n {
            Some(i + 1)
        } else {
            self.closed.then_some(0)
        };
        (prev, next)
    }

    /// For an open contour: `Some(Side::In)` when `i` is the first anchor,
    /// `Some(Side::Out)` when it is the last (the side a new point extends).
    pub fn endpoint(&self, i: usize) -> Option<Side> {
        if self.closed || self.anchors.is_empty() {
            return None;
        }
        if i + 1 == self.anchors.len() {
            Some(Side::Out)
        } else if i == 0 {
            Some(Side::In)
        } else {
            None
        }
    }

    fn write(&self, out: &mut String) {
        if let Some(s) = &self.source
            && s.closed == self.closed
            && s.anchors == self.anchors
        {
            out.push_str(&s.text);
            return;
        }
        let a = &self.anchors;
        let Some(first) = a.first() else { return };
        out.push('M');
        push_pt(out, first.p);
        for w in a.windows(2) {
            push_segment(out, &w[0], &w[1]);
        }
        if self.closed && self.segment_count() > 0 {
            push_segment(out, &a[a.len() - 1], first);
            out.push_str(" Z");
        }
    }
}

/// Pen's number format: rounded to 3 decimals, shortest form (`1.5`, `-2`).
pub fn fmt_coord(v: f32) -> String {
    let r = ((v as f64) * 1000.0).round() / 1000.0;
    if r == 0.0 {
        "0".to_string()
    } else {
        format!("{r}")
    }
}

fn push_pt(out: &mut String, p: Vec2) {
    out.push_str(&fmt_coord(p.x));
    out.push(' ');
    out.push_str(&fmt_coord(p.y));
}

fn push_segment(out: &mut String, a: &Anchor, b: &Anchor) {
    let s = Segment::between(a, b);
    if s.curve {
        out.push_str(" C");
        push_pt(out, s.c1);
        out.push(' ');
        push_pt(out, s.c2);
        out.push(' ');
        push_pt(out, s.p3);
    } else {
        out.push_str(" L");
        push_pt(out, s.p3);
    }
}

/// `(contour, anchor)` index pair.
pub type AnchorRef = (usize, usize);

/// A pointer near a segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentHit {
    pub contour: usize,
    /// Index of the segment's first anchor.
    pub segment: usize,
    pub t: f32,
    /// Distance in the hit-test space.
    pub dist: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VectorPath {
    pub contours: Vec<Contour>,
}

impl VectorPath {
    /// Parses SVG path data into contours (Pen's rules: quadratics become
    /// cubics, arcs become cubics, handles on their anchor are dropped, and a
    /// closed contour whose last point repeats the first merges the two).
    pub fn parse(d: &str) -> VectorPath {
        let (els, complete) = parse_els(d);
        let mut contours: Vec<Contour> = Vec::new();
        // Contours started by an `M`/`m` (vs. implicitly after `Z`).
        let mut explicit = 0usize;
        let mut implicit = false;
        let mut cur = Vec2::ZERO;
        let mut start = Vec2::ZERO;
        let mut open = false;
        for el in els {
            let target = match el {
                PathEl::MoveTo(p) => {
                    contours.push(Contour::new(vec![Anchor::corner(p)], false));
                    explicit += 1;
                    open = true;
                    cur = p;
                    start = p;
                    continue;
                }
                PathEl::Close => {
                    if open && let Some(c) = contours.last_mut() {
                        let n = c.anchors.len();
                        if n > 1 && c.anchors[0].p.distance(c.anchors[n - 1].p) < HANDLE_EPS {
                            let last = c.anchors.pop().unwrap();
                            c.anchors[0].handle_in = last.handle_in;
                        }
                        c.closed = true;
                    }
                    open = false;
                    cur = start;
                    continue;
                }
                PathEl::LineTo(p) => (None, None, p),
                PathEl::QuadTo(c, p) => (
                    Some(cur + (c - cur) * (2.0 / 3.0)),
                    Some(p + (c - p) * (2.0 / 3.0)),
                    p,
                ),
                PathEl::CubicTo(c1, c2, p) => (Some(c1), Some(c2), p),
            };
            if !open {
                // Drawing after `Z` without a move: new contour at the start.
                contours.push(Contour::new(vec![Anchor::corner(start)], false));
                implicit = true;
                open = true;
            }
            let (c1, c2, p) = target;
            let c = contours.last_mut().unwrap();
            if let Some(c1) = c1
                && let Some(last) = c.anchors.last_mut()
                && c1.distance(last.p) > HANDLE_EPS
            {
                last.handle_out = Some(c1);
            }
            let mut a = Anchor::corner(p);
            if let Some(c2) = c2
                && c2.distance(p) > HANDLE_EPS
            {
                a.handle_in = Some(c2);
            }
            c.anchors.push(a);
            cur = p;
        }
        let mut path = VectorPath { contours };
        // Keep the original text of each contour when it can be cut out
        // unambiguously: one `M`/`m` per contour and the whole input parsed.
        let starts: Vec<usize> = d
            .bytes()
            .enumerate()
            .filter(|(_, b)| matches!(b, b'M' | b'm'))
            .map(|(i, _)| i)
            .collect();
        if complete && !implicit && explicit == starts.len() && explicit == path.contours.len() {
            for (i, c) in path.contours.iter_mut().enumerate() {
                let end = starts.get(i + 1).copied().unwrap_or(d.len());
                let text = absolute_start(d[starts[i]..end].trim(), c.anchors[0].p);
                c.source = Some(Box::new(Source {
                    text,
                    anchors: c.anchors.clone(),
                    closed: c.closed,
                }));
            }
        }
        path
    }

    /// SVG path data (see the module docs for the format).
    pub fn to_svg(&self) -> String {
        let mut out = String::new();
        for c in &self.contours {
            if !c.is_drawable() && c.source.is_none() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            c.write(&mut out);
        }
        out
    }

    pub fn anchor(&self, r: AnchorRef) -> Option<&Anchor> {
        self.contours.get(r.0)?.anchors.get(r.1)
    }

    pub fn anchor_mut(&mut self, r: AnchorRef) -> Option<&mut Anchor> {
        self.contours.get_mut(r.0)?.anchors.get_mut(r.1)
    }

    pub fn anchor_count(&self) -> usize {
        self.contours.iter().map(|c| c.anchors.len()).sum()
    }

    /// Every anchor reference, in order.
    pub fn anchor_refs(&self) -> impl Iterator<Item = AnchorRef> + '_ {
        self.contours
            .iter()
            .enumerate()
            .flat_map(|(ci, c)| (0..c.anchors.len()).map(move |ai| (ci, ai)))
    }

    /// Same contours with the same anchor counts (indices stay valid).
    pub fn same_structure(&self, o: &VectorPath) -> bool {
        self.contours.len() == o.contours.len()
            && self
                .contours
                .iter()
                .zip(&o.contours)
                .all(|(a, b)| a.anchors.len() == b.anchors.len())
    }

    /// Tight bounds `[min x, min y, max x, max y]` of the drawable contours.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for c in self.contours.iter().filter(|c| c.is_drawable()) {
            for i in 0..c.segment_count() {
                c.segment(i).unwrap().add_bounds(&mut min, &mut max);
            }
        }
        (min.x <= max.x).then_some([min.x, min.y, max.x, max.y])
    }

    /// Anchor nearest to `p` within `radius`, measured after mapping the
    /// geometry with `m` (e.g. geometry -> world).
    pub fn hit_anchor(&self, m: &Affine2, p: Vec2, radius: f32) -> Option<AnchorRef> {
        let mut best = None;
        let mut best_d = radius;
        for r in self.anchor_refs() {
            let d = m.transform_point2(self.anchor(r).unwrap().p).distance(p);
            if d <= best_d {
                best_d = d;
                best = Some(r);
            }
        }
        best
    }

    /// Handle nearest to `p` within `radius` among the anchors accepted by
    /// `shown`. Handles within `min_arm` of their anchor are ignored (the
    /// anchor wins there).
    pub fn hit_handle(
        &self,
        m: &Affine2,
        p: Vec2,
        radius: f32,
        min_arm: f32,
        shown: impl Fn(AnchorRef) -> bool,
    ) -> Option<(AnchorRef, Side)> {
        let mut best = None;
        let mut best_d = radius;
        for r in self.anchor_refs().filter(|&r| shown(r)) {
            let a = self.anchor(r).unwrap();
            let ap = m.transform_point2(a.p);
            for side in [Side::In, Side::Out] {
                let Some(h) = a.handle(side) else { continue };
                let hp = m.transform_point2(h);
                let d = hp.distance(p);
                if d <= best_d && hp.distance(ap) > min_arm {
                    best_d = d;
                    best = Some((r, side));
                }
            }
        }
        best
    }

    /// Segment nearest to `p` within `tol` (in the space mapped by `m`).
    pub fn hit_segment(&self, m: &Affine2, p: Vec2, tol: f32) -> Option<SegmentHit> {
        let mut best: Option<SegmentHit> = None;
        for (ci, c) in self.contours.iter().enumerate() {
            for si in 0..c.segment_count() {
                let s = c.segment(si).unwrap().transformed(m);
                // Cheap reject: control polygon box.
                let lo = s.p0.min(s.p3).min(s.c1.min(s.c2)) - Vec2::splat(tol);
                let hi = s.p0.max(s.p3).max(s.c1.max(s.c2)) + Vec2::splat(tol);
                if p.x < lo.x || p.y < lo.y || p.x > hi.x || p.y > hi.y {
                    continue;
                }
                let (t, dist) = s.nearest(p);
                if dist <= tol && best.is_none_or(|b| dist < b.dist) {
                    best = Some(SegmentHit {
                        contour: ci,
                        segment: si,
                        t,
                        dist,
                    });
                }
            }
        }
        best
    }

    /// Inserts an anchor on segment `si` of contour `ci` at `t` without
    /// changing the shape. Returns the new anchor.
    pub fn split_segment(&mut self, ci: usize, si: usize, t: f32) -> Option<AnchorRef> {
        let c = self.contours.get_mut(ci)?;
        let seg = c.segment(si)?;
        let t = t.clamp(1e-4, 1.0 - 1e-4);
        let n = c.anchors.len();
        let next = (si + 1) % n;
        let new = if seg.curve {
            let (l, r) = seg.split(t);
            if c.anchors[si].handle_out.is_some() {
                c.anchors[si].handle_out = Some(l.c1);
            }
            if c.anchors[next].handle_in.is_some() {
                c.anchors[next].handle_in = Some(r.c2);
            }
            Anchor {
                p: l.p3,
                handle_in: Some(l.c2),
                handle_out: Some(r.c1),
            }
        } else {
            Anchor::corner(seg.at(t))
        };
        c.anchors.insert(si + 1, new);
        Some((ci, si + 1))
    }

    /// Deletes anchors; their neighbors stay connected (Pen). Contours left
    /// with nothing to draw are removed. True if anything was deleted.
    pub fn delete_anchors(&mut self, sel: &[AnchorRef]) -> bool {
        let mut changed = false;
        let mut out = Vec::with_capacity(self.contours.len());
        for (ci, mut c) in std::mem::take(&mut self.contours).into_iter().enumerate() {
            let before = c.anchors.len();
            let mut ai = 0;
            c.anchors.retain(|_| {
                let keep = !sel.contains(&(ci, ai));
                ai += 1;
                keep
            });
            if c.anchors.len() != before {
                changed = true;
                if !c.is_drawable() {
                    continue;
                }
            }
            out.push(c);
        }
        self.contours = out;
        changed
    }

    /// Double click on an anchor (Pen): a smooth/curved anchor becomes a
    /// corner; one handle gets its mirror; a corner gets symmetric handles
    /// along its neighbors.
    pub fn toggle_smooth(&mut self, r: AnchorRef) -> bool {
        let Some(c) = self.contours.get_mut(r.0) else {
            return false;
        };
        let (prev, next) = c.neighbors(r.1);
        let Some(&a) = c.anchors.get(r.1) else {
            return false;
        };
        let mirror = |h: Vec2| a.p * 2.0 - h;
        let new = match (a.handle_in, a.handle_out) {
            (Some(_), Some(_)) => Anchor::corner(a.p),
            (Some(h), None) => Anchor {
                handle_out: Some(mirror(h)),
                ..a
            },
            (None, Some(h)) => Anchor {
                handle_in: Some(mirror(h)),
                ..a
            },
            (None, None) => {
                let pc = prev.map(|i| {
                    let b = &c.anchors[i];
                    b.handle_out.unwrap_or(b.p)
                });
                let nc = next.map(|i| {
                    let b = &c.anchors[i];
                    b.handle_in.unwrap_or(b.p)
                });
                let d = match (pc, nc) {
                    (Some(pc), Some(nc)) => (nc - pc) / 6.0,
                    (None, Some(nc)) => (nc - a.p) / 3.0,
                    (Some(pc), None) => (a.p - pc) / 3.0,
                    (None, None) => Vec2::ZERO,
                };
                if d == Vec2::ZERO {
                    return false;
                }
                Anchor {
                    p: a.p,
                    handle_in: pc.map(|_| a.p - d),
                    handle_out: nc.map(|_| a.p + d),
                }
            }
        };
        let changed = new != a;
        c.anchors[r.1] = new;
        changed
    }

    /// Moves anchors (with their handles) by `d`.
    pub fn translate_anchors(&mut self, refs: &[AnchorRef], d: Vec2) {
        for &r in refs {
            if let Some(a) = self.anchor_mut(r) {
                a.translate(d);
            }
        }
    }

    /// Maps every anchor and handle with `m`.
    pub fn transform(&mut self, m: &Affine2) {
        for c in &mut self.contours {
            for a in &mut c.anchors {
                a.transform(m);
            }
        }
    }
}

/// A contour's text with its first move made absolute, so it can be written
/// after any other contour (a relative `m` depends on the previous one).
fn absolute_start(text: &str, p: Vec2) -> String {
    if !text.starts_with('m') {
        return text.to_string();
    }
    let mut lx = Lexer {
        b: text.as_bytes(),
        i: 1,
    };
    let _ = (lx.number(), lx.number());
    let rest = &text[lx.i..];
    lx.skip_sep();
    // Implicit coordinates after `m` are relative line-tos.
    let implicit =
        lx.b.get(lx.i)
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'));
    format!(
        "M{} {}{}{}",
        p.x,
        p.y,
        if implicit { " l" } else { "" },
        rest
    )
}

/// Constrains `p` to the nearest multiple of 45° around `origin` (Shift),
/// projecting it onto that ray.
pub fn constrain_45(origin: Vec2, p: Vec2) -> Vec2 {
    let v = p - origin;
    if v == Vec2::ZERO {
        return p;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let a = (v.y.atan2(v.x) / step).round() * step;
    let dir = Vec2::new(a.cos(), a.sin());
    // Exact axes (no 1e-8 drift on horizontal / vertical lines).
    let dir = Vec2::new(
        if dir.x.abs() < 1e-6 { 0.0 } else { dir.x },
        if dir.y.abs() < 1e-6 { 0.0 } else { dir.y },
    );
    origin + dir * v.dot(dir).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::parse_svg_path;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    #[test]
    fn parse_lines_and_close_merge() {
        let p = VectorPath::parse("M0 0 L10 0 L10 10 L0 0 Z");
        assert_eq!(p.contours.len(), 1);
        let c = &p.contours[0];
        assert!(c.closed);
        assert_eq!(c.anchors.len(), 3, "repeated start point merged");
        assert_eq!(c.segment_count(), 3);
        let p = VectorPath::parse("M12 0l12 12-12 12-12-12 12-12z");
        assert_eq!(p.contours[0].anchors.len(), 4);
        assert!(p.contours[0].closed);
    }

    #[test]
    fn parse_curves_and_drop_degenerate_handles() {
        // c1 on the start point: no out handle on the first anchor.
        let p = VectorPath::parse("M0 0 C0 0 10 10 10 0");
        let a = &p.contours[0].anchors;
        assert_eq!(a[0].handle_out, None);
        assert_eq!(a[1].handle_in, Some(v(10.0, 10.0)));
        // Quadratic -> cubic.
        let p = VectorPath::parse("M0 0 Q 6 6 12 0");
        let a = &p.contours[0].anchors;
        assert_eq!(a[0].handle_out, Some(v(4.0, 4.0)));
        assert_eq!(a[1].handle_in, Some(v(8.0, 4.0)));
    }

    #[test]
    fn untouched_round_trip_is_verbatim() {
        for d in [
            "M7 0l0 14m7-7l-7 7-7-7",
            "M16 26c1.65686 0 3 1.34314 3 3 0 1.65686-1.34314 3-3 3z m-10-2c1.10457 0 2 0.89543 2 2z",
            "M0 0h100l-28 33.333 28 33.334h-100l28-33.334z",
            "M.5.5a5 5 0 1010 0",
            "M0 0 Q 5 5 10 0 T 20 0",
        ] {
            let p = VectorPath::parse(d);
            let out = p.to_svg();
            // Same drawing (relative first moves became absolute).
            let a = parse_svg_path(d);
            let b = parse_svg_path(&out);
            assert_eq!(a.els, b.els, "{d} -> {out}");
            // Idempotent.
            assert_eq!(VectorPath::parse(&out).to_svg(), out);
        }
        // Absolute starts are kept byte for byte.
        let d = "M0 0h100l-28 33.333 28 33.334h-100l28-33.334z";
        assert_eq!(VectorPath::parse(d).to_svg(), d);
    }

    #[test]
    fn edited_contour_is_rewritten_others_kept() {
        let d = "M0 0h10v10z M20 20q5 5 10 0";
        let mut p = VectorPath::parse(d);
        p.translate_anchors(&[(0, 1)], v(1.0, 0.0));
        let out = p.to_svg();
        assert_eq!(out, "M0 0 L11 0 L10 10 L0 0 Z M20 20q5 5 10 0");
        // Second contour still draws the same.
        let a = parse_svg_path("M20 20q5 5 10 0");
        let b = parse_svg_path(&out);
        assert_eq!(&b.els[b.els.len() - 2..], &a.els[..]);
    }

    #[test]
    fn relative_first_move_with_implicit_lines() {
        // `m` followed by implicit (relative) line-tos.
        let d = "M0 0 L1 1 m5 5 1 0 0 1";
        let p = VectorPath::parse(d);
        let out = p.to_svg();
        assert_eq!(parse_svg_path(d).els, parse_svg_path(&out).els, "{out}");
        assert!(out.contains("M6 6 l"), "{out}");
    }

    #[test]
    fn serialize_pen_format() {
        let mut p = VectorPath::default();
        p.contours.push(Contour::new(
            vec![
                Anchor::corner(v(0.0, 0.0)),
                Anchor {
                    p: v(10.0, 0.0),
                    handle_in: Some(v(8.0, -2.0)),
                    handle_out: None,
                },
                Anchor::corner(v(1.0 / 3.0, 10.0)),
            ],
            true,
        ));
        assert_eq!(p.to_svg(), "M0 0 C0 0 8 -2 10 0 L0.333 10 L0 0 Z");
        assert_eq!(fmt_coord(-0.0004), "0");
        assert_eq!(fmt_coord(2.5), "2.5");
        // Contours with a single point are not written.
        p.contours
            .push(Contour::new(vec![Anchor::corner(v(5.0, 5.0))], false));
        assert!(!p.to_svg().contains("M5 5"));
    }

    #[test]
    fn bounds_match_renderer() {
        for d in [
            "M0 0 C0 10 10 10 10 0",
            "M7 0l0 14m7-7l-7 7-7-7",
            "M0 0 Q 5 5 10 0 T 20 0",
        ] {
            let p = VectorPath::parse(d);
            let b = p.bounds().unwrap();
            let r = parse_svg_path(d).bounds;
            for i in 0..4 {
                assert!((b[i] - r[i]).abs() < 1e-3, "{d}: {b:?} vs {r:?}");
            }
        }
        assert_eq!(VectorPath::parse("").bounds(), None);
    }

    #[test]
    fn hit_testing() {
        let p = VectorPath::parse("M0 0 L100 0 C100 50 50 100 0 100");
        // Geometry scaled 2x into the hit space.
        let m = Affine2::from_scale(Vec2::splat(2.0));
        assert_eq!(p.hit_anchor(&m, v(201.0, 1.0), 4.0), Some((0, 1)));
        assert_eq!(p.hit_anchor(&m, v(100.0, 10.0), 4.0), None);
        let h = p.hit_segment(&m, v(100.0, 3.0), 4.0).unwrap();
        assert_eq!((h.contour, h.segment), (0, 0));
        assert!((h.t - 0.5).abs() < 1e-3);
        // On the curve.
        let s = p.contours[0].segment(1).unwrap().transformed(&m);
        let q = s.at(0.3);
        let h = p.hit_segment(&m, q + v(1.0, 0.0), 3.0).unwrap();
        assert_eq!(h.segment, 1);
        assert!((h.t - 0.3).abs() < 0.02, "{}", h.t);
        // Handles: the out handle of anchor 1 at (100, 50) -> (200, 100).
        let hh = p.hit_handle(&m, v(199.0, 101.0), 5.0, 4.0, |_| true);
        assert_eq!(hh, Some(((0, 1), Side::Out)));
        assert_eq!(p.hit_handle(&m, v(199.0, 101.0), 5.0, 4.0, |_| false), None);
    }

    #[test]
    fn split_keeps_shape() {
        let mut p = VectorPath::parse("M0 0 C0 50 100 50 100 0");
        let before = p.contours[0].segment(0).unwrap();
        let r = p.split_segment(0, 0, 0.25).unwrap();
        assert_eq!(r, (0, 1));
        let c = &p.contours[0];
        assert_eq!(c.anchors.len(), 3);
        let mid = c.anchors[1].p;
        assert!(mid.distance(before.at(0.25)) < 1e-4);
        // Points on the halves lie on the original curve.
        let l = c.segment(0).unwrap();
        let rr = c.segment(1).unwrap();
        assert!(l.at(0.5).distance(before.at(0.125)) < 1e-3);
        assert!(rr.at(0.5).distance(before.at(0.625)) < 1e-3);
        // Line split: corner point, closing segment too.
        let mut p = VectorPath::parse("M0 0 L10 0 L10 10 Z");
        let r = p.split_segment(0, 2, 0.5).unwrap();
        assert_eq!(r, (0, 3));
        assert_eq!(p.contours[0].anchors[3], Anchor::corner(v(5.0, 5.0)));
        assert_eq!(p.to_svg(), "M0 0 L10 0 L10 10 L5 5 L0 0 Z");
    }

    #[test]
    fn delete_and_toggle() {
        let mut p = VectorPath::parse("M0 0 L10 0 L10 10 L0 10 Z");
        assert!(p.delete_anchors(&[(0, 1)]));
        assert_eq!(p.to_svg(), "M0 0 L10 10 L0 10 L0 0 Z");
        // Down to one point: the contour goes away.
        assert!(p.delete_anchors(&[(0, 0), (0, 1)]));
        assert!(p.contours.is_empty());
        assert!(!p.delete_anchors(&[(0, 0)]));

        let mut p = VectorPath::parse("M0 0 L10 0 L20 0");
        assert!(p.toggle_smooth((0, 1)));
        let a = p.contours[0].anchors[1];
        assert_eq!(a.handle_in, Some(v(10.0 - 20.0 / 6.0, 0.0)));
        assert!(a.is_symmetric() && a.is_smooth());
        assert!(p.toggle_smooth((0, 1)));
        assert_eq!(p.contours[0].anchors[1], Anchor::corner(v(10.0, 0.0)));
        // Endpoint: one handle towards the neighbor.
        assert!(p.toggle_smooth((0, 0)));
        assert_eq!(
            p.contours[0].anchors[0].handle_out,
            Some(v(10.0 / 3.0, 0.0))
        );
        assert_eq!(p.contours[0].anchors[0].handle_in, None);
        // One handle -> mirrored.
        assert!(p.toggle_smooth((0, 0)));
        assert_eq!(
            p.contours[0].anchors[0].handle_in,
            Some(v(-10.0 / 3.0, 0.0))
        );
    }

    #[test]
    fn handle_mirroring() {
        let mut a = Anchor {
            p: v(0.0, 0.0),
            handle_in: Some(v(-5.0, 0.0)),
            handle_out: Some(v(10.0, 0.0)),
        };
        assert!(a.is_smooth() && !a.is_symmetric());
        a.drag_handle(Side::Out, v(0.0, 20.0), Mirror::Angle);
        assert!(a.handle_in.unwrap().distance(v(0.0, -5.0)) < 1e-5);
        a.drag_handle(Side::Out, v(3.0, 4.0), Mirror::Full);
        assert_eq!(a.handle_in, Some(v(-3.0, -4.0)));
        assert!(a.is_symmetric());
        a.drag_handle(Side::In, v(1.0, 1.0), Mirror::None);
        assert_eq!(a.handle_out, Some(v(3.0, 4.0)));
    }

    /// Every path of the corpus survives parse + write unchanged (drawing
    /// and text). Skipped when `testdata/` is absent.
    #[test]
    fn corpus_paths_round_trip() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let mut seen = 0;
        for dir in ["legacy", "user"] {
            let Ok(rd) = std::fs::read_dir(root.join(dir)) else {
                continue;
            };
            for f in rd.flatten() {
                let Ok(text) = std::fs::read_to_string(f.path()) else {
                    continue;
                };
                for part in text.split("\"geometry\"").skip(1) {
                    let Some(s) = part.trim_start().strip_prefix(':') else {
                        continue;
                    };
                    let Some(s) = s.trim_start().strip_prefix('"') else {
                        continue;
                    };
                    let Some(end) = s.find('"') else { continue };
                    let d = &s[..end];
                    let out = VectorPath::parse(d).to_svg();
                    assert_eq!(parse_svg_path(d).els, parse_svg_path(&out).els, "{d}");
                    if d.trim_start().starts_with('M') && !d.contains(['m', 'Z', 'z']) {
                        assert_eq!(out, d.trim());
                    }
                    seen += 1;
                }
            }
        }
        eprintln!("{seen} corpus paths round-tripped");
    }

    #[test]
    fn shift_constraint() {
        assert_eq!(constrain_45(v(0.0, 0.0), v(10.0, 2.0)), v(10.0, 0.0));
        assert_eq!(constrain_45(v(0.0, 0.0), v(-1.0, 9.0)), v(0.0, 9.0));
        let d = constrain_45(v(0.0, 0.0), v(10.0, 9.0));
        assert!((d.x - d.y).abs() < 1e-4 && (d.x - 9.5).abs() < 1e-3);
    }
}
