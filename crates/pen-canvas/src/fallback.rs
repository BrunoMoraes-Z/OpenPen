//! CPU-raster fallback for content GPUI cannot draw (pure parts, no GPUI types).
//!
//! - [`analyze`] walks a chunk once per version and finds the *outermost* scopes that need
//!   `pen-raster`: layers with blend modes / layer blur / backdrop blur, opacity groups whose
//!   children overlap, clips with rounded / rotated / elliptical / path shapes whose content
//!   actually reaches outside the shape's safe area, and items with effects the GPU path
//!   approximates (blurred shadows or gradient/image paints on paths, rotated or
//!   gradient-painted text). Rounded clips whose overflowing children are plain quads are
//!   handled natively by *adopting* the clip's corner radii (no fallback).
//! - [`plan_region`] picks the zoom bucket (powers of √2) and the texture region (the whole
//!   scope when it fits the limits, otherwise the visible part plus a margin snapped to a grid).
//! - [`FallbackCache`] is an LRU (by frame) keyed by [`TexKey`] with a byte budget.
//! - [`RasterJob`] renders a scope with `pen-raster` (meant to run on a background thread).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use pen_render::{
    Affine2, BlendMode, Chunk, Color, DisplayItem, DisplayList, NodeKey, Paint, Rect, Shadow,
    Shape, Vec2,
};

use crate::geom;
use crate::order::{self, Segment, Unit};

/// How the texture composes with what the GPU already drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackKind {
    /// The scope rendered alone over transparent, drawn with normal alpha blending.
    Isolated,
    /// Needs the pixels below (blend mode / backdrop blur): everything painted before the scope
    /// end inside its bounds is rendered over the canvas background and drawn opaque.
    Backdrop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    BlendMode,
    BackdropBlur,
    LayerBlur,
    GroupOpacity,
    ClipShape,
    ShapeEffect,
    TextEffect,
}

/// An outermost scope (or single item) of a chunk drawn from a CPU texture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FallbackScope {
    /// Index of the push item (or of the item itself).
    pub start: u32,
    /// Index just past the matching pop (or the item).
    pub end: u32,
    pub kind: FallbackKind,
    pub reason: FallbackReason,
    /// Doc-space area the texture must cover.
    pub bounds: Rect,
}

/// A quad child of a rounded clip drawn natively as `rect` with these radii (doc space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipAdopt {
    pub item: u32,
    pub rect: Rect,
    pub radii: [f32; 4],
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkPlan {
    /// Sorted by `start`, non-overlapping.
    pub scopes: Vec<FallbackScope>,
    pub adopt: Vec<ClipAdopt>,
    /// Draw-order segments (used when the chunk is painted with layers).
    pub segments: Vec<Segment>,
    /// Segments valid while every text item is drawn as bars (fewer splits).
    pub segments_bars: Vec<Segment>,
    /// Largest text size in doc units (font size × transform scale): all text is drawn as
    /// bars below `zoom = text_bar_px / max_text_size`.
    pub max_text_size: f32,
}

const EPS: f32 = 1e-3;

fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = a.right().min(b.right());
    let btm = a.bottom().min(b.bottom());
    (r > x && btm > y).then(|| Rect::new(x, y, r - x, btm - y))
}

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(&b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn outset(r: &Rect, d: f32) -> Rect {
    Rect::new(r.x - d, r.y - d, r.w + 2.0 * d, r.h + 2.0 * d)
}

/// Point inside a rounded rect (radii TL, TR, BR, BL), boundary included.
pub fn in_rounded_rect(r: &Rect, radii: &[f32; 4], p: Vec2) -> bool {
    if p.x < r.x - EPS || p.x > r.right() + EPS || p.y < r.y - EPS || p.y > r.bottom() + EPS {
        return false;
    }
    let centers = [
        (
            Vec2::new(r.x + radii[0], r.y + radii[0]),
            radii[0],
            -1.0,
            -1.0,
        ),
        (
            Vec2::new(r.right() - radii[1], r.y + radii[1]),
            radii[1],
            1.0,
            -1.0,
        ),
        (
            Vec2::new(r.right() - radii[2], r.bottom() - radii[2]),
            radii[2],
            1.0,
            1.0,
        ),
        (
            Vec2::new(r.x + radii[3], r.bottom() - radii[3]),
            radii[3],
            -1.0,
            1.0,
        ),
    ];
    for (c, rad, sx, sy) in centers {
        if rad > 0.0 && (p.x - c.x) * sx > 0.0 && (p.y - c.y) * sy > 0.0 {
            return (p - c).length() <= rad + EPS;
        }
    }
    true
}

/// Clip geometry used for the "is this child entirely inside the shape" test.
enum ClipGeom {
    /// Axis-aligned zero-radius rect: natively exact.
    Native,
    /// Convex local shape under `inv` (doc → local).
    Convex {
        inv: Affine2,
        size: Vec2,
        /// `None` = ellipse.
        radii: Option<[f32; 4]>,
        /// Doc-space rounded rect when the clip is axis-aligned (enables adoption).
        doc: Option<(Rect, [f32; 4])>,
    },
    /// Arbitrary path: never provably safe.
    Path,
}

impl ClipGeom {
    fn new(t: &Affine2, shape: &Shape) -> ClipGeom {
        match shape {
            Shape::Rect { radii, .. } if radii.is_zero() && geom::is_axis_aligned(t) => {
                ClipGeom::Native
            }
            Shape::Path(_) => ClipGeom::Path,
            _ if t.matrix2.determinant().abs() < 1e-12 => ClipGeom::Path,
            Shape::Rect { size, radii } => ClipGeom::Convex {
                inv: t.inverse(),
                size: *size,
                radii: Some(geom::clamp_radii(size.x, size.y, radii.0)),
                doc: geom::quad_geom(t, shape),
            },
            Shape::Ellipse { size } => ClipGeom::Convex {
                inv: t.inverse(),
                size: *size,
                radii: None,
                doc: geom::quad_geom(t, shape),
            },
        }
    }

    /// All of `b` (doc space) lies inside the clip shape.
    fn contains(&self, b: &Rect) -> bool {
        match self {
            ClipGeom::Native => true,
            ClipGeom::Path => false,
            ClipGeom::Convex {
                inv, size, radii, ..
            } => {
                let local = Rect::new(0.0, 0.0, size.x, size.y);
                [
                    Vec2::new(b.x, b.y),
                    Vec2::new(b.right(), b.y),
                    Vec2::new(b.right(), b.bottom()),
                    Vec2::new(b.x, b.bottom()),
                ]
                .iter()
                .all(|p| {
                    let q = inv.transform_point2(*p);
                    match radii {
                        Some(r) => in_rounded_rect(&local, r, q),
                        None => {
                            let (rx, ry) = (size.x * 0.5, size.y * 0.5);
                            if rx <= 0.0 || ry <= 0.0 {
                                return false;
                            }
                            let d = Vec2::new((q.x - rx) / rx, (q.y - ry) / ry);
                            d.length_squared() <= 1.0 + 2.0 * EPS
                        }
                    }
                })
            }
        }
    }
}

/// Native drawing of a quad `k` (radii `rk`) clipped by the rounded rect `c` (radii `rc`), both
/// axis-aligned in doc space: the clipped rect and its radii, when the intersection is exactly a
/// rounded rect (each corner is either the clip's corner, the child's corner inside the clip,
/// or a square corner inside the clip).
pub fn adopt_clip(c: &Rect, rc: &[f32; 4], k: &Rect, rk: &[f32; 4]) -> Option<(Rect, [f32; 4])> {
    let q = intersect(c, k)?;
    let mut out = [0.0f32; 4];
    // Corner j: (sx, sy) = -1 for left/top, +1 for right/bottom. Mirror so "outward" is negative.
    let signs = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    for (j, &(sx, sy)) in signs.iter().enumerate() {
        let edge = |r: &Rect, s: f32, x: bool| -> f32 {
            match (x, s < 0.0) {
                (true, true) => -r.x,
                (true, false) => r.right(),
                (false, true) => -r.y,
                (false, false) => r.bottom(),
            }
        };
        // Outward coordinate (bigger = further out).
        let (kx, ky) = (edge(k, sx, true), edge(k, sy, false));
        let (cx, cy) = (edge(c, sx, true), edge(c, sy, false));
        let on_c_x = kx >= cx - EPS;
        let on_c_y = ky >= cy - EPS;
        let on_k_x = kx <= cx + EPS;
        let on_k_y = ky <= cy + EPS;
        let (rcj, rkj) = (rc[j], rk[j]);
        // Q's corner point in doc space.
        let qp = Vec2::new(
            if sx < 0.0 { q.x } else { q.right() },
            if sy < 0.0 { q.y } else { q.bottom() },
        );
        // The child's own rounded corner stays outside the clip along an axis.
        let k_arc_out_x = rkj <= EPS || kx - rkj >= cx - EPS;
        let k_arc_out_y = rkj <= EPS || ky - rkj >= cy - EPS;
        out[j] = if on_c_x && on_c_y {
            if on_k_x && on_k_y {
                rcj.max(rkj)
            } else if k_arc_out_x || k_arc_out_y {
                rcj
            } else {
                return None;
            }
        } else if on_k_x && on_k_y {
            if !in_rounded_rect(c, rc, qp) {
                return None;
            }
            rkj
        } else {
            // Mixed: one side from the clip, the other from the child.
            if !in_rounded_rect(c, rc, qp) {
                return None;
            }
            let ok = if on_c_x { k_arc_out_x } else { k_arc_out_y };
            if !ok {
                return None;
            }
            0.0
        };
    }
    // The radii must fit without clamping, or the GPU shape differs from the intersection.
    let fits = out[0] + out[1] <= q.w + EPS
        && out[3] + out[2] <= q.w + EPS
        && out[0] + out[3] <= q.h + EPS
        && out[1] + out[2] <= q.h + EPS;
    fits.then_some((q, out))
}

/// Analysis node: a drawable item or a scope.
struct Node {
    bounds: Option<Rect>,
    /// Would show internal overlap if drawn with a group alpha per primitive.
    multi: bool,
    /// Plain filled quad usable for clip adoption: (item, doc rect, radii).
    quad: Option<(u32, Rect, [f32; 4])>,
}

struct Cand {
    start: u32,
    end: u32,
    reason: FallbackReason,
    backdrop: bool,
    bounds: Rect,
}

struct Analyzer<'a> {
    items: &'a [DisplayItem],
    bounds: &'a [Option<Rect>],
    cands: Vec<Cand>,
    adopt: Vec<ClipAdopt>,
}

