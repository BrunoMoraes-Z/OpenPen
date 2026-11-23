//! Pure canvas geometry (no GPUI): oriented boxes, the spatial index used for
//! hit-testing, snapping to sibling edges/centers, resize and rotation math.
//!
//! Conventions follow pen-layout: a node's local box is `[0, w] x [0, h]`,
//! placed in its parent at `(x, y)` and rotated by `rotation` degrees
//! counter-clockwise around its top-left corner (`Affine2::from_angle(-θ)`).

use std::collections::HashMap;

use pen_render::{Affine2, Rect, Vec2};

// ----------------------------------------------------------------- boxes

/// A node's layout box in world space: `t` maps local `[0,w]x[0,h]` to world.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Obb {
    pub t: Affine2,
    pub w: f32,
    pub h: f32,
}

impl Obb {
    pub fn new(t: Affine2, w: f32, h: f32) -> Self {
        Self { t, w, h }
    }

    pub fn from_rect(r: Rect) -> Self {
        Self::new(Affine2::from_translation(Vec2::new(r.x, r.y)), r.w, r.h)
    }

    /// World point at normalized local coordinates (`u`, `v` in 0..1).
    pub fn at(&self, u: f32, v: f32) -> Vec2 {
        self.t.transform_point2(Vec2::new(u * self.w, v * self.h))
    }

    /// Corners clockwise from the top-left.
    pub fn corners(&self) -> [Vec2; 4] {
        [
            self.at(0.0, 0.0),
            self.at(1.0, 0.0),
            self.at(1.0, 1.0),
            self.at(0.0, 1.0),
        ]
    }

    pub fn aabb(&self) -> Rect {
        Rect::new(0.0, 0.0, self.w, self.h).transformed(&self.t)
    }

    /// Point in local coordinates (`None` for a degenerate transform).
    pub fn to_local(&self, p: Vec2) -> Option<Vec2> {
        if self.t.matrix2.determinant().abs() < 1e-12 {
            return None;
        }
        Some(self.t.inverse().transform_point2(p))
    }

    /// Whether `p` lies in the box, grown by `tol` (local units) on axes
    /// thinner than `2 * tol` (so lines stay clickable).
    pub fn contains(&self, p: Vec2, tol: f32) -> bool {
        let Some(l) = self.to_local(p) else {
            return false;
        };
        let tx = if self.w < 2.0 * tol { tol } else { 0.0 };
        let ty = if self.h < 2.0 * tol { tol } else { 0.0 };
        l.x >= -tx && l.x <= self.w + tx && l.y >= -ty && l.y <= self.h + ty
    }

    /// Rotation of the box's x axis, in degrees (screen convention).
    pub fn angle_deg(&self) -> f32 {
        let a = self.t.matrix2.x_axis;
        a.y.atan2(a.x).to_degrees()
    }
}

pub fn rect_contains(r: &Rect, p: Vec2) -> bool {
    p.x >= r.x && p.x <= r.right() && p.y >= r.y && p.y <= r.bottom()
}

pub fn rect_from_points(a: Vec2, b: Vec2) -> Rect {
    let min = a.min(b);
    let max = a.max(b);
    Rect::new(min.x, min.y, max.x - min.x, max.y - min.y)
}

pub fn union_all(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    rects.into_iter().reduce(|a, b| a.union(&b))
}

/// Rotation matrix for `deg` counter-clockwise (pen-layout convention).
pub fn rot(deg: f32) -> glam::Mat2 {
    glam::Mat2::from_angle(-deg.to_radians())
}

// --------------------------------------------------------- spatial index

/// Uniform-grid spatial index over world AABBs. Items spanning too many cells
/// go to a small "large" list scanned on every query.
#[derive(Debug, Default, Clone)]
pub struct SpatialIndex<T: Copy> {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<u32>>,
    large: Vec<u32>,
    items: Vec<(Rect, T)>,
}

const MAX_CELLS_PER_ITEM: i64 = 64;

