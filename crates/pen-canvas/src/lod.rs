//! Culling and level-of-detail decisions (pure, no GPUI).

use pen_render::{DisplayList, Rect};

/// Tunables for culling / LOD. Screen sizes are logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodPolicy {
    /// Items whose largest screen dimension is below this are skipped.
    pub min_item_px: f32,
    /// Text whose screen font size is below this renders as gray bars.
    pub text_bar_px: f32,
    /// Shadows are skipped below this zoom.
    pub shadow_min_zoom: f32,
    /// Shadows whose screen blur+spread+offset is below this are skipped.
    pub min_shadow_px: f32,
    /// Non-rect shapes (paths, rotated rects, ellipses) smaller than this on screen are drawn
    /// as a coverage-weighted quad instead of a tessellated path.
    pub path_as_quad_px: f32,
    /// Below this zoom chunks are painted as a few GPUI layers (draw-order segments, see
    /// `order.rs`): primitives skip GPUI's per-primitive overlap ordering (much cheaper), and
    /// the segments are split wherever primitive kinds overlap out of paint order, so the
    /// result stays exact.
    pub flatten_below_zoom: f32,
    /// CPU-raster fallback scopes smaller than this on screen are drawn with the GPU
    /// approximation (no texture).
    pub fallback_min_px: f32,
}

impl Default for LodPolicy {
    fn default() -> Self {
        Self {
            min_item_px: 0.5,
            text_bar_px: 4.0,
            shadow_min_zoom: 0.25,
            min_shadow_px: 0.5,
            path_as_quad_px: 3.0,
            flatten_below_zoom: 0.5,
            fallback_min_px: 6.0,
        }
    }
}

impl LodPolicy {
    /// Largest screen dimension of a doc rect at `zoom` is below the threshold.
    pub fn too_small(&self, doc: &Rect, zoom: f32) -> bool {
        doc.w.max(doc.h) * zoom < self.min_item_px
    }

    pub fn text_as_bars(&self, screen_font_px: f32) -> bool {
        screen_font_px < self.text_bar_px
    }

    /// Draw a non-rect shape whose doc bounds are `doc` as a flat quad.
    pub fn path_as_quad(&self, doc: &Rect, zoom: f32) -> bool {
        doc.w.max(doc.h) * zoom < self.path_as_quad_px
    }

    pub fn flatten_chunks(&self, zoom: f32) -> bool {
        zoom < self.flatten_below_zoom
    }

    pub fn draw_shadows(&self, zoom: f32) -> bool {
        zoom >= self.shadow_min_zoom
    }
}

/// Indices of chunks to draw: intersect `view` (doc space) and are not sub-pixel.
pub fn visible_chunks(
    list: &DisplayList,
    view: &Rect,
    zoom: f32,
    policy: &LodPolicy,
    out: &mut Vec<usize>,
) {
    out.clear();
    for (i, c) in list.chunks.iter().enumerate() {
        if c.bounds.intersects(view) && !policy.too_small(&c.bounds, zoom) {
            out.push(i);
        }
    }
}

/// `a` contains `b` entirely.
pub fn contains(a: &Rect, b: &Rect) -> bool {
    b.x >= a.x && b.y >= a.y && b.right() <= a.right() && b.bottom() <= a.bottom()
}

/// Pixel grid line positions: first doc-integer coordinate ≥ `start` and the count up to `end`.
pub fn grid_lines(start: f32, end: f32) -> (i64, i64) {
    let first = start.ceil() as i64;
    let last = end.floor() as i64;
    (first, (last - first + 1).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pen_render::{Chunk, NodeKey};

    fn chunk(i: u32, r: Rect) -> Chunk {
        Chunk {
            root: NodeKey(i),
            bounds: r,
            version: 0,
            items: vec![],
        }
    }

    #[test]
    fn culls_offscreen_and_tiny() {
        let list = DisplayList::from_chunks([
            chunk(0, Rect::new(0.0, 0.0, 100.0, 100.0)),
            chunk(1, Rect::new(5000.0, 0.0, 100.0, 100.0)),
            chunk(2, Rect::new(10.0, 10.0, 0.1, 0.1)),
            chunk(3, Rect::new(-50.0, -50.0, 60.0, 60.0)),
        ]);
        let policy = LodPolicy::default();
        let mut out = Vec::new();
        visible_chunks(
            &list,
            &Rect::new(0.0, 0.0, 1000.0, 1000.0),
            1.0,
            &policy,
            &mut out,
        );
        assert_eq!(out, vec![0, 3]);
        // Zoomed in far enough the tiny chunk becomes visible.
        visible_chunks(
            &list,
            &Rect::new(0.0, 0.0, 50.0, 50.0),
            20.0,
            &policy,
            &mut out,
        );
        assert_eq!(out, vec![0, 2, 3]);
    }

    #[test]
    fn lod_thresholds() {
        let p = LodPolicy::default();
        assert!(p.too_small(&Rect::new(0.0, 0.0, 4.0, 1.0), 0.1));
        assert!(!p.too_small(&Rect::new(0.0, 0.0, 4.0, 1.0), 0.2));
        assert!(p.text_as_bars(14.0 * 0.25));
        assert!(!p.text_as_bars(14.0 * 0.5));
        assert!(!p.draw_shadows(0.1));
        assert!(p.path_as_quad(&Rect::new(0.0, 0.0, 20.0, 20.0), 0.1));
        assert!(!p.path_as_quad(&Rect::new(0.0, 0.0, 20.0, 20.0), 1.0));
        assert!(p.draw_shadows(1.0));
        assert!(p.flatten_chunks(0.1) && !p.flatten_chunks(1.0));
    }

    #[test]
    fn grid_line_range() {
        assert_eq!(grid_lines(0.5, 3.2), (1, 3));
        assert_eq!(grid_lines(-1.0, 1.0), (-1, 3));
        assert_eq!(grid_lines(2.2, 2.8), (3, 0));
        assert!(contains(
            &Rect::new(0.0, 0.0, 10.0, 10.0),
            &Rect::new(1.0, 1.0, 2.0, 2.0)
        ));
    }
}