fn complex_paint(p: &Paint) -> bool {
    match p {
        Paint::Solid(_) => false,
        Paint::Linear { stops, .. } => stops.len() > 2,
        _ => true,
    }
}

/// Item-level reason to rasterize on the CPU (effects the GPU path approximates).
pub fn item_reason(item: &DisplayItem) -> Option<FallbackReason> {
    match item {
        DisplayItem::Shape {
            transform,
            shape,
            fills,
            stroke,
            shadows,
            ..
        } => {
            let quad = geom::quad_geom(transform, shape).is_some();
            let stroke_complex = stroke
                .as_ref()
                .is_some_and(|s| s.paints.iter().any(complex_paint));
            let shadow_complex = !quad
                && shadows.iter().any(|s| match s {
                    Shadow::Drop { blur, spread, .. } => *blur > 0.0 || *spread != 0.0,
                    Shadow::Inner { .. } => true,
                });
            let fill_complex = !quad && fills.iter().any(complex_paint);
            (stroke_complex || shadow_complex || fill_complex)
                .then_some(FallbackReason::ShapeEffect)
        }
        DisplayItem::Text {
            transform, block, ..
        } => {
            let m = transform.matrix2;
            let rotated = m.x_axis.y.abs() > 1e-4
                || m.y_axis.x.abs() > 1e-4
                || m.x_axis.x <= 0.0
                || m.y_axis.y <= 0.0;
            let painted = block
                .runs
                .iter()
                .any(|r| !matches!(r.style.paint, Paint::Solid(_)));
            (rotated || painted).then_some(FallbackReason::TextEffect)
        }
        _ => None,
    }
}

/// Children overlap pairwise (sweep over x).
fn any_overlap(children: &[Node]) -> bool {
    let mut b: Vec<Rect> = children.iter().filter_map(|c| c.bounds).collect();
    b.sort_by(|p, q| p.x.total_cmp(&q.x));
    for i in 0..b.len() {
        for j in i + 1..b.len() {
            if b[j].x >= b[i].right() {
                break;
            }
            if b[i].intersects(&b[j]) {
                return true;
            }
        }
    }
    false
}