impl<T: Copy> SpatialIndex<T> {
    pub fn build(items: Vec<(Rect, T)>) -> Self {
        let n = items.len().max(1) as f32;
        let total = union_all(items.iter().map(|i| i.0)).unwrap_or_default();
        // ~4 items per cell on a uniform distribution, at least 64 units.
        let cell = ((total.w.max(1.0) * total.h.max(1.0) / n).sqrt() * 2.0).clamp(64.0, 1.0e6);
        let mut s = Self {
            cell,
            cells: HashMap::new(),
            large: Vec::new(),
            items,
        };
        for i in 0..s.items.len() {
            let r = s.items[i].0;
            if !(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite()) {
                continue;
            }
            let (x0, y0, x1, y1) = s.cell_range(&r);
            let span = (x1 as i64 - x0 as i64 + 1) * (y1 as i64 - y0 as i64 + 1);
            if span > MAX_CELLS_PER_ITEM {
                s.large.push(i as u32);
                continue;
            }
            for cy in y0..=y1 {
                for cx in x0..=x1 {
                    s.cells.entry((cx, cy)).or_default().push(i as u32);
                }
            }
        }
        s
    }

    fn cell_of(&self, v: f32) -> i32 {
        (v / self.cell).floor().clamp(-1.0e9, 1.0e9) as i32
    }

    fn cell_range(&self, r: &Rect) -> (i32, i32, i32, i32) {
        (
            self.cell_of(r.x),
            self.cell_of(r.y),
            self.cell_of(r.right()),
            self.cell_of(r.bottom()),
        )
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Items whose AABB contains `p` (unordered).
    pub fn query_point(&self, p: Vec2) -> Vec<T> {
        let key = (self.cell_of(p.x), self.cell_of(p.y));
        let cell = self.cells.get(&key).map(Vec::as_slice).unwrap_or(&[]);
        cell.iter()
            .chain(self.large.iter())
            .map(|&i| &self.items[i as usize])
            .filter(|(r, _)| rect_contains(r, p))
            .map(|(_, t)| *t)
            .collect()
    }

    /// Items whose AABB intersects `q` (unordered, deduplicated).
    pub fn query_rect(&self, q: &Rect) -> Vec<T> {
        let (x0, y0, x1, y1) = self.cell_range(q);
        let span = (x1 as i64 - x0 as i64 + 1) * (y1 as i64 - y0 as i64 + 1);
        let mut seen = vec![false; self.items.len()];
        let mut out = Vec::new();
        let mut push = |i: u32, out: &mut Vec<T>| {
            let (r, t) = &self.items[i as usize];
            if !seen[i as usize] && (r.intersects(q) || touches(r, q)) {
                seen[i as usize] = true;
                out.push(*t);
            }
        };
        if span > 4096 {
            for i in 0..self.items.len() as u32 {
                push(i, &mut out);
            }
            return out;
        }
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                if let Some(c) = self.cells.get(&(cx, cy)) {
                    for &i in c {
                        push(i, &mut out);
                    }
                }
            }
        }
        for &i in &self.large {
            push(i, &mut out);
        }
        out
    }
}

/// Zero-area rects (lines) still "intersect" when they overlap the query.
fn touches(r: &Rect, q: &Rect) -> bool {
    r.x <= q.right() && q.x <= r.right() && r.y <= q.bottom() && q.y <= r.bottom()
}

// -------------------------------------------------------------- snapping

/// Screen-space snap threshold (Pen: 5 px, i.e. `5 / zoom` in the document).
pub const SNAP_THRESHOLD_PX: f32 = 5.0;

/// A red snap guide line (world space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    /// `true`: vertical line at x = `pos`, from y = `from` to `to`.
    pub vertical: bool,
    pub pos: f32,
    pub from: f32,
    pub to: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub dx: f32,
    pub dy: f32,
    pub guides: Vec<Guide>,
}

fn lines_x(r: &Rect) -> [f32; 3] {
    [r.x, r.x + r.w * 0.5, r.right()]
}

fn lines_y(r: &Rect) -> [f32; 3] {
    [r.y, r.y + r.h * 0.5, r.bottom()]
}

