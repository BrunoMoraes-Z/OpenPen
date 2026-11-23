//! Draws the agent flashes and cursors in the canvas overlay (GPUI).
//!
//! [`collect`] samples the pure timing state of [`AgentFx`] at `now` while
//! the document is borrowed; [`paint`] draws the result in screen space.

use std::time::Instant;

use gpui_kit::*;
use pen_render::Vec2;

use super::fx::AgentFx;
use crate::geom::Obb;
use crate::scene_document::SceneDocument;

/// One flash at `now`.
pub struct Ring {
    pub obb: Obb,
    pub color: u32,
    pub alpha: f32,
    /// Largest corner radius of the node (world units).
    pub radius: f32,
    /// Sweep line position (0 = top, 1 = bottom).
    pub sweep: Option<f32>,
}

/// One cursor at `now`.
pub struct Cursor {
    /// World position of the arrow tip.
    pub at: Vec2,
    /// Idle wobble (screen px).
    pub wobble: Vec2,
    pub name: SharedString,
    pub color: u32,
    pub opacity: f32,
}

/// What to draw at `now`; nodes are looked up again by id so a flash
/// follows layout changes.
pub fn collect(
    fx: &AgentFx,
    doc: &SceneDocument,
    zoom: f32,
    now: Instant,
) -> (Vec<Ring>, Vec<Cursor>) {
    let rings = fx
        .flashes
        .iter()
        .filter_map(|f| {
            let alpha = f.alpha(now);
            if alpha <= 0.0 {
                return None;
            }
            let key = doc.scene.find(&f.id);
            let obb = key
                .and_then(|k| doc.world_obb(k))
                .unwrap_or_else(|| Obb::from_rect(f.rect));
            let radius = key
                .and_then(|k| doc.get(k))
                .map(|n| n.corner_radius.iter().copied().fold(0.0, f32::max))
                .unwrap_or(0.0);
            let screen_h = obb.aabb().h * zoom;
            Some(Ring {
                sweep: f.sweep(now, screen_h),
                obb,
                radius,
                color: f.color,
                alpha,
            })
        })
        .collect();
    let cursors = fx
        .cursors
        .iter()
        .filter_map(|c| {
            let opacity = c.opacity(now);
            (opacity > 0.0).then(|| Cursor {
                at: c.position(now),
                wobble: c.wobble(now),
                name: c.name.clone().into(),
                color: c.color,
                opacity,
            })
        })
        .collect();
    (rings, cursors)
}

fn hsla(c: u32) -> Hsla {
    rgb(c).into()
}

/// Arrow glyph, tip at the origin (screen px).
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 17.0),
    (4.2, 13.1),
    (7.2, 19.6),
    (10.1, 18.3),
    (7.2, 12.0),
    (12.6, 12.0),
];

fn arrow_path(tip: Point<Pixels>, mut pb: PathBuilder) -> Option<Path<Pixels>> {
    for (i, (x, y)) in ARROW.iter().enumerate() {
        let p = point(tip.x + px(*x), tip.y + px(*y));
        if i == 0 {
            pb.move_to(p);
        } else {
            pb.line_to(p);
        }
    }
    pb.close();
    pb.build().ok()
}

pub fn paint(
    window: &mut Window,
    cx: &mut App,
    rings: &[Ring],
    cursors: &[Cursor],
    to_s: &dyn Fn(Vec2) -> Point<Pixels>,
    zoom: f32,
    family: &'static str,
) {
    for r in rings {
        let a = r.obb.aabb();
        let p0 = to_s(Vec2::new(a.x, a.y));
        let p1 = to_s(Vec2::new(a.right(), a.bottom()));
        let node = Bounds::from_corners(point(p0.x, p0.y), point(p1.x, p1.y));
        let color = hsla(r.color);
        // The ring settles onto the node during the attack and lifts off
        // during the release.
        let grow = px(2.0 + 6.0 * (1.0 - r.alpha));
        let ring = Bounds::from_corners(
            point(node.origin.x - grow, node.origin.y - grow),
            point(
                node.origin.x + node.size.width + grow,
                node.origin.y + node.size.height + grow,
            ),
        );
        // Follow the node's rounded corners (capped to half its size).
        let max_r = node.size.width.min(node.size.height) / 2.;
        let radius = (px(r.radius * zoom).min(max_r) + grow).max(px(4.));
        window.paint_quad(quad(
            ring,
            radius,
            color.opacity(0.07 * r.alpha),
            px(2.),
            color.opacity(r.alpha),
            BorderStyle::Solid,
        ));
        if let Some(s) = r.sweep {
            let h = node.size.height;
            let y = node.origin.y + h * s;
            let trail = (h * 0.35).min(px(28.));
            window.with_content_mask(Some(ContentMask { bounds: node }), |window| {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(node.origin.x, y - trail),
                        size: size(node.size.width, trail),
                    },
                    linear_gradient(
                        180.,
                        linear_color_stop(color.opacity(0.0), 0.),
                        linear_color_stop(color.opacity(0.28 * r.alpha), 1.),
                    ),
                ));
                window.paint_quad(fill(
                    Bounds {
                        origin: point(node.origin.x, y - px(1.)),
                        size: size(node.size.width, px(2.)),
                    },
                    color.opacity(0.9 * r.alpha),
                ));
            });
        }
    }
    for c in cursors {
        let base = to_s(c.at);
        let tip = point(base.x + px(c.wobble.x), base.y + px(c.wobble.y));
        let color = hsla(c.color);
        let op = c.opacity;
        if let Some(p) = arrow_path(point(tip.x + px(1.), tip.y + px(1.5)), PathBuilder::fill()) {
            window.paint_path(p, hsla(0x000000).opacity(0.25 * op));
        }
        if let Some(p) = arrow_path(tip, PathBuilder::stroke(px(3.))) {
            window.paint_path(p, hsla(0xffffff).opacity(op));
        }
        if let Some(p) = arrow_path(tip, PathBuilder::fill()) {
            window.paint_path(p, color.opacity(op));
        }
        let mut f = font(family);
        f.weight = FontWeight::MEDIUM;
        let run = TextRun {
            len: c.name.len(),
            font: f,
            color: hsla(0xffffff).opacity(op),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(c.name.clone(), px(11.), &[run], None);
        let lb = Bounds {
            origin: point(tip.x + px(12.), tip.y + px(18.)),
            size: size(line.width + px(12.), px(18.)),
        };
        window.paint_quad(quad(
            lb,
            px(9.),
            color.opacity(op),
            px(1.),
            hsla(0xffffff).opacity(0.6 * op),
            BorderStyle::Solid,
        ));
        let _ = line.paint(
            point(lb.origin.x + px(6.), lb.origin.y + px(2.)),
            px(14.),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}