impl Analyzer<'_> {
    /// Parse siblings starting at `*i` until the pop closing `closer` (or the end).
    fn seq(&mut self, i: &mut usize, closer: Option<bool>) -> Vec<Node> {
        let mut out = Vec::new();
        while *i < self.items.len() {
            let idx = *i;
            match &self.items[idx] {
                DisplayItem::PopClip | DisplayItem::PopLayer => {
                    *i += 1;
                    let is_clip = matches!(self.items[idx], DisplayItem::PopClip);
                    if closer == Some(is_clip) {
                        return out;
                    }
                }
                DisplayItem::PushClip { transform, shape } => {
                    *i += 1;
                    let n = self.clip(idx, i, transform, shape);
                    out.push(n);
                }
                DisplayItem::PushLayer {
                    opacity,
                    blend,
                    blur,
                    backdrop_blur,
                    bounds,
                } => {
                    *i += 1;
                    let n = self.layer(idx, i, *opacity, *blend, *blur, *backdrop_blur, bounds);
                    out.push(n);
                }
                item => {
                    *i += 1;
                    out.push(self.leaf(idx, item));
                }
            }
        }
        out
    }

    fn leaf(&mut self, idx: usize, item: &DisplayItem) -> Node {
        let bounds = self.bounds.get(idx).copied().flatten();
        if let (Some(reason), Some(b)) = (item_reason(item), bounds) {
            self.cands.push(Cand {
                start: idx as u32,
                end: idx as u32 + 1,
                reason,
                backdrop: false,
                bounds: b,
            });
        }
        let (multi, quad) = match item {
            DisplayItem::Shape {
                transform,
                shape,
                fills,
                stroke,
                shadows,
                ..
            } => {
                let layers =
                    fills.len() + stroke.as_ref().map_or(0, |s| s.paints.len()) + shadows.len();
                let plain = stroke.is_none()
                    && shadows.is_empty()
                    && fills.iter().all(|f| match f {
                        Paint::Image { fit, .. } => {
                            matches!(
                                fit,
                                pen_render::ImageFit::Cover | pen_render::ImageFit::Stretch
                            )
                        }
                        _ => true,
                    });
                let quad = plain
                    .then(|| geom::quad_geom(transform, shape))
                    .flatten()
                    .map(|(r, radii)| (idx as u32, r, radii));
                (layers > 1, quad)
            }
            _ => (false, None),
        };
        Node {
            bounds,
            multi,
            quad,
        }
    }

    fn clip(&mut self, start: usize, i: &mut usize, t: &Affine2, shape: &Shape) -> Node {
        let children = self.seq(i, Some(true));
        let end = *i;
        let clip_b = geom::shape_local_bounds(shape).transformed(t);
        let content = children.iter().fold(None, |a, c| union(a, c.bounds));
        let bounds = content.and_then(|c| intersect(&c, &clip_b));
        let multi = children.len() > 1 || children.iter().any(|c| c.multi);
        let node = Node {
            bounds,
            multi,
            quad: None,
        };
        let Some(vis) = bounds else {
            return node;
        };
        let g = ClipGeom::new(t, shape);
        let mut unsafe_child = false;
        let mut adopts = Vec::new();
        for c in &children {
            let Some(cb) = c.bounds else { continue };
            // The GPU clips to the shape's bounding box exactly, so only the part of the child
            // inside the box must lie inside the (convex) shape.
            let Some(inner) = intersect(&cb, &clip_b) else {
                continue;
            };
            if g.contains(&inner) {
                continue;
            }
            let adopted = match (&g, c.quad) {
                (
                    ClipGeom::Convex {
                        doc: Some((cr, crad)),
                        ..
                    },
                    Some((item, kr, krad)),
                ) => adopt_clip(cr, crad, &kr, &krad).map(|(rect, radii)| ClipAdopt {
                    item,
                    rect,
                    radii,
                }),
                _ => None,
            };
            match adopted {
                Some(a) => adopts.push(a),
                None => unsafe_child = true,
            }
        }
        if unsafe_child {
            self.cands.push(Cand {
                start: start as u32,
                end: end as u32,
                reason: FallbackReason::ClipShape,
                backdrop: false,
                bounds: vis,
            });
        }
        // Adoption also makes the approximate GPU drawing better while a texture is pending.
        self.adopt.extend(adopts);
        node
    }

    #[allow(clippy::too_many_arguments)]
    fn layer(
        &mut self,
        start: usize,
        i: &mut usize,
        opacity: f32,
        blend: BlendMode,
        blur: f32,
        backdrop_blur: f32,
        lb: &Rect,
    ) -> Node {
        let children = self.seq(i, Some(false));
        let end = *i;
        let content = children.iter().fold(None, |a, c| union(a, c.bounds));
        let has_lb = lb.w > 0.0 && lb.h > 0.0;
        let area = if has_lb {
            content.and_then(|c| intersect(&c, lb))
        } else {
            content
        };
        let margin = if blur > 0.0 { blur * 1.5 + 1.0 } else { 0.0 };
        let mut bounds = area.map(|a| outset(&a, margin));
        if backdrop_blur > 0.0 && has_lb {
            bounds = union(bounds, Some(*lb));
        }
        let multi = children.len() > 1 || children.iter().any(|c| c.multi);
        let node = Node {
            bounds,
            multi,
            quad: None,
        };
        let Some(b) = bounds else { return node };
        if opacity <= 1.0 / 512.0 {
            return node;
        }
        let reason = if blend != BlendMode::Normal {
            Some(FallbackReason::BlendMode)
        } else if backdrop_blur > 0.0 {
            Some(FallbackReason::BackdropBlur)
        } else if blur > 0.0 {
            Some(FallbackReason::LayerBlur)
        } else if opacity < 0.999 && (children.iter().any(|c| c.multi) || any_overlap(&children)) {
            Some(FallbackReason::GroupOpacity)
        } else {
            None
        };
        if let Some(reason) = reason {
            self.cands.push(Cand {
                start: start as u32,
                end: end as u32,
                reason,
                backdrop: matches!(
                    reason,
                    FallbackReason::BlendMode | FallbackReason::BackdropBlur
                ),
                bounds: b,
            });
        }
        node
    }
}

/// Fallback scopes, clip adoptions and draw-order segments of a chunk. `bounds[i]` are the
/// doc-space bounds of `items[i]` (`None` for scope items).
pub fn analyze(items: &[DisplayItem], bounds: &[Option<Rect>]) -> ChunkPlan {
    let mut a = Analyzer {
        items,
        bounds,
        cands: Vec::new(),
        adopt: Vec::new(),
    };
    let mut i = 0;
    a.seq(&mut i, None);
    let mut cands = a.cands;
    cands.sort_by(|p, q| p.start.cmp(&q.start).then(q.end.cmp(&p.end)));
    let mut scopes: Vec<FallbackScope> = Vec::new();
    for c in cands {
        if let Some(last) = scopes.last_mut()
            && c.start < last.end
        {
            // Nested inside an outer fallback: pen-raster renders it as part of the outer one,
            // but a blend / backdrop inside needs the outer one to see the backdrop too.
            if c.backdrop {
                last.kind = FallbackKind::Backdrop;
            }
            continue;
        }
        scopes.push(FallbackScope {
            start: c.start,
            end: c.end,
            kind: if c.backdrop {
                FallbackKind::Backdrop
            } else {
                FallbackKind::Isolated
            },
            reason: c.reason,
            bounds: c.bounds,
        });
    }
    let mut adopt = a.adopt;
    adopt.sort_by_key(|a| a.item);
    let segments = segments_for(items, bounds, &scopes, false);
    let segments_bars = segments_for(items, bounds, &scopes, true);
    let max_text_size = items
        .iter()
        .filter_map(|it| match it {
            DisplayItem::Text {
                transform, block, ..
            } => Some(
                block.runs.iter().map(|r| r.style.size).fold(0.0, f32::max)
                    * geom::transform_scale(transform),
            ),
            _ => None,
        })
        .fold(0.0, f32::max);
    ChunkPlan {
        scopes,
        adopt,
        segments,
        segments_bars,
        max_text_size,
    }
}