/// Snaps `moving` (already at its candidate position) to the edges and
/// centers of `targets`, independently per axis, within `threshold` world
/// units. Returns the correction and the guides to draw.
pub fn snap_rect(moving: &Rect, targets: &[Rect], threshold: f32) -> Snap {
    let best = |mine: [f32; 3], theirs: &dyn Fn(&Rect) -> [f32; 3]| -> Option<f32> {
        let mut best: Option<f32> = None;
        for t in targets {
            for tv in theirs(t) {
                for mv in mine {
                    let d = tv - mv;
                    if d.abs() <= threshold && best.is_none_or(|b| d.abs() < b.abs()) {
                        best = Some(d);
                    }
                }
            }
        }
        best
    };
    let dx = best(lines_x(moving), &lines_x).unwrap_or(0.0);
    let dy = best(lines_y(moving), &lines_y).unwrap_or(0.0);
    let snapped_x = dx != 0.0 || best(lines_x(moving), &lines_x).is_some();
    let snapped_y = dy != 0.0 || best(lines_y(moving), &lines_y).is_some();
    let m = Rect::new(moving.x + dx, moving.y + dy, moving.w, moving.h);
    let mut guides = Vec::new();
    const EPS: f32 = 0.01;
    if snapped_x {
        for mv in lines_x(&m) {
            let mut span: Option<(f32, f32)> = None;
            for t in targets {
                if lines_x(t).iter().any(|tv| (tv - mv).abs() < EPS) {
                    let (a, b) = span.unwrap_or((m.y, m.bottom()));
                    span = Some((a.min(t.y), b.max(t.bottom())));
                }
            }
            if let Some((from, to)) = span {
                guides.push(Guide {
                    vertical: true,
                    pos: mv,
                    from,
                    to,
                });
            }
        }
    }
    if snapped_y {
        for mv in lines_y(&m) {
            let mut span: Option<(f32, f32)> = None;
            for t in targets {
                if lines_y(t).iter().any(|tv| (tv - mv).abs() < EPS) {
                    let (a, b) = span.unwrap_or((m.x, m.right()));
                    span = Some((a.min(t.x), b.max(t.right())));
                }
            }
            if let Some((from, to)) = span {
                guides.push(Guide {
                    vertical: false,
                    pos: mv,
                    from,
                    to,
                });
            }
        }
    }
    Snap { dx, dy, guides }
}

fn snap_guides(r: &Rect, xs: &[f32], ys: &[f32], targets: &[Rect]) -> Vec<Guide> {
    const EPS: f32 = 0.01;
    let mut guides = Vec::new();
    for &x in xs {
        let mut span: Option<(f32, f32)> = None;
        for t in targets {
            if lines_x(t).iter().any(|tv| (tv - x).abs() < EPS) {
                let (a, b) = span.unwrap_or((r.y, r.bottom()));
                span = Some((a.min(t.y), b.max(t.bottom())));
            }
        }
        if let Some((from, to)) = span {
            guides.push(Guide {
                vertical: true,
                pos: x,
                from,
                to,
            });
        }
    }
    for &y in ys {
        let mut span: Option<(f32, f32)> = None;
        for t in targets {
            if lines_y(t).iter().any(|tv| (tv - y).abs() < EPS) {
                let (a, b) = span.unwrap_or((r.x, r.right()));
                span = Some((a.min(t.x), b.max(t.right())));
            }
        }
        if let Some((from, to)) = span {
            guides.push(Guide {
                vertical: false,
                pos: y,
                from,
                to,
            });
        }
    }
    guides
}

/// Resize snapping: the edges of `r` (the resized box, world AABB) that
/// follow the pointer `p` for `handle` snap to the edges and centers of the
/// targets. The correction is to be added to the pointer.
pub fn snap_resize(r: &Rect, handle: Handle, p: Vec2, targets: &[Rect], threshold: f32) -> Snap {
    let (hu, hv) = handle.uv();
    // The edge nearest the pointer is the moving one (handles can flip).
    let near = |lo: f32, hi: f32, v: f32| {
        if (v - lo).abs() <= (v - hi).abs() {
            lo
        } else {
            hi
        }
    };
    let mx = (hu != 0.5).then(|| near(r.x, r.right(), p.x));
    let my = (hv != 0.5).then(|| near(r.y, r.bottom(), p.y));
    let best = |mine: Option<f32>, theirs: &dyn Fn(&Rect) -> [f32; 3]| -> Option<f32> {
        let mv = mine?;
        let mut best: Option<f32> = None;
        for t in targets {
            for tv in theirs(t) {
                let d = tv - mv;
                if d.abs() <= threshold && best.is_none_or(|b| d.abs() < b.abs()) {
                    best = Some(d);
                }
            }
        }
        best
    };
    let bx = best(mx, &lines_x);
    let by = best(my, &lines_y);
    let (dx, dy) = (bx.unwrap_or(0.0), by.unwrap_or(0.0));
    let xs: Vec<f32> = bx.and(mx).map(|x| x + dx).into_iter().collect();
    let ys: Vec<f32> = by.and(my).map(|y| y + dy).into_iter().collect();
    // Guides span the box as it will be once corrected.
    let mut fixed = *r;
    if let Some(x) = mx {
        if x == r.x {
            fixed.x += dx;
            fixed.w -= dx;
        } else {
            fixed.w += dx;
        }
    }
    if let Some(y) = my {
        if y == r.y {
            fixed.y += dy;
            fixed.h -= dy;
        } else {
            fixed.h += dy;
        }
    }
    Snap {
        dx,
        dy,
        guides: snap_guides(&fixed, &xs, &ys, targets),
    }
}

