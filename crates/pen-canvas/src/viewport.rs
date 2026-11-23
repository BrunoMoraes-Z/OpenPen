//! Viewport math (pan/zoom), independent of GPUI so it can be unit-tested.
//!
//! "Screen" coordinates here are logical pixels relative to the canvas element's top-left
//! corner. `DesignCanvas` converts window coordinates by subtracting the element origin.

use pen_render::{Affine2, Rect, Vec2};

pub const MIN_ZOOM: f32 = 0.02;
pub const MAX_ZOOM: f32 = 256.0;

/// `screen = doc * zoom + offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Screen position of the document origin.
    pub offset: Vec2,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            offset: Vec2::ZERO,
            zoom: 1.0,
        }
    }
}

impl Viewport {
    pub fn new(offset: Vec2, zoom: f32) -> Self {
        Self {
            offset,
            zoom: clamp_zoom(zoom),
        }
    }

    pub fn doc_to_screen(&self, p: Vec2) -> Vec2 {
        p * self.zoom + self.offset
    }

    pub fn screen_to_doc(&self, p: Vec2) -> Vec2 {
        (p - self.offset) / self.zoom
    }

    pub fn doc_rect_to_screen(&self, r: &Rect) -> Rect {
        let o = self.doc_to_screen(Vec2::new(r.x, r.y));
        Rect::new(o.x, o.y, r.w * self.zoom, r.h * self.zoom)
    }

    /// Document-space rectangle covered by a canvas of `size` screen pixels.
    pub fn visible_doc_rect(&self, size: Vec2) -> Rect {
        let o = self.screen_to_doc(Vec2::ZERO);
        Rect::new(o.x, o.y, size.x / self.zoom, size.y / self.zoom)
    }

    /// Document → screen affine (uniform scale + translation).
    pub fn transform(&self) -> Affine2 {
        Affine2::from_scale_angle_translation(Vec2::splat(self.zoom), 0.0, self.offset)
    }

    pub fn pan_by(&mut self, delta: Vec2) {
        self.offset += delta;
    }

    /// Set the zoom keeping the document point under `anchor` (screen) fixed.
    pub fn zoom_at(&mut self, anchor: Vec2, new_zoom: f32) {
        let new_zoom = clamp_zoom(new_zoom);
        let doc = self.screen_to_doc(anchor);
        self.zoom = new_zoom;
        self.offset = anchor - doc * new_zoom;
    }

    /// Multiply the zoom by `factor` around `anchor`.
    pub fn zoom_by(&mut self, anchor: Vec2, factor: f32) {
        self.zoom_at(anchor, self.zoom * factor);
    }

    /// Viewport that fits `content` inside a canvas of `size`, centered, leaving `padding`
    /// screen pixels on each side. Zoom is clamped to `[MIN_ZOOM, max_zoom]`.
    pub fn fit(content: &Rect, size: Vec2, padding: f32, max_zoom: f32) -> Viewport {
        let avail = (size - Vec2::splat(2.0 * padding)).max(Vec2::splat(1.0));
        let cw = content.w.max(1e-3);
        let ch = content.h.max(1e-3);
        let zoom = (avail.x / cw)
            .min(avail.y / ch)
            .clamp(MIN_ZOOM, max_zoom.min(MAX_ZOOM));
        let center = Vec2::new(content.x + content.w * 0.5, content.y + content.h * 0.5);
        Viewport {
            offset: size * 0.5 - center * zoom,
            zoom,
        }
    }
}

pub fn clamp_zoom(z: f32) -> f32 {
    if z.is_finite() {
        z.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        1.0
    }
}

/// Zoom factor for a wheel/trackpad scroll with the zoom modifier held.
/// `dy` is the vertical delta (positive = scroll up = zoom in). `precise` = pixel deltas
/// (trackpad / precision touchpad); otherwise `dy` is in wheel lines.
pub fn wheel_zoom_factor(dy: f32, precise: bool) -> f32 {
    let k = if precise { 0.01 } else { 0.18 };
    (dy * k).clamp(-2.0, 2.0).exp()
}