/// Paint units (items, with fallback scopes as single direct units) → segments.
fn segments_for(
    items: &[DisplayItem],
    bounds: &[Option<Rect>],
    scopes: &[FallbackScope],
    bars: bool,
) -> Vec<Segment> {
    let mut units = Vec::new();
    let mut si = 0;
    let mut i = 0;
    while i < items.len() {
        if si < scopes.len() && scopes[si].start as usize == i {
            let s = &scopes[si];
            // A single-item fallback draws either one sprite or the item itself: it can share
            // a layer. Multi-item scopes go through per-primitive ordering.
            let single = s.end == s.start + 1;
            let item = order::item_ranks(&items[i], bars).filter(|_| single);
            units.push(Unit {
                start: s.start,
                end: s.end,
                bounds: s.bounds,
                ranks: match item {
                    Some(r) => order::Ranks {
                        min: r.min,
                        max: order::R_POLY,
                        poly: true,
                        descending: r.descending,
                    },
                    None => order::Ranks {
                        min: order::R_SHADOW,
                        max: order::R_POLY,
                        poly: true,
                        descending: false,
                    },
                },
                direct: item.is_none(),
            });
            i = s.end as usize;
            si += 1;
            continue;
        }
        if let (Some(r), Some(b)) = (
            order::item_ranks(&items[i], bars),
            bounds.get(i).copied().flatten(),
        ) {
            units.push(Unit {
                start: i as u32,
                end: i as u32 + 1,
                bounds: b,
                ranks: r,
                direct: false,
            });
        }
        i += 1;
    }
    order::segment(items.len(), &units, 256)
}

// ---------------------------------------------------------------------------------------------
// Zoom buckets and texture regions

/// Bucket index for `device_scale` (device pixels per doc unit): powers of √2.
pub fn zoom_bucket(device_scale: f32) -> i16 {
    if !(device_scale.is_finite() && device_scale > 0.0) {
        return 0;
    }
    (2.0 * device_scale.log2()).round().clamp(-60.0, 60.0) as i16
}