/// Equal-spacing snap of a moved box: per axis, centered between its two
/// nearest neighbours (that overlap it on the other axis), or repeating the
/// gap between a neighbour and the next one. Returns the correction (0 when
/// nothing is within `threshold`) and the gap segments to label.
pub fn snap_spacing(
    m: &Rect,
    targets: &[Rect],
    threshold: f32,
) -> (Vec2, Vec<crate::overlay::Measure>) {
    use crate::overlay::Measure;
    let mut d = Vec2::ZERO;
    let mut out = Vec::new();
    for horizontal in [true, false] {
        let main = |r: &Rect| {
            if horizontal {
                (r.x, r.right())
            } else {
                (r.y, r.bottom())
            }
        };
        let cross = |r: &Rect| {
            if horizontal {
                (r.y, r.bottom())
            } else {
                (r.x, r.right())
            }
        };
        let (ms, me) = main(m);
        let len = me - ms;
        let (cs, ce) = cross(m);
        let row: Vec<(f32, f32)> = targets
            .iter()
            .filter(|r| {
                let (a, b) = cross(r);
                a < ce && b > cs
            })
            .map(main)
            .collect();
        let before = row
            .iter()
            .copied()
            .filter(|r| r.1 <= ms + threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let after = row
            .iter()
            .copied()
            .filter(|r| r.0 >= me - threshold)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        // Candidate start positions with the gap segments they would show.
        let mut cands: Vec<(f32, [(f32, f32); 2])> = Vec::new();
        if let (Some(b), Some(a)) = (before, after) {
            let free = a.0 - b.1 - len;
            if free >= 0.0 {
                let s = b.1 + free / 2.0;
                cands.push((s, [(b.1, s), (s + len, a.0)]));
            }
        }
        if let Some(b) = before
            && let Some(pv) = row
                .iter()
                .copied()
                .filter(|r| r.1 <= b.0)
                .max_by(|x, y| x.1.total_cmp(&y.1))
        {
            let s = b.1 + (b.0 - pv.1);
            cands.push((s, [(pv.1, b.0), (b.1, s)]));
        }
        if let Some(a) = after
            && let Some(nx) = row
                .iter()
                .copied()
                .filter(|r| r.0 >= a.1)
                .min_by(|x, y| x.0.total_cmp(&y.0))
        {
            let s = a.0 - (nx.0 - a.1) - len;
            cands.push((s, [(s + len, a.0), (a.1, nx.0)]));
        }
        let Some((s, segs)) = cands
            .into_iter()
            .filter(|(s, _)| (s - ms).abs() <= threshold)
            .min_by(|a, b| (a.0 - ms).abs().total_cmp(&(b.0 - ms).abs()))
        else {
            continue;
        };
        let mid = (cs + ce) / 2.0;
        for (a, b) in segs {
            let (pa, pb) = if horizontal {
                (Vec2::new(a, mid), Vec2::new(b, mid))
            } else {
                (Vec2::new(mid, a), Vec2::new(mid, b))
            };
            out.push(Measure {
                a: pa,
                b: pb,
                value: b - a,
            });
        }
        if horizontal {
            d.x = s - ms;
        } else {
            d.y = s - ms;
        }
    }
    (d, out)
}

// ---------------------------------------------------------------- resize

/// The 8 resize handles of a selection box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::Top,
        Handle::TopRight,
        Handle::Right,
        Handle::BottomRight,
        Handle::Bottom,
        Handle::BottomLeft,
        Handle::Left,
    ];

    /// Normalized position on the box (0, 0.5 or 1 per axis).
    pub fn uv(self) -> (f32, f32) {
        match self {
            Handle::TopLeft => (0.0, 0.0),
            Handle::Top => (0.5, 0.0),
            Handle::TopRight => (1.0, 0.0),
            Handle::Right => (1.0, 0.5),
            Handle::BottomRight => (1.0, 1.0),
            Handle::Bottom => (0.5, 1.0),
            Handle::BottomLeft => (0.0, 1.0),
            Handle::Left => (0.0, 0.5),
        }
    }

    pub fn is_corner(self) -> bool {
        let (u, v) = self.uv();
        u != 0.5 && v != 0.5
    }
}

