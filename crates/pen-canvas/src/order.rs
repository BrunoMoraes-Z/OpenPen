//! Draw-order segmentation (pure, no GPUI).
//!
//! GPUI orders primitives with a `BoundsTree` (one insert per primitive, ~1.5 µs). Inside a
//! `paint_layer` every primitive shares the layer's order and the renderer then draws by
//! primitive kind: shadows < quads < paths < monochrome sprites (glyphs) < polychrome sprites
//! (images), and sprites of one kind are sorted by atlas texture. Painting a whole chunk in one
//! layer is therefore only correct when no primitive is covered by an *earlier* primitive of a
//! *later-drawn* kind.
//!
//! [`segment`] splits a chunk's paint sequence into runs that are safe to paint as one layer
//! (checked with doc-space bounds, independent of zoom) and "direct" ranges that go through the
//! per-primitive `BoundsTree` (items whose own primitives are emitted out of kind order, and
//! CPU-raster fallback scopes).

use pen_render::{DisplayItem, Paint, Rect, Shadow};

use crate::geom;

pub const R_SHADOW: u8 = 0;
pub const R_QUAD: u8 = 1;
pub const R_PATH: u8 = 2;
pub const R_MONO: u8 = 3;
pub const R_POLY: u8 = 4;

/// Primitive kinds an item may emit (over every zoom / LOD), as GPUI draw ranks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ranks {
    pub min: u8,
    pub max: u8,
    /// Emits polychrome sprites (images / gradient textures): sorted by texture inside a layer.
    pub poly: bool,
    /// Its own primitives are emitted with a decreasing rank (e.g. fill texture then a quad
    /// stroke, or an inner shadow after the fill): it can never share a layer with itself.
    pub descending: bool,
}

/// True for transforms without rotation/skew (axis scale, flips allowed).
fn diagonal(t: &pen_render::Affine2) -> bool {
    t.matrix2.x_axis.y.abs() < 1e-6 && t.matrix2.y_axis.x.abs() < 1e-6
}

/// Conservative primitive ranks of a drawable item; `None` for scope items. With
/// `text_as_bars`, text is known to be drawn as gray bars (quads) only (low zoom).
pub fn item_ranks(item: &DisplayItem, text_as_bars: bool) -> Option<Ranks> {
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
            let mut seq: smallvec::SmallVec<[u8; 8]> = smallvec::SmallVec::new();
            for sh in shadows {
                if let Shadow::Drop { blur, spread, .. } = sh {
                    if quad {
                        seq.push(R_SHADOW);
                    } else if *blur <= 0.0 && *spread == 0.0 {
                        seq.push(R_PATH);
                    }
                }
            }
            for f in fills {
                seq.push(if !quad {
                    R_PATH
                } else {
                    match f {
                        Paint::Solid(_) => R_QUAD,
                        Paint::Linear { stops, .. } if stops.len() <= 2 => R_QUAD,
                        _ => R_POLY,
                    }
                });
            }
            if quad && shadows.iter().any(|s| matches!(s, Shadow::Inner { .. })) {
                seq.push(R_SHADOW);
            }
            if let Some(st) = stroke
                && !st.paints.is_empty()
            {
                let solid = st.paints.iter().all(|p| matches!(p, Paint::Solid(_)));
                let dashed = st.dash.iter().any(|d| *d > 0.0);
                seq.push(if quad && diagonal(transform) && solid && !dashed {
                    R_QUAD
                } else {
                    R_PATH
                });
            }
            let descending = seq.windows(2).any(|w| w[1] < w[0]);
            // Tiny non-rect shapes are drawn as a single quad (LOD alternative).
            if !quad || seq.is_empty() {
                seq.push(R_QUAD);
            }
            Some(Ranks {
                min: *seq.iter().min().unwrap_or(&R_QUAD),
                max: *seq.iter().max().unwrap_or(&R_QUAD),
                poly: seq.contains(&R_POLY),
                descending,
            })
        }
        // Glyphs (mono sprites); bars and decorations are quads. Decorations after glyphs share
        // the glyph color, so their inverted order is invisible.
        DisplayItem::Text { .. } => Some(Ranks {
            min: R_QUAD,
            max: if text_as_bars { R_QUAD } else { R_MONO },
            poly: false,
            descending: false,
        }),
        _ => None,
    }
}

/// A paint unit: one drawable item, or a whole fallback scope (`direct`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Unit {
    pub start: u32,
    pub end: u32,
    pub bounds: Rect,
    pub ranks: Ranks,
    pub direct: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    /// Items `start..end` painted inside one `paint_layer(bounds)` (doc-space bounds).
    Layer { start: u32, end: u32, bounds: Rect },
    /// Items `start..end` painted with GPUI's per-primitive ordering.
    Direct { start: u32, end: u32 },
}

impl Segment {
    pub fn range(&self) -> std::ops::Range<usize> {
        match *self {
            Segment::Layer { start, end, .. } | Segment::Direct { start, end } => {
                start as usize..end as usize
            }
        }
    }
}