/// Next zoom preset step (used by zoom in/out commands).
pub fn step_zoom(zoom: f32, zoom_in: bool) -> f32 {
    const STEPS: [f32; 17] = [
        0.02, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0,
        256.0,
    ];
    if zoom_in {
        STEPS
            .iter()
            .copied()
            .find(|s| *s > zoom * 1.001)
            .unwrap_or(MAX_ZOOM)
    } else {
        STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < zoom / 1.001)
            .unwrap_or(MIN_ZOOM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).length() < 1e-3
    }

    #[test]
    fn screen_doc_roundtrip() {
        let v = Viewport::new(Vec2::new(120.0, -40.0), 2.5);
        let p = Vec2::new(33.0, 71.5);
        assert!(close(v.screen_to_doc(v.doc_to_screen(p)), p));
        assert!(close(v.doc_to_screen(Vec2::ZERO), Vec2::new(120.0, -40.0)));
        assert!(close(v.transform().transform_point2(p), v.doc_to_screen(p)));
    }

    #[test]
    fn zoom_to_cursor_keeps_anchor() {
        let mut v = Viewport::new(Vec2::new(10.0, 20.0), 1.0);
        let anchor = Vec2::new(400.0, 300.0);
        let before = v.screen_to_doc(anchor);
        for f in [1.7, 0.3, 12.0, 0.01, 1000.0] {
            v.zoom_by(anchor, f);
            assert!(close(v.screen_to_doc(anchor), before), "factor {f}");
            assert!((MIN_ZOOM..=MAX_ZOOM).contains(&v.zoom));
        }
    }

    #[test]
    fn zoom_is_clamped() {
        let mut v = Viewport::default();
        v.zoom_at(Vec2::ZERO, 1e9);
        assert_eq!(v.zoom, MAX_ZOOM);
        v.zoom_at(Vec2::ZERO, 0.0);
        assert_eq!(v.zoom, MIN_ZOOM);
        v.zoom_at(Vec2::ZERO, f32::NAN);
        assert_eq!(v.zoom, 1.0);
    }

    #[test]
    fn fit_centers_and_contains() {
        let content = Rect::new(100.0, 50.0, 2000.0, 1000.0);
        let size = Vec2::new(1000.0, 800.0);
        let v = Viewport::fit(&content, size, 50.0, MAX_ZOOM);
        assert!((v.zoom - 0.45).abs() < 1e-5);
        let s = v.doc_rect_to_screen(&content);
        assert!(s.x >= 49.9 && s.right() <= 950.1);
        assert!(s.y >= 49.9 && s.bottom() <= 750.1);
        let c = Vec2::new(s.x + s.w / 2.0, s.y + s.h / 2.0);
        assert!(close(c, size / 2.0));
        // Tiny content is capped by max_zoom.
        let v = Viewport::fit(&Rect::new(0.0, 0.0, 1.0, 1.0), size, 0.0, 1.0);
        assert_eq!(v.zoom, 1.0);
    }

    #[test]
    fn visible_rect_matches_corners() {
        let v = Viewport::new(Vec2::new(-200.0, 100.0), 4.0);
        let size = Vec2::new(800.0, 600.0);
        let r = v.visible_doc_rect(size);
        assert!(close(v.doc_to_screen(Vec2::new(r.x, r.y)), Vec2::ZERO));
        assert!(close(
            v.doc_to_screen(Vec2::new(r.right(), r.bottom())),
            size
        ));
    }

    #[test]
    fn wheel_and_steps() {
        assert!(wheel_zoom_factor(1.0, false) > 1.0);
        assert!(wheel_zoom_factor(-1.0, false) < 1.0);
        assert!(
            (wheel_zoom_factor(10.0, true) * wheel_zoom_factor(-10.0, true) - 1.0).abs() < 1e-5
        );
        assert_eq!(step_zoom(1.0, true), 1.5);
        assert_eq!(step_zoom(1.0, false), 0.75);
        assert_eq!(step_zoom(256.0, true), MAX_ZOOM);
        assert_eq!(step_zoom(0.02, false), MIN_ZOOM);
    }
}