/// A node box in its parent's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Degrees counter-clockwise around the top-left corner.
    pub rotation: f32,
}

impl BoxGeom {
    /// Parent-space position of a local point.
    pub fn to_parent(&self, local: Vec2) -> Vec2 {
        Vec2::new(self.x, self.y) + rot(self.rotation) * local
    }

    pub fn to_local(&self, parent: Vec2) -> Vec2 {
        rot(self.rotation).inverse() * (parent - Vec2::new(self.x, self.y))
    }

    pub fn center(&self) -> Vec2 {
        self.to_parent(Vec2::new(self.w * 0.5, self.h * 0.5))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResizeMods {
    /// Keep the aspect ratio.
    pub shift: bool,
    /// Resize around the center.
    pub alt: bool,
}

/// New box when dragging `handle` of `b` to `pointer` (parent space). The
/// opposite handle (or the center with `alt`) stays fixed; the rotation is
/// kept. Crossing the anchor flips the box onto the other side.
pub fn resize(b: &BoxGeom, handle: Handle, pointer: Vec2, mods: ResizeMods) -> BoxGeom {
    let (hu, hv) = handle.uv();
    let p = b.to_local(pointer);
    // Per axis: (min, max) of the new local extent.
    let axis = |h: f32, size: f32, pv: f32| -> (f32, f32) {
        if h == 0.5 {
            return (0.0, size);
        }
        if mods.alt {
            let c = size * 0.5;
            let half = if h == 1.0 { pv - c } else { c - pv };
            (c - half.abs(), c + half.abs())
        } else {
            let anchor = if h == 1.0 { 0.0 } else { size };
            (anchor.min(pv), anchor.max(pv))
        }
    };
    let (mut x0, mut x1) = axis(hu, b.w, p.x);
    let (mut y0, mut y1) = axis(hv, b.h, p.y);
    if mods.shift && b.w > 0.0 && b.h > 0.0 {
        let sx = (x1 - x0) / b.w;
        let sy = (y1 - y0) / b.h;
        let s = if handle.is_corner() {
            sx.max(sy)
        } else if hu == 0.5 {
            sy
        } else {
            sx
        };
        let (nw, nh) = (b.w * s, b.h * s);
        // Re-anchor each axis: the fixed side (or center) stays put; an axis
        // without a handle grows around its center.
        let fit = |h: f32, lo: f32, hi: f32, size: f32, new: f32, flipped: bool| -> (f32, f32) {
            if h == 0.5 || mods.alt {
                let c = if h == 0.5 {
                    size * 0.5
                } else {
                    (lo + hi) * 0.5
                };
                (c - new * 0.5, c + new * 0.5)
            } else if (h == 1.0) != flipped {
                (lo, lo + new)
            } else {
                (hi - new, hi)
            }
        };
        let flip_x = if hu == 1.0 { p.x < 0.0 } else { p.x > b.w };
        let flip_y = if hv == 1.0 { p.y < 0.0 } else { p.y > b.h };
        (x0, x1) = fit(hu, x0, x1, b.w, nw, flip_x);
        (y0, y1) = fit(hv, y0, y1, b.h, nh, flip_y);
    }
    let origin = b.to_parent(Vec2::new(x0, y0));
    BoxGeom {
        x: origin.x,
        y: origin.y,
        w: x1 - x0,
        h: y1 - y0,
        rotation: b.rotation,
    }
}

/// Rounds a box to whole pixels (Pen's "snap to pixel grid") when unrotated.
pub fn round_box(b: BoxGeom) -> BoxGeom {
    if b.rotation != 0.0 {
        return b;
    }
    let x0 = b.x.round();
    let y0 = b.y.round();
    let x1 = (b.x + b.w).round();
    let y1 = (b.y + b.h).round();
    BoxGeom {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
        rotation: 0.0,
    }
}

// -------------------------------------------------------------- rotation

/// Rotates `b` around its center so that the box turns by the angle swept
/// from `start` to `pointer` (both parent space). `shift` snaps the final
/// rotation to 15° steps.
pub fn rotate_around_center(b: &BoxGeom, start: Vec2, pointer: Vec2, shift: bool) -> BoxGeom {
    let c = b.center();
    let a0 = (start - c).to_angle();
    let a1 = (pointer - c).to_angle();
    // Screen angles grow clockwise (y down); the data rotation is ccw.
    let mut rotation = b.rotation - (a1 - a0).to_degrees();
    if shift {
        rotation = (rotation / 15.0).round() * 15.0;
    }
    rotation = normalize_deg(rotation);
    let half = Vec2::new(b.w * 0.5, b.h * 0.5);
    let origin = c - rot(rotation) * half;
    BoxGeom {
        x: origin.x,
        y: origin.y,
        w: b.w,
        h: b.h,
        rotation,
    }
}

/// Keeps an angle in (-180, 180].
pub fn normalize_deg(d: f32) -> f32 {
    if !d.is_finite() {
        return 0.0;
    }
    let mut d = d % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d <= -180.0 {
        d += 360.0;
    }
    if d.abs() < 1e-4 { 0.0 } else { d }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn resize_snaps_moving_edge_only() {
        let target = Rect::new(200.0, 0.0, 50.0, 50.0);
        // Box 0..197 wide being resized by its right edge (pointer at 197).
        let r = Rect::new(0.0, 100.0, 197.0, 40.0);
        let s = snap_resize(&r, Handle::Right, Vec2::new(197.0, 120.0), &[target], 5.0);
        assert_eq!((s.dx, s.dy), (3.0, 0.0));
        assert_eq!(s.guides.len(), 1);
        assert!(s.guides[0].vertical && s.guides[0].pos == 200.0);
        assert_eq!((s.guides[0].from, s.guides[0].to), (0.0, 140.0));
        // Top handle: no x snap.
        let s = snap_resize(&r, Handle::Top, Vec2::new(197.0, 100.0), &[target], 5.0);
        assert_eq!(s.dx, 0.0);
        // Too far: no snap.
        let far = Rect::new(0.0, 100.0, 190.0, 40.0);
        let s = snap_resize(
            &far,
            Handle::BottomRight,
            Vec2::new(190.0, 140.0),
            &[target],
            5.0,
        );
        assert_eq!((s.dx, s.dy), (0.0, 0.0));
        assert!(s.guides.is_empty());
        // Center line of the target (y = 25) snaps a moving bottom edge.
        let r = Rect::new(300.0, 0.0, 20.0, 23.0);
        let s = snap_resize(&r, Handle::Bottom, Vec2::new(310.0, 23.0), &[target], 5.0);
        assert_eq!(s.dy, 2.0);
    }

    #[test]
    fn spacing_snaps_to_equal_gaps() {
        let l = Rect::new(0.0, 0.0, 10.0, 10.0);
        let r = Rect::new(100.0, 0.0, 10.0, 10.0);
        // 20 wide between 10 and 100: centered at x = 45.
        let m = Rect::new(43.0, 0.0, 20.0, 10.0);
        let (d, segs) = snap_spacing(&m, &[l, r], 5.0);
        assert_eq!(d, Vec2::new(2.0, 0.0));
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].value, 35.0);
        assert_eq!(segs[1].value, 35.0);
        // Repeat the gap of a row: a(0..10) gap 20 b(30..40) -> next at 60.
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(30.0, 0.0, 10.0, 10.0);
        let m = Rect::new(62.0, 0.0, 10.0, 10.0);
        let (d, segs) = snap_spacing(&m, &[a, b], 5.0);
        assert_eq!(d.x, -2.0);
        assert!(segs.iter().all(|s| s.value == 20.0));
        // Not overlapping on the cross axis: ignored.
        let m = Rect::new(62.0, 50.0, 10.0, 10.0);
        assert_eq!(snap_spacing(&m, &[a, b], 5.0).0, Vec2::ZERO);
    }