/// Split `n_items` items (with drawable `units` in paint order) into contiguous segments.
/// A run is closed before a unit that overlaps an earlier unit of the run drawing a later kind
/// (or, for images, another image), and after `max_run` units (bounds the O(run²) checks).
pub fn segment(n_items: usize, units: &[Unit], max_run: usize) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut run: Vec<(Rect, u8, bool)> = Vec::new();
    let mut run_bounds: Option<Rect> = None;
    let mut start = 0u32;
    let close = |out: &mut Vec<Segment>,
                 run: &mut Vec<(Rect, u8, bool)>,
                 rb: &mut Option<Rect>,
                 start: &mut u32,
                 end: u32| {
        if let Some(bounds) = rb.take() {
            out.push(Segment::Layer {
                start: *start,
                end,
                bounds,
            });
            *start = end;
        }
        run.clear();
    };
    for u in units {
        if u.direct || u.ranks.descending {
            close(&mut out, &mut run, &mut run_bounds, &mut start, u.start);
            match out.last_mut() {
                Some(Segment::Direct { end, .. }) if *end == start => *end = u.end,
                _ => out.push(Segment::Direct { start, end: u.end }),
            }
            start = u.end;
            continue;
        }
        let conflict = run.len() >= max_run
            || run.iter().any(|(b, max, poly)| {
                (*max > u.ranks.min || (*poly && u.ranks.poly)) && b.intersects(&u.bounds)
            });
        if conflict {
            close(&mut out, &mut run, &mut run_bounds, &mut start, u.start);
        }
        run.push((u.bounds, u.ranks.max, u.ranks.poly));
        run_bounds = Some(match run_bounds {
            Some(b) => b.union(&u.bounds),
            None => u.bounds,
        });
    }
    let n = n_items as u32;
    if run_bounds.is_some() {
        close(&mut out, &mut run, &mut run_bounds, &mut start, n);
    }
    // Trailing scope items (pops) join the last segment.
    if start < n {
        match out.last_mut() {
            Some(Segment::Layer { end, .. } | Segment::Direct { end, .. }) => *end = n,
            None => out.push(Segment::Direct { start, end: n }),
        }
    }
    out
}