pub fn bucket_scale(bucket: i16) -> f32 {
    2f32.powf(bucket as f32 * 0.5)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionLimits {
    pub max_side: u32,
    pub max_pixels: u64,
    /// Extra area around the viewport for partial regions (fraction of the view size).
    pub margin: f32,
    /// Partial regions snap to cells of this many device pixels.
    pub cell_px: f32,
}

impl Default for RegionLimits {
    fn default() -> Self {
        Self {
            max_side: 4096,
            max_pixels: 8 << 20,
            margin: 0.15,
            cell_px: 256.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionPlan {
    pub bucket: i16,
    pub scale: f32,
    /// Doc-space rect to render.
    pub doc: Rect,
    /// `None` = the whole scope; otherwise the snapped cell range.
    pub cells: Option<[i32; 4]>,
}

fn fits(r: &Rect, scale: f32, l: &RegionLimits) -> bool {
    let (w, h) = ((r.w * scale).ceil(), (r.h * scale).ceil());
    w <= l.max_side as f32 && h <= l.max_side as f32 && (w as u64) * (h as u64) <= l.max_pixels
}

/// Texture region for a scope with doc bounds `scope`, viewed through `view` (doc space).
pub fn plan_region(
    scope: &Rect,
    view: &Rect,
    device_scale: f32,
    l: &RegionLimits,
) -> Option<RegionPlan> {
    intersect(scope, view)?;
    let mut bucket = zoom_bucket(device_scale);
    for _ in 0..64 {
        let scale = bucket_scale(bucket);
        if fits(scope, scale, l) {
            return Some(RegionPlan {
                bucket,
                scale,
                doc: *scope,
                cells: None,
            });
        }
        let m = Rect::new(
            view.x - view.w * l.margin,
            view.y - view.h * l.margin,
            view.w * (1.0 + 2.0 * l.margin),
            view.h * (1.0 + 2.0 * l.margin),
        );
        if let Some(need) = intersect(scope, &m) {
            let cell = l.cell_px / scale;
            let c = [
                (need.x / cell).floor() as i32,
                (need.y / cell).floor() as i32,
                (need.right() / cell).ceil() as i32,
                (need.bottom() / cell).ceil() as i32,
            ];
            let snapped = Rect::new(
                c[0] as f32 * cell,
                c[1] as f32 * cell,
                (c[2] - c[0]) as f32 * cell,
                (c[3] - c[1]) as f32 * cell,
            );
            if let Some(doc) = intersect(&snapped, scope)
                && fits(&doc, scale, l)
            {
                return Some(RegionPlan {
                    bucket,
                    scale,
                    doc,
                    cells: Some(c),
                });
            }
        }
        bucket -= 1;
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Texture cache

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId {
    pub root: NodeKey,
    /// Generation of the chunk's prepared data in the canvas (unique per chunk allocation the
    /// canvas has seen; not the builder's `Chunk::version`, which only one builder keeps unique).
    pub version: u64,
    pub scope: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TexKey {
    pub id: ScopeId,
    pub bucket: i16,
    /// Group alpha baked into isolated textures (1/64 steps).
    pub alpha: u8,
    /// Hash of what lies below (backdrop textures) and the background.
    pub env: u64,
    pub cells: Option<[i32; 4]>,
}

struct Entry<T> {
    key: TexKey,
    doc_rect: Rect,
    image: T,
    bytes: usize,
    last_used: u64,
}

/// LRU texture cache with a byte budget. Entries used in the current frame are never evicted.
pub struct FallbackCache<T> {
    entries: HashMap<ScopeId, Vec<Entry<T>>>,
    pending: HashSet<TexKey>,
    evicted: Vec<T>,
    bytes: usize,
    pub budget: usize,
    frame: u64,
}

impl<T> Default for FallbackCache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            pending: HashSet::new(),
            evicted: Vec::new(),
            bytes: 0,
            budget: 192 << 20,
            frame: 0,
        }
    }
}

fn covers(a: &Rect, b: &Rect) -> bool {
    let e = 1e-3 * (a.w.max(a.h)).max(1.0);
    b.x >= a.x - e && b.y >= a.y - e && b.right() <= a.right() + e && b.bottom() <= a.bottom() + e
}

impl<T: Clone> FallbackCache<T> {
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn is_pending(&self, key: &TexKey) -> bool {
        self.pending.contains(key)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn mark_pending(&mut self, key: TexKey) {
        self.pending.insert(key);
    }

    pub fn cancel_pending(&mut self, key: &TexKey) {
        self.pending.remove(key);
    }

    /// Exact entry for `key` (marks it used).
    pub fn get(&mut self, key: &TexKey) -> Option<(Rect, T)> {
        let frame = self.frame;
        let e = self
            .entries
            .get_mut(&key.id)?
            .iter_mut()
            .find(|e| e.key == *key)?;
        e.last_used = frame;
        Some((e.doc_rect, e.image.clone()))
    }

    /// Best stand-in for `key` while it renders: same scope/alpha/env, covering `need`, with
    /// the closest bucket (ties prefer the sharper one).
    pub fn nearest(&mut self, key: &TexKey, need: &Rect) -> Option<(Rect, T)> {
        let frame = self.frame;
        let list = self.entries.get_mut(&key.id)?;
        let e = list
            .iter_mut()
            .filter(|e| {
                e.key.alpha == key.alpha && e.key.env == key.env && covers(&e.doc_rect, need)
            })
            .min_by_key(|e| {
                let d = (e.key.bucket as i32 - key.bucket as i32).abs() * 2;
                d + (e.key.bucket < key.bucket) as i32
            })?;
        e.last_used = frame;
        Some((e.doc_rect, e.image.clone()))
    }

    pub fn insert(&mut self, key: TexKey, doc_rect: Rect, image: T, bytes: usize) {
        self.pending.remove(&key);
        let list = self.entries.entry(key.id).or_default();
        if let Some(old) = list.iter().position(|e| e.key == key) {
            let e = list.swap_remove(old);
            self.bytes -= e.bytes;
            self.evicted.push(e.image);
        }
        list.push(Entry {
            key,
            doc_rect,
            image,
            bytes,
            last_used: self.frame,
        });
        self.bytes += bytes;
        self.evict_over_budget();
    }

    /// Drop least-recently-used entries until under budget (never ones used this frame).
    pub fn evict_over_budget(&mut self) {
        while self.bytes > self.budget {
            let victim = self
                .entries
                .iter()
                .flat_map(|(id, l)| {
                    l.iter()
                        .enumerate()
                        .map(move |(i, e)| (e.last_used, *id, i))
                })
                .filter(|(used, ..)| *used < self.frame)
                .min_by_key(|(used, ..)| *used);
            let Some((_, id, i)) = victim else { break };
            let list = self.entries.get_mut(&id).expect("victim list");
            let e = list.swap_remove(i);
            if list.is_empty() {
                self.entries.remove(&id);
            }
            self.bytes -= e.bytes;
            self.evicted.push(e.image);
        }
    }

    /// Remove entries whose `(root, version)` is no longer current.
    pub fn retain_versions(&mut self, mut live: impl FnMut(NodeKey, u64) -> bool) {
        let evicted = &mut self.evicted;
        let bytes = &mut self.bytes;
        self.entries.retain(|id, list| {
            let keep = live(id.root, id.version);
            if !keep {
                for e in list.drain(..) {
                    *bytes -= e.bytes;
                    evicted.push(e.image);
                }
            }
            keep
        });
        // Pending keys stay until their job reports back (`insert` / `cancel_pending`): the
        // jobs are still running, and dropping them here would let more than
        // `fallback_max_jobs` run at once. A stale result is rejected by the caller.
    }

    pub fn clear(&mut self) {
        self.retain_versions(|_, _| false);
    }

    /// Images removed since the last call (to release their GPU atlas tiles).
    pub fn take_evicted(&mut self) -> Vec<T> {
        std::mem::take(&mut self.evicted)
    }
}

// ---------------------------------------------------------------------------------------------
// Raster job

/// Everything needed to render one fallback texture off the UI thread.
#[derive(Clone)]
pub struct RasterJob {
    pub key: TexKey,
    pub list: Arc<DisplayList>,
    pub chunk: usize,
    pub start: usize,
    pub end: usize,
    pub kind: FallbackKind,
    pub region: Rect,
    pub scale: f32,
    pub alpha: f32,
    pub background: Color,
}

pub struct RasterOutput {
    pub key: TexKey,
    /// Doc rect covered by the pixels.
    pub doc_rect: Rect,
    pub width: u32,
    pub height: u32,
    /// Straight-alpha BGRA.
    pub bgra: Vec<u8>,
}

/// Pops closing the scopes still open after `items`.
fn closing_pops(items: &[DisplayItem]) -> Vec<DisplayItem> {
    let mut stack = Vec::new();
    for it in items {
        match it {
            DisplayItem::PushClip { .. } => stack.push(true),
            DisplayItem::PushLayer { .. } => stack.push(false),
            DisplayItem::PopClip | DisplayItem::PopLayer => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
        .into_iter()
        .rev()
        .map(|clip| {
            if clip {
                DisplayItem::PopClip
            } else {
                DisplayItem::PopLayer
            }
        })
        .collect()
}

/// Hash of the chunks painted below chunk `ci` inside `region` (backdrop texture validity).
pub fn backdrop_env(list: &DisplayList, ci: usize, region: &Rect, background: Color) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in &list.chunks[..ci.min(list.chunks.len())] {
        if c.bounds.intersects(region) {
            // The allocation tells replaced chunks apart even when another builder reused
            // the same `(root, version)`.
            (c.root, c.version, Arc::as_ptr(c) as usize).hash(&mut h);
        }
    }
    [background.r, background.g, background.b, background.a]
        .map(f32::to_bits)
        .hash(&mut h);
    h.finish() | 1
}

/// Premultiplied RGBA → straight BGRA with `alpha` applied.
pub fn premul_rgba_to_bgra(src: &[u8], alpha: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let k = alpha.clamp(0.0, 1.0);
    for px in src.as_chunks::<4>().0 {
        let a = px[3] as u32;
        let un = |c: u8| {
            (c as u32 * 255 + a / 2)
                .checked_div(a)
                .map_or(0, |v| v.min(255) as u8)
        };
        out.extend_from_slice(&[
            un(px[2]),
            un(px[1]),
            un(px[0]),
            (a as f32 * k).round() as u8,
        ]);
    }
    out
}

fn context_pool() -> &'static Mutex<Vec<pen_raster::RasterContext>> {
    static POOL: std::sync::OnceLock<Mutex<Vec<pen_raster::RasterContext>>> =
        std::sync::OnceLock::new();
    POOL.get_or_init(|| Mutex::new(Vec::new()))
}

impl RasterJob {
    /// The display list pen-raster renders for this job.
    pub fn build_list(&self) -> DisplayList {
        let Some(chunk) = self.list.chunks.get(self.chunk) else {
            return DisplayList::default();
        };
        let end = self.end.min(chunk.items.len());
        let start = self.start.min(end);
        match self.kind {
            FallbackKind::Isolated => DisplayList::from_chunks([Chunk {
                root: chunk.root,
                bounds: chunk.bounds,
                version: chunk.version,
                items: chunk.items[start..end].to_vec(),
            }]),
            FallbackKind::Backdrop => {
                // Chunks below are shared, not copied.
                let mut chunks: Vec<Arc<Chunk>> = self.list.chunks[..self.chunk]
                    .iter()
                    .filter(|c| c.bounds.intersects(&self.region))
                    .cloned()
                    .collect();
                let mut items = chunk.items[..end].to_vec();
                items.extend(closing_pops(&chunk.items[..end]));
                chunks.push(Arc::new(Chunk {
                    root: chunk.root,
                    bounds: chunk.bounds,
                    version: chunk.version,
                    items,
                }));
                DisplayList { chunks }
            }
        }
    }

    /// Render with a pooled pen-raster context (fonts load once per context).
    pub fn run(&self) -> Option<RasterOutput> {
        let list = self.build_list();
        let bg = match self.kind {
            FallbackKind::Isolated => Color::TRANSPARENT,
            FallbackKind::Backdrop => self.background,
        };
        // Align the origin to the device-pixel grid of the bucket.
        let s = self.scale;
        let x0 = (self.region.x * s).floor() / s;
        let y0 = (self.region.y * s).floor() / s;
        let region = Rect::new(x0, y0, self.region.right() - x0, self.region.bottom() - y0);
        let (w, h) = pen_raster::output_size(&region, s);
        if w == 0 || h == 0 {
            return None;
        }
        let mut ctx = context_pool()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .unwrap_or_default();
        let pixmap = ctx.render(&list, region, s, bg);
        context_pool()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ctx);
        let alpha = match self.kind {
            FallbackKind::Isolated => self.alpha,
            FallbackKind::Backdrop => 1.0,
        };
        Some(RasterOutput {
            key: self.key,
            doc_rect: Rect::new(x0, y0, w as f32 / s, h as f32 / s),
            width: w,
            height: h,
            bgra: premul_rgba_to_bgra(pixmap.data(), alpha),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pen_render::{CornerRadii, GradientStop, Path, PathEl, TextBlock};
    use smallvec::smallvec;

    fn rect_item(x: f32, y: f32, w: f32, h: f32, r: f32) -> DisplayItem {
        DisplayItem::Shape {
            node: NodeKey(1),
            transform: Affine2::from_translation(Vec2::new(x, y)),
            shape: Shape::Rect {
                size: Vec2::new(w, h),
                radii: CornerRadii::uniform(r),
            },
            fills: smallvec![Paint::Solid(Color::BLACK)],
            stroke: None,
            shadows: smallvec![],
        }
    }

    fn clip(x: f32, y: f32, w: f32, h: f32, r: f32) -> DisplayItem {
        DisplayItem::PushClip {
            transform: Affine2::from_translation(Vec2::new(x, y)),
            shape: Shape::Rect {
                size: Vec2::new(w, h),
                radii: CornerRadii::uniform(r),
            },
        }
    }

    fn layer(opacity: f32, blend: BlendMode, blur: f32, backdrop: f32) -> DisplayItem {
        DisplayItem::PushLayer {
            opacity,
            blend,
            blur,
            backdrop_blur: backdrop,
            bounds: Rect::default(),
        }
    }

    fn plan(items: &[DisplayItem]) -> ChunkPlan {
        let b: Vec<Option<Rect>> = items.iter().map(geom::item_doc_bounds).collect();
        analyze(items, &b)
    }

    #[test]
    fn plain_content_needs_no_fallback() {
        let items = vec![
            rect_item(0.0, 0.0, 100.0, 100.0, 8.0),
            clip(0.0, 0.0, 100.0, 100.0, 0.0),
            rect_item(-10.0, -10.0, 200.0, 50.0, 0.0),
            DisplayItem::PopClip,
            // opacity over non-overlapping children is exact per item
            layer(0.5, BlendMode::Normal, 0.0, 0.0),
            rect_item(0.0, 200.0, 10.0, 10.0, 0.0),
            rect_item(20.0, 200.0, 10.0, 10.0, 0.0),
            DisplayItem::PopLayer,
        ];
        let p = plan(&items);
        assert!(p.scopes.is_empty(), "{:?}", p.scopes);
    }

    #[test]
    fn detects_layer_effects_outermost_only() {
        let items = vec![
            rect_item(0.0, 0.0, 50.0, 50.0, 0.0),
            layer(1.0, BlendMode::Normal, 4.0, 0.0), // 1: blur
            rect_item(0.0, 0.0, 50.0, 50.0, 0.0),
            layer(1.0, BlendMode::Multiply, 0.0, 0.0), // nested blend → outer becomes backdrop
            rect_item(10.0, 10.0, 50.0, 50.0, 0.0),
            DisplayItem::PopLayer,
            DisplayItem::PopLayer,                   // 6
            layer(0.5, BlendMode::Normal, 0.0, 0.0), // 7: overlapping children
            rect_item(0.0, 100.0, 50.0, 50.0, 0.0),
            rect_item(25.0, 100.0, 50.0, 50.0, 0.0),
            DisplayItem::PopLayer,
            layer(1.0, BlendMode::Normal, 0.0, 6.0), // 11: backdrop blur
            rect_item(0.0, 200.0, 50.0, 50.0, 0.0),
            DisplayItem::PopLayer,
        ];
        let p = plan(&items);
        let got: Vec<_> = p
            .scopes
            .iter()
            .map(|s| (s.start, s.end, s.kind, s.reason))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, 7, FallbackKind::Backdrop, FallbackReason::LayerBlur),
                (7, 11, FallbackKind::Isolated, FallbackReason::GroupOpacity),
                (11, 14, FallbackKind::Backdrop, FallbackReason::BackdropBlur),
            ]
        );
        // blur margin is included in the texture bounds
        assert!(p.scopes[0].bounds.x < -5.0 && p.scopes[0].bounds.right() > 65.0);
        // fallback scopes are direct segments
        assert!(p.segments.contains(&Segment::Direct { start: 1, end: 14 }));
    }

    #[test]
    fn text_bars_regime_needs_fewer_layers() {
        let label = DisplayItem::Text {
            node: NodeKey(1),
            transform: Affine2::from_scale(Vec2::splat(2.0)),
            block: Arc::new(TextBlock {
                text: "label".into(),
                runs: smallvec![pen_render::TextRun {
                    range: 0..5,
                    style: pen_render::TextStyle {
                        family: "x".into(),
                        size: 10.0,
                        weight: 400,
                        italic: false,
                        letter_spacing: 0.0,
                        underline: false,
                        strikethrough: false,
                        paint: Paint::Solid(Color::BLACK),
                    },
                }],
                lines: vec![],
                align: Default::default(),
                size: Vec2::new(50.0, 12.0),
                clip: false,
            }),
        };
        // a rect drawn over the label: glyphs (sprites) would end up above it in one layer
        let items = vec![label, rect_item(0.0, 0.0, 100.0, 30.0, 0.0)];
        let p = plan(&items);
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.segments_bars.len(), 1);
        assert_eq!(p.max_text_size, 20.0);
    }

    #[test]
    fn group_opacity_counts_self_overlap() {
        let mut r = rect_item(0.0, 0.0, 50.0, 50.0, 0.0);
        if let DisplayItem::Shape { stroke, .. } = &mut r {
            *stroke = Some(pen_render::Stroke {
                paints: smallvec![Paint::Solid(Color::WHITE)],
                widths: [2.0; 4],
                align: pen_render::StrokeAlign::Center,
                cap: Default::default(),
                join: Default::default(),
                miter_limit: 4.0,
                dash: smallvec![],
            });
        }
        let items = vec![
            layer(0.5, BlendMode::Normal, 0.0, 0.0),
            r,
            DisplayItem::PopLayer,
        ];
        assert_eq!(plan(&items).scopes.len(), 1);
    }

    #[test]
    fn rounded_clip_safe_adopted_or_fallback() {
        // child fully inside the safe area
        let inside = vec![
            clip(0.0, 0.0, 100.0, 100.0, 10.0),
            rect_item(5.0, 20.0, 90.0, 60.0, 0.0),
            DisplayItem::PopClip,
        ];
        let p = plan(&inside);
        assert!(p.scopes.is_empty() && p.adopt.is_empty());
        // a shadowed rect crossing only the straight bottom edge: the bbox mask clips it exactly
        let mut shadowed = rect_item(40.0, 85.0, 20.0, 30.0, 4.0);
        if let DisplayItem::Shape { shadows, .. } = &mut shadowed {
            shadows.push(Shadow::Drop {
                offset: Vec2::new(0.0, 2.0),
                blur: 6.0,
                spread: 0.0,
                color: Color::BLACK,
            });
        }
        let p = plan(&[
            clip(0.0, 0.0, 100.0, 100.0, 10.0),
            shadowed,
            DisplayItem::PopClip,
        ]);
        assert!(p.scopes.is_empty() && p.adopt.is_empty());
        // ... but crossing near a rounded corner it is not
        let corner = rect_item(-5.0, 85.0, 30.0, 30.0, 0.0);
        let mut c2 = corner.clone();
        if let DisplayItem::Shape { shadows, .. } = &mut c2 {
            shadows.push(Shadow::Drop {
                offset: Vec2::ZERO,
                blur: 2.0,
                spread: 0.0,
                color: Color::BLACK,
            });
        }
        assert_eq!(
            plan(&[clip(0.0, 0.0, 100.0, 100.0, 10.0), c2, DisplayItem::PopClip])
                .scopes
                .len(),
            1
        );
        // full-bleed header: adopted with the clip's top radii
        let header = vec![
            clip(0.0, 0.0, 100.0, 100.0, 10.0),
            rect_item(-5.0, -5.0, 110.0, 40.0, 0.0),
            DisplayItem::PopClip,
        ];
        let p = plan(&header);
        assert!(p.scopes.is_empty());
        assert_eq!(
            p.adopt,
            vec![ClipAdopt {
                item: 1,
                rect: Rect::new(0.0, 0.0, 100.0, 35.0),
                radii: [10.0, 10.0, 0.0, 0.0],
            }]
        );
        // a path crossing the corner can't be adopted → fallback
        let star = Arc::new(Path {
            els: vec![
                PathEl::MoveTo(Vec2::ZERO),
                PathEl::LineTo(Vec2::new(30.0, 0.0)),
                PathEl::LineTo(Vec2::new(0.0, 30.0)),
                PathEl::Close,
            ],
            fill_rule: Default::default(),
        });
        let mut path_item = rect_item(-5.0, -5.0, 1.0, 1.0, 0.0);
        if let DisplayItem::Shape { shape, .. } = &mut path_item {
            *shape = Shape::Path(star);
        }
        let p = plan(&[
            clip(0.0, 0.0, 100.0, 100.0, 10.0),
            path_item,
            DisplayItem::PopClip,
        ]);
        assert_eq!(p.scopes.len(), 1);
        assert_eq!(p.scopes[0].reason, FallbackReason::ClipShape);
        assert_eq!(p.scopes[0].bounds, Rect::new(0.0, 0.0, 25.0, 25.0));
        // rotated clip with content poking out
        let rot = vec![
            DisplayItem::PushClip {
                transform: Affine2::from_angle(0.4),
                shape: Shape::Rect {
                    size: Vec2::new(100.0, 100.0),
                    radii: CornerRadii::default(),
                },
            },
            rect_item(0.0, 0.0, 100.0, 100.0, 0.0),
            DisplayItem::PopClip,
        ];
        assert_eq!(plan(&rot).scopes.len(), 1);
        // circle avatar: full-size image rect adopts radius w/2
        let avatar = vec![
            DisplayItem::PushClip {
                transform: Affine2::IDENTITY,
                shape: Shape::Ellipse {
                    size: Vec2::new(40.0, 40.0),
                },
            },
            rect_item(0.0, 0.0, 40.0, 40.0, 0.0),
            DisplayItem::PopClip,
        ];
        let p = plan(&avatar);
        assert!(p.scopes.is_empty());
        assert_eq!(p.adopt[0].radii, [20.0; 4]);
    }

    #[test]
    fn adoption_rules() {
        let c = Rect::new(0.0, 0.0, 100.0, 100.0);
        let rc = [10.0; 4];
        // thin strip at the top: the arc would be cut → not representable
        assert!(adopt_clip(&c, &rc, &Rect::new(0.0, 0.0, 100.0, 5.0), &[0.0; 4]).is_none());
        // strip starting inside the corner square on the left
        assert!(adopt_clip(&c, &rc, &Rect::new(-5.0, 3.0, 50.0, 50.0), &[0.0; 4]).is_none());
        // strip below the corner arc: square left corners on the clip edge
        let (q, r) = adopt_clip(&c, &rc, &Rect::new(-5.0, 20.0, 50.0, 50.0), &[0.0; 4]).unwrap();
        assert_eq!((q, r), (Rect::new(0.0, 20.0, 45.0, 50.0), [0.0; 4]));
        // same rect with its own bigger radius: max of both
        let (_, r) = adopt_clip(&c, &rc, &c, &[20.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(r, [20.0, 10.0, 10.0, 10.0]);
        // child with a rounded corner poking past the clip edge partially
        assert!(adopt_clip(&c, &rc, &Rect::new(-2.0, 30.0, 50.0, 40.0), &[8.0; 4]).is_none());
    }

    #[test]
    fn item_effects() {
        let mut star = rect_item(0.0, 0.0, 10.0, 10.0, 0.0);
        if let DisplayItem::Shape {
            transform, fills, ..
        } = &mut star
        {
            *transform = Affine2::from_angle(0.5);
            fills[0] = Paint::Radial {
                center: Vec2::ZERO,
                radius: Vec2::ONE,
                stops: smallvec![GradientStop {
                    offset: 0.0,
                    color: Color::BLACK
                }],
            };
        }
        assert_eq!(item_reason(&star), Some(FallbackReason::ShapeEffect));
        // the same radial on an axis-aligned rect is a native texture
        let mut flat = star.clone();
        if let DisplayItem::Shape { transform, .. } = &mut flat {
            *transform = Affine2::IDENTITY;
        }
        assert_eq!(item_reason(&flat), None);
        let text = DisplayItem::Text {
            node: NodeKey(1),
            transform: Affine2::from_angle(0.2),
            block: Arc::new(TextBlock {
                text: "a".into(),
                runs: smallvec![],
                lines: vec![],
                align: Default::default(),
                size: Vec2::new(10.0, 10.0),
                clip: false,
            }),
        };
        assert_eq!(item_reason(&text), Some(FallbackReason::TextEffect));
    }

    #[test]
    fn buckets_and_regions() {
        assert_eq!(zoom_bucket(1.0), 0);
        assert_eq!(zoom_bucket(2.0), 2);
        assert_eq!(zoom_bucket(std::f32::consts::SQRT_2), 1);
        assert_eq!(zoom_bucket(1.1), 0);
        assert_eq!(zoom_bucket(0.5), -2);
        assert!((bucket_scale(3) - 2.0f32.powf(1.5)).abs() < 1e-5);
        let l = RegionLimits::default();
        let view = Rect::new(0.0, 0.0, 1000.0, 800.0);
        // small scope: whole texture, key independent of the view
        let s = Rect::new(100.0, 100.0, 200.0, 150.0);
        let p = plan_region(&s, &view, 1.0, &l).unwrap();
        assert_eq!((p.cells, p.doc, p.bucket), (None, s, 0));
        assert!(plan_region(&s, &Rect::new(5000.0, 0.0, 10.0, 10.0), 1.0, &l).is_none());
        // huge scope at high zoom: partial region, snapped, stable under small pans
        let big = Rect::new(0.0, 0.0, 100_000.0, 100_000.0);
        let v1 = Rect::new(510.0, 510.0, 400.0, 300.0);
        let p1 = plan_region(&big, &v1, 4.0, &l).unwrap();
        let p2 = plan_region(&big, &Rect::new(513.0, 511.0, 400.0, 300.0), 4.0, &l).unwrap();
        assert!(p1.cells.is_some());
        assert_eq!(p1.cells, p2.cells);
        assert!(covers(&p1.doc, &v1));
        assert!(fits(&p1.doc, p1.scale, &l));
        // far too many pixels even for the view: bucket goes down until it fits
        let tiny = RegionLimits {
            max_side: 64,
            max_pixels: 64 * 64,
            ..l
        };
        let p = plan_region(&s, &view, 4.0, &tiny).unwrap();
        assert!(p.bucket < zoom_bucket(4.0) && fits(&p.doc, p.scale, &tiny));
    }

    fn key(scope: u32, bucket: i16) -> TexKey {
        TexKey {
            id: ScopeId {
                root: NodeKey(1),
                version: 1,
                scope,
            },
            bucket,
            alpha: 64,
            env: 0,
            cells: None,
        }
    }

    #[test]
    fn cache_lookup_nearest_and_lru() {
        let mut c: FallbackCache<u32> = FallbackCache {
            budget: 300,
            ..Default::default()
        };
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        c.begin_frame();
        c.mark_pending(key(0, 0));
        c.insert(key(0, 0), r, 1, 100);
        assert!(!c.is_pending(&key(0, 0)));
        c.insert(key(0, 4), r, 2, 100);
        assert_eq!(c.get(&key(0, 0)), Some((r, 1)));
        assert_eq!(c.get(&key(0, 1)), None);
        // nearest: bucket 1 is closer to 0; bucket 3 closer to 4
        assert_eq!(c.nearest(&key(0, 1), &r).map(|e| e.1), Some(1));
        assert_eq!(c.nearest(&key(0, 3), &r).map(|e| e.1), Some(2));
        // ties (bucket 2: distance 2 to both) prefer the sharper texture
        assert_eq!(c.nearest(&key(0, 2), &r).map(|e| e.1), Some(2));
        // not covering the needed rect → no stand-in
        assert_eq!(c.nearest(&key(0, 2), &Rect::new(0.0, 0.0, 20.0, 5.0)), None);
        // LRU: touch entry 1 in a later frame, then overflow → entry 2 is evicted
        c.begin_frame();
        c.get(&key(0, 0));
        c.begin_frame();
        c.insert(key(1, 0), r, 3, 100);
        c.insert(key(2, 0), r, 4, 100);
        assert_eq!(c.bytes(), 300);
        assert_eq!(c.take_evicted(), vec![2]);
        // everything used this frame stays even over budget
        c.get(&key(1, 0));
        c.get(&key(2, 0));
        c.get(&key(0, 0));
        c.budget = 50;
        c.evict_over_budget();
        assert_eq!(c.len(), 3);
        c.begin_frame();
        c.evict_over_budget();
        assert!(c.is_empty() && c.bytes() == 0);
        // stale versions are purged
        c.budget = 1000;
        c.insert(key(0, 0), r, 9, 10);
        c.retain_versions(|_, v| v != 1);
        assert!(c.is_empty() && c.take_evicted().contains(&9));
    }

    #[test]
    fn jobs_build_lists_and_render() {
        let items = vec![
            rect_item(0.0, 0.0, 100.0, 100.0, 0.0),
            clip(0.0, 0.0, 100.0, 100.0, 0.0),
            layer(1.0, BlendMode::Multiply, 0.0, 0.0),
            rect_item(10.0, 10.0, 20.0, 20.0, 0.0),
            DisplayItem::PopLayer,
            rect_item(50.0, 50.0, 20.0, 20.0, 0.0),
            DisplayItem::PopClip,
        ];
        let below = Chunk {
            root: NodeKey(7),
            bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
            version: 3,
            items: vec![rect_item(0.0, 0.0, 10.0, 10.0, 0.0)],
        };
        let far = Chunk {
            root: NodeKey(8),
            bounds: Rect::new(900.0, 0.0, 10.0, 10.0),
            version: 1,
            items: vec![],
        };
        let list = Arc::new(DisplayList::from_chunks([
            below,
            far,
            Chunk {
                root: NodeKey(9),
                bounds: Rect::new(0.0, 0.0, 100.0, 100.0),
                version: 1,
                items,
            },
        ]));
        let mut job = RasterJob {
            key: key(2, 0),
            list: list.clone(),
            chunk: 2,
            start: 2,
            end: 5,
            kind: FallbackKind::Backdrop,
            region: Rect::new(8.0, 8.0, 20.0, 20.0),
            scale: 1.5,
            alpha: 1.0,
            background: Color::WHITE,
        };
        let l = job.build_list();
        assert_eq!(l.chunks.len(), 2); // `far` is outside the region
        let own = &l.chunks[1].items;
        assert_eq!(own.len(), 6); // prefix 0..5 + the PopClip closing the open clip
        assert_eq!(own[5], DisplayItem::PopClip);
        let out = job.run().unwrap();
        assert_eq!((out.width, out.height), (30, 30));
        assert_eq!(out.bgra.len(), 30 * 30 * 4);
        // multiply of black over white background stays black, opaque
        assert_eq!(&out.bgra[..4], &[0, 0, 0, 255]);
        job.kind = FallbackKind::Isolated;
        job.alpha = 0.5;
        let l = job.build_list();
        assert_eq!(l.chunks.len(), 1);
        assert_eq!(l.chunks[0].items.len(), 3);
        let out = job.run().unwrap();
        // doc (15, 15) → pixel (10, 10)
        assert_eq!(out.bgra[(10 * 30 + 10) * 4 + 3], 128);
        // backdrop env changes with what is below
        let e1 = backdrop_env(&list, 2, &Rect::new(0.0, 0.0, 50.0, 50.0), Color::WHITE);
        let e2 = backdrop_env(&list, 2, &Rect::new(500.0, 500.0, 5.0, 5.0), Color::WHITE);
        assert_ne!(e1, e2);
    }
}