    fn bx(x: f32, y: f32, w: f32, h: f32, r: f32) -> BoxGeom {
        BoxGeom {
            x,
            y,
            w,
            h,
            rotation: r,
        }
    }

    #[test]
    fn obb_contains_and_aabb() {
        let o = Obb::from_rect(Rect::new(10.0, 10.0, 100.0, 50.0));
        assert!(o.contains(Vec2::new(50.0, 30.0), 0.0));
        assert!(!o.contains(Vec2::new(5.0, 30.0), 0.0));
        let t = Affine2::from_translation(Vec2::new(100.0, 100.0))
            * Affine2::from_angle(-90f32.to_radians());
        let r = Obb::new(t, 40.0, 10.0);
        // Rotated 90° ccw: x axis points up on screen.
        let a = r.aabb();
        assert!(close(a.w, 10.0) && close(a.h, 40.0), "{a:?}");
        assert!(r.contains(Vec2::new(105.0, 80.0), 0.0));
        assert!(!r.contains(Vec2::new(105.0, 120.0), 0.0));
        // A zero-height line is hit within the tolerance.
        let line = Obb::from_rect(Rect::new(0.0, 0.0, 100.0, 0.0));
        assert!(line.contains(Vec2::new(50.0, 2.0), 3.0));
        assert!(!line.contains(Vec2::new(50.0, 4.0), 3.0));
    }