/// Reference check used by tests: given the per-item primitive ranks actually emitted in paint
/// order, simulate GPUI's in-layer kind ordering and report whether any overlapping pair is
/// drawn in the wrong order.
#[cfg(test)]
pub fn simulate_misordered(
    segments: &[Segment],
    prims: &[(usize, u8, Rect)], // (item index, rank, bounds) in paint order
) -> bool {
    // Assign each primitive a (segment order, kind, seq) key the way GPUI would draw it.
    let seg_of = |item: usize| {
        segments
            .iter()
            .position(|s| s.range().contains(&item))
            .unwrap_or(usize::MAX)
    };
    let mut keyed: Vec<(usize, usize, u8, usize)> = Vec::new(); // (paint idx, seg, rank, sub)
    for (pi, (item, rank, _)) in prims.iter().enumerate() {
        let s = seg_of(*item);
        let layered = matches!(segments.get(s), Some(Segment::Layer { .. }));
        // Direct primitives are ordered exactly (unique sub-order per primitive).
        let (r, sub) = if layered { (*rank, 0) } else { (0, pi) };
        keyed.push((pi, s, r, sub));
    }
    let mut drawn = keyed.clone();
    drawn.sort_by_key(|&(pi, s, r, sub)| (s, r, sub, pi));
    let pos: Vec<usize> = {
        let mut p = vec![0; prims.len()];
        for (k, (pi, ..)) in drawn.iter().enumerate() {
            p[*pi] = k;
        }
        p
    };
    for a in 0..prims.len() {
        for b in a + 1..prims.len() {
            if prims[a].2.intersects(&prims[b].2) && pos[a] > pos[b] {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use pen_render::{Affine2, Color, CornerRadii, NodeKey, Shape, Vec2};
    use smallvec::smallvec;

    fn unit(i: u32, b: Rect, min: u8, max: u8) -> Unit {
        Unit {
            start: i,
            end: i + 1,
            bounds: b,
            ranks: Ranks {
                min,
                max,
                poly: max == R_POLY,
                descending: false,
            },
            direct: false,
        }
    }

    fn prims_of(units: &[Unit]) -> Vec<(usize, u8, Rect)> {
        // Each unit emits its min then max rank (worst case for ordering).
        let mut v = Vec::new();
        for u in units {
            v.push((u.start as usize, u.ranks.min, u.bounds));
            if u.ranks.max != u.ranks.min {
                v.push((u.start as usize, u.ranks.max, u.bounds));
            }
        }
        v
    }

    #[test]
    fn rect_over_text_splits_layer() {
        let a = Rect::new(0.0, 0.0, 100.0, 20.0);
        // text, then an opaque rect covering it: one layer would draw the quad below glyphs.
        let units = [
            unit(0, a, R_QUAD, R_MONO),
            unit(1, a, R_QUAD, R_QUAD),
            unit(2, Rect::new(500.0, 0.0, 10.0, 10.0), R_QUAD, R_QUAD),
        ];
        let segs = segment(3, &units, 256);
        assert_eq!(segs.len(), 2);
        assert!(!simulate_misordered(&segs, &prims_of(&units)));
        // A single layer would be wrong.
        let one = [Segment::Layer {
            start: 0,
            end: 3,
            bounds: a,
        }];
        assert!(simulate_misordered(&one, &prims_of(&units)));
    }

    #[test]
    fn disjoint_or_ascending_content_stays_in_one_layer() {
        let units: Vec<Unit> = (0..40)
            .map(|i| {
                let b = Rect::new((i % 8) as f32 * 50.0, (i / 8) as f32 * 50.0, 40.0, 40.0);
                // card grid: alternating quads and text, never overlapping out of order
                if i % 2 == 0 {
                    unit(i, b, R_QUAD, R_QUAD)
                } else {
                    unit(i, b, R_QUAD, R_MONO)
                }
            })
            .collect();
        let segs = segment(40, &units, 256);
        assert_eq!(segs.len(), 1);
        // background quad then text over it: ascending, fine in one layer
        let bg = Rect::new(0.0, 0.0, 100.0, 100.0);
        let units = [
            unit(0, bg, R_QUAD, R_QUAD),
            unit(1, bg, R_PATH, R_PATH),
            unit(2, bg, R_QUAD, R_MONO),
        ];
        // path then text (min quad < path max) overlapping → must split once
        let segs = segment(3, &units, 256);
        assert!(!simulate_misordered(&segs, &prims_of(&units)));
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn overlapping_images_and_direct_units() {
        let b = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut units = vec![
            unit(0, b, R_POLY, R_POLY),
            unit(1, b, R_POLY, R_POLY),
            unit(3, b, R_QUAD, R_QUAD),
        ];
        // a direct fallback scope covering items 3..6
        units[2].end = 6;
        units[2].direct = true;
        let segs = segment(7, &units, 256);
        assert_eq!(
            segs,
            vec![
                Segment::Layer {
                    start: 0,
                    end: 1,
                    bounds: b
                },
                Segment::Layer {
                    start: 1,
                    end: 3,
                    bounds: b
                },
                Segment::Direct { start: 3, end: 7 },
            ]
        );
        // contiguous cover
        let mut next = 0;
        for s in &segs {
            assert_eq!(s.range().start, next);
            next = s.range().end;
        }
        assert_eq!(next, 7);
    }

    #[test]
    fn randomized_segments_never_misorder() {
        // xorshift for determinism without extra deps
        let mut s = 0x9e3779b97f4a7c15u64;
        let mut rnd = |m: u32| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % m as u64) as u32
        };
        for _ in 0..200 {
            let n = 1 + rnd(60) as usize;
            let units: Vec<Unit> = (0..n as u32)
                .map(|i| {
                    let b = Rect::new(
                        rnd(200) as f32,
                        rnd(200) as f32,
                        1.0 + rnd(80) as f32,
                        1.0 + rnd(80) as f32,
                    );
                    let a = rnd(5) as u8;
                    let c = rnd(5) as u8;
                    unit(i, b, a.min(c), a.max(c))
                })
                .collect();
            let segs = segment(n, &units, 1 + rnd(20) as usize);
            assert!(!simulate_misordered(&segs, &prims_of(&units)));
        }
    }

    #[test]
    fn item_ranks_classify() {
        let rect = DisplayItem::Shape {
            node: NodeKey(1),
            transform: Affine2::IDENTITY,
            shape: Shape::Rect {
                size: Vec2::new(10.0, 10.0),
                radii: CornerRadii::default(),
            },
            fills: smallvec![Paint::Solid(Color::BLACK)],
            stroke: None,
            shadows: smallvec![Shadow::Inner {
                offset: Vec2::ZERO,
                blur: 2.0,
                spread: 0.0,
                color: Color::BLACK
            }],
        };
        let r = item_ranks(&rect, false).unwrap();
        // fill quad, then inner shadow (drawn below quads inside a layer)
        assert!(r.descending && r.min == R_SHADOW && r.max == R_QUAD);
        let DisplayItem::Shape { shape, .. } = &rect else {
            unreachable!()
        };
        let star = DisplayItem::Shape {
            node: NodeKey(2),
            transform: Affine2::from_angle(0.3),
            shape: shape.clone(),
            fills: smallvec![Paint::Solid(Color::BLACK)],
            stroke: None,
            shadows: smallvec![],
        };
        let r = item_ranks(&star, false).unwrap();
        assert_eq!((r.min, r.max, r.descending), (R_QUAD, R_PATH, false));
        assert!(item_ranks(&DisplayItem::PopClip, false).is_none());
    }
}