    #[test]
    fn spatial_index_queries() {
        let mut items = Vec::new();
        for i in 0..100 {
            let x = (i % 10) as f32 * 100.0;
            let y = (i / 10) as f32 * 100.0;
            items.push((Rect::new(x, y, 50.0, 50.0), i));
        }
        items.push((Rect::new(-1.0e5, -1.0e5, 2.0e5, 2.0e5), 1000));
        let idx = SpatialIndex::build(items);
        let mut hit = idx.query_point(Vec2::new(325.0, 425.0));
        hit.sort();
        assert_eq!(hit, vec![43, 1000]);
        assert_eq!(idx.query_point(Vec2::new(375.0, 425.0)), vec![1000]);
        let mut r = idx.query_rect(&Rect::new(90.0, 90.0, 120.0, 20.0));
        r.sort();
        assert_eq!(r, vec![11, 12, 1000]);
    }

    #[test]
    fn snapping_to_edges_and_centers() {
        let targets = [Rect::new(0.0, 0.0, 100.0, 100.0)];
        // Left edge 3 units right of the target's right edge -> snaps back.
        let s = snap_rect(&Rect::new(103.0, 200.0, 50.0, 50.0), &targets, 5.0);
        assert_eq!((s.dx, s.dy), (-3.0, 0.0));
        assert_eq!(s.guides.len(), 1);
        let g = s.guides[0];
        assert!(g.vertical && g.pos == 100.0 && g.from == 0.0 && g.to == 250.0);
        // Centers: moving center 52 vs target center 50.
        let s = snap_rect(&Rect::new(300.0, 27.0, 50.0, 50.0), &targets, 5.0);
        assert_eq!(s.dy, -2.0);
        assert!(s.guides.iter().any(|g| !g.vertical && g.pos == 50.0));
        // Out of range: nothing.
        let s = snap_rect(&Rect::new(110.0, 300.0, 10.0, 10.0), &targets, 5.0);
        assert_eq!(s, Snap::default());
        // Threshold scales with zoom: 5 px at 400% = 1.25 units.
        let s = snap_rect(&Rect::new(102.0, 300.0, 10.0, 10.0), &targets, 5.0 / 4.0);
        assert_eq!(s.dx, 0.0);
        // Already aligned: guide without correction.
        let s = snap_rect(&Rect::new(100.0, 300.0, 10.0, 10.0), &targets, 5.0);
        assert_eq!(s.dx, 0.0);
        assert_eq!(s.guides.len(), 1);
    }

    #[test]
    fn resize_corners_edges_and_flip() {
        let b = bx(10.0, 20.0, 100.0, 50.0, 0.0);
        let m = ResizeMods::default();
        let r = resize(&b, Handle::BottomRight, Vec2::new(150.0, 100.0), m);
        assert_eq!(r, bx(10.0, 20.0, 140.0, 80.0, 0.0));
        let r = resize(&b, Handle::TopLeft, Vec2::new(0.0, 0.0), m);
        assert_eq!(r, bx(0.0, 0.0, 110.0, 70.0, 0.0));
        // Edge handles only change one axis.
        let r = resize(&b, Handle::Right, Vec2::new(60.0, 999.0), m);
        assert_eq!(r, bx(10.0, 20.0, 50.0, 50.0, 0.0));
        // Dragging the right edge past the left one flips.
        let r = resize(&b, Handle::Right, Vec2::new(0.0, 0.0), m);
        assert_eq!(r, bx(0.0, 20.0, 10.0, 50.0, 0.0));
    }

    #[test]
    fn resize_shift_keeps_ratio_and_alt_from_center() {
        let b = bx(0.0, 0.0, 100.0, 50.0, 0.0);
        let shift = ResizeMods {
            shift: true,
            alt: false,
        };
        let r = resize(&b, Handle::BottomRight, Vec2::new(200.0, 60.0), shift);
        assert!(close(r.w, 200.0) && close(r.h, 100.0) && r.x == 0.0 && r.y == 0.0);
        // Top-left with shift keeps the bottom-right corner.
        let r = resize(&b, Handle::TopLeft, Vec2::new(-100.0, 40.0), shift);
        assert!(
            close(r.x, -100.0) && close(r.y, -50.0) && close(r.w, 200.0) && close(r.h, 100.0),
            "{r:?}"
        );
        // Edge + shift grows the other axis around its center.
        let r = resize(&b, Handle::Right, Vec2::new(200.0, 0.0), shift);
        assert!(close(r.w, 200.0) && close(r.h, 100.0) && close(r.y, -25.0));
        let alt = ResizeMods {
            shift: false,
            alt: true,
        };
        let r = resize(&b, Handle::Right, Vec2::new(120.0, 0.0), alt);
        assert_eq!(r, bx(-20.0, 0.0, 140.0, 50.0, 0.0));
        let both = ResizeMods {
            shift: true,
            alt: true,
        };
        let r = resize(&b, Handle::BottomRight, Vec2::new(150.0, 0.0), both);
        assert!(
            close(r.w, 200.0) && close(r.h, 100.0) && close(r.x, -50.0) && close(r.y, -25.0),
            "{r:?}"
        );
    }

    #[test]
    fn resize_rotated_keeps_opposite_corner() {
        // 90° ccw: local x axis points up (screen), local y points right.
        let b = bx(100.0, 100.0, 40.0, 20.0, 90.0);
        let fixed = b.to_parent(Vec2::new(0.0, 0.0));
        let far = b.to_parent(Vec2::new(60.0, 30.0));
        let r = resize(&b, Handle::BottomRight, far, ResizeMods::default());
        assert!(close(r.w, 60.0) && close(r.h, 30.0), "{r:?}");
        let f2 = r.to_parent(Vec2::ZERO);
        assert!(close(f2.x, fixed.x) && close(f2.y, fixed.y));
        // Top-left handle on a rotated box keeps the bottom-right corner.
        let br = b.to_parent(Vec2::new(40.0, 20.0));
        let r = resize(
            &b,
            Handle::TopLeft,
            b.to_parent(Vec2::new(-10.0, -10.0)),
            ResizeMods::default(),
        );
        assert!(close(r.w, 50.0) && close(r.h, 30.0));
        let br2 = r.to_parent(Vec2::new(r.w, r.h));
        assert!(close(br2.x, br.x) && close(br2.y, br.y), "{br:?} {br2:?}");
    }

    #[test]
    fn rotation_around_center() {
        let b = bx(0.0, 0.0, 100.0, 50.0, 0.0);
        let c = b.center();
        // Pointer swept a quarter turn clockwise on screen (from right to below).
        let r = rotate_around_center(
            &b,
            c + Vec2::new(100.0, 0.0),
            c + Vec2::new(0.0, 100.0),
            false,
        );
        assert!(close(r.rotation, -90.0), "{r:?}");
        let c2 = r.center();
        assert!(close(c2.x, c.x) && close(c2.y, c.y));
        let r = rotate_around_center(
            &b,
            c + Vec2::new(100.0, 0.0),
            c + Vec2::new(100.0, 20.0),
            true,
        );
        assert!(close(r.rotation, -15.0), "{r:?}");
        assert_eq!(normalize_deg(190.0), -170.0);
        assert_eq!(normalize_deg(-180.0), 180.0);
    }

    #[test]
    fn pixel_rounding() {
        let r = round_box(bx(0.4, 0.6, 10.2, 9.7, 0.0));
        assert_eq!(r, bx(0.0, 1.0, 11.0, 9.0, 0.0));
        let rr = bx(0.4, 0.6, 10.2, 9.7, 30.0);
        assert_eq!(round_box(rr), rr);
    }
}
