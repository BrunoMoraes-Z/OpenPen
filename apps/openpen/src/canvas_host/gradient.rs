//! On-canvas handles of the gradient open in the inspector's fill popover
//! (docs/fill-editing.md): start/end points (linear), center / radius /
//! second radius (radial, angular; as in Pen, dragging the center keeps the
//! radius handle in place and turns the ellipse) and the color stops, all
//! draggable with
//! live preview. Each drag is one undo step (`begin_gesture` /
//! `end_gesture`); every move applies absolute values through the
//! incremental update path. The geometry lives in `inspector::fill`.

use gpui_kit::*;
use pen_doc::NodeKey;
use pen_render::Vec2;

use super::{CanvasHost, rgb_u};
use crate::geom::Obb;
use crate::inspector::fill::{self, FillEditTarget, GradHandle, GradKind, GradientInfo, PaintOp};
use crate::inspector::model::PropEdit;
use crate::scene_document::key_of;

/// Screen sizes (px).
const KNOB: f32 = 10.0;
const STOP: f32 = 16.0;
const STOP_SELECTED: f32 = 20.0;
/// Stop markers sit this far beside the gradient line (Pen offsets them so
/// they do not cover the end points).
const STOP_OFFSET: f32 = 18.0;
const PICK: f32 = 7.0;
const LINE_PICK: f32 = 5.0;
const SNAP: f32 = 6.0;
const DRAG_START: f32 = 3.0;
const BLUE: u32 = 0x3d99ff;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Handle(GradHandle),
    Stop(usize),
}

/// A drag of a gradient handle or stop.
pub(super) struct GradDrag {
    target: Target,
    fe: FillEditTarget,
    /// Node box and gradient when the drag started.
    obb: Obb,
    g0: GradientInfo,
    start: Point<Pixels>,
    moved: bool,
}

/// What lies under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hit {
    Target(Target),
    /// On the gradient line, away from the stops: a click adds a stop at `t`.
    Line(f32),
}

fn v(p: [f32; 2]) -> Vec2 {
    Vec2::new(p[0], p[1])
}

impl CanvasHost {
    /// The gradient being edited: one selected node whose fill popover has
    /// a gradient open.
    fn gradient_edit(&self, cx: &App) -> Option<(NodeKey, FillEditTarget, GradientInfo, Obb)> {
        let s = self.state.read(cx);
        let fe = s.fill_edit?;
        let [id] = s.selection.as_slice() else {
            return None;
        };
        let props = crate::inspector::model::PropertySource::props(&s.doc, *id)?;
        let list = if fe.stroke {
            &props.strokes
        } else {
            &props.fills
        };
        let g = list.get(fe.index)?.gradient.clone()?;
        let obb = s.doc.world_obb(key_of(*id))?;
        Some((key_of(*id), fe, g, obb))
    }

    /// True while gradient handles replace the selection handles.
    pub(super) fn gradient_active(&self, cx: &App) -> bool {
        self.gradient_edit(cx).is_some()
    }

    /// Screen point of a node-local point.
    fn local_to_screen(&self, obb: &Obb, p: [f32; 2], cx: &App) -> Vec2 {
        self.to_screen(obb.t.transform_point2(v(p)), cx)
    }

    /// Screen positions of the stop markers (offset beside the line).
    fn stop_markers(g: &GradientInfo, obb: &Obb, to_s: &dyn Fn(Vec2) -> Vec2) -> Vec<(Vec2, Vec2)> {
        let hp = fill::handle_points(g, obb.w, obb.h);
        let s0 = to_s(obb.t.transform_point2(v(hp.start)));
        let s1 = to_s(obb.t.transform_point2(v(hp.end)));
        let line = s1 - s0;
        let side = if line.length() > 1e-3 {
            let n = line / line.length();
            Vec2::new(-n.y, n.x)
        } else {
            Vec2::new(0.0, 1.0)
        };
        g.stops
            .iter()
            .map(|st| {
                let a =
                    to_s(
                        obb.t
                            .transform_point2(v(fill::stop_point(g, st.position, obb.w, obb.h))),
                    );
                let off = match g.kind {
                    // Outward from the center.
                    GradKind::Angular => {
                        let d = a - s0;
                        if d.length() > 1e-3 {
                            d / d.length()
                        } else {
                            side
                        }
                    }
                    _ => side,
                };
                (a, a + off * STOP_OFFSET)
            })
            .collect()
    }

    fn gradient_hit(&self, sp: Vec2, cx: &App) -> Option<Hit> {
        let (_, fe, g, obb) = self.gradient_edit(cx)?;
        let to_s = |p: Vec2| self.to_screen(p, cx);
        // Stops first (the selected one is on top).
        let markers = Self::stop_markers(&g, &obb, &to_s);
        let mut best: Option<(f32, usize)> = None;
        for (i, (_, m)) in markers.iter().enumerate() {
            let r = if fe.stop == Some(i) {
                STOP_SELECTED
            } else {
                STOP
            } / 2.0
                + 2.0;
            let d = m.distance(sp);
            if d <= r && best.is_none_or(|b| d < b.0 || fe.stop == Some(i)) {
                best = Some((d, i));
            }
        }
        if let Some((_, i)) = best {
            return Some(Hit::Target(Target::Stop(i)));
        }
        let hp = fill::handle_points(&g, obb.w, obb.h);
        let mut handles = vec![(GradHandle::End, hp.end), (GradHandle::Start, hp.start)];
        if let Some(w) = hp.width_knob() {
            handles.insert(0, (GradHandle::Width, w));
        }
        for (h, p) in handles {
            if self.local_to_screen(&obb, p, cx).distance(sp) <= PICK {
                return Some(Hit::Target(Target::Handle(h)));
            }
        }
        // On the line (linear / radial): add a stop there.
        if g.kind != GradKind::Angular {
            let a = self.local_to_screen(&obb, hp.start, cx);
            let b = self.local_to_screen(&obb, hp.end, cx);
            let ab = b - a;
            let l2 = ab.length_squared();
            if l2 > 1.0 {
                let t = ((sp - a).dot(ab) / l2).clamp(0.0, 1.0);
                if (a + ab * t).distance(sp) <= LINE_PICK {
                    return Some(Hit::Line((t * 100.0).round() / 100.0));
                }
            }
        }
        None
    }

    /// Mouse down while a gradient is edited: grabs a handle / stop, or adds
    /// a stop on the line. False when nothing was hit.
    pub(super) fn gradient_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) -> bool {
        let sp = Vec2::new(f32::from(ev.position.x), f32::from(ev.position.y));
        let Some(hit) = self.gradient_hit(sp, cx) else {
            return false;
        };
        let Some((_, fe, g0, obb)) = self.gradient_edit(cx) else {
            return false;
        };
        let target = match hit {
            Hit::Target(t) => t,
            Hit::Line(_) => Target::Stop(g0.stops.len()),
        };
        self.state.update(cx, |s, cx| {
            s.begin_gesture();
            s.fill_handle_drag = true;
            if let Hit::Line(t) = hit {
                s.apply(
                    PropEdit::Paint {
                        stroke: fe.stroke,
                        index: fe.index,
                        op: PaintOp::AddStop(t),
                    },
                    cx,
                );
            }
            let stop = match target {
                Target::Stop(i) => Some(i),
                Target::Handle(_) => fe.stop,
            };
            s.set_fill_edit(Some(FillEditTarget { stop, ..fe }), cx);
        });
        self.grad = Some(GradDrag {
            target,
            fe,
            obb,
            g0,
            start: ev.position,
            moved: matches!(hit, Hit::Line(_)),
        });
        cx.notify();
        true
    }

    pub(super) fn on_grad_move(
        &mut self,
        ev: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let world = self.to_doc(ev.position, cx);
        let zoom = self.zoom(cx);
        let Some(d) = &mut self.grad else { return };
        if !d.moved {
            let dd = ev.position - d.start;
            if f32::from(dd.x).abs() < DRAG_START && f32::from(dd.y).abs() < DRAG_START {
                return;
            }
            d.moved = true;
        }
        let Some(mut lp) = d.obb.to_local(world) else {
            return;
        };
        let (w, h) = (d.obb.w, d.obb.h);
        let op = match d.target {
            Target::Handle(hd) => {
                // Snap to the box's corners, edge midpoints and center
                // (Ctrl: free), like Pen.
                if !ev.modifiers.control {
                    let s = d.obb.t.matrix2.determinant().abs().sqrt().max(1e-6);
                    let tol = SNAP / (zoom * s);
                    let mut best = tol;
                    // Pen's 8 snap points: corners and edge midpoints
                    // (not the center).
                    for u in [0.0, 0.5, 1.0] {
                        for vv in [0.0, 0.5, 1.0] {
                            if u == 0.5 && vv == 0.5 {
                                continue;
                            }
                            let q = Vec2::new(u * w, vv * h);
                            let dist = q.distance(lp);
                            if dist < best {
                                best = dist;
                                lp = q;
                            }
                        }
                    }
                }
                let (center, size, rotation) = fill::drag_handle(&d.g0, hd, [lp.x, lp.y], w, h);
                PaintOp::Geometry {
                    center,
                    size,
                    rotation,
                }
            }
            Target::Stop(i) => {
                let t = fill::point_to_t(&d.g0, [lp.x, lp.y], w, h);
                PaintOp::StopPosition(i, (t * 100.0).round() / 100.0)
            }
        };
        let fe = d.fe;
        self.state.update(cx, |s, cx| {
            s.apply(
                PropEdit::Paint {
                    stroke: fe.stroke,
                    index: fe.index,
                    op,
                },
                cx,
            )
        });
        cx.notify();
    }

    pub(super) fn on_grad_up(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.grad.take().is_some() {
            self.state.update(cx, |s, cx| {
                s.fill_handle_drag = false;
                s.end_gesture(cx);
            });
        }
        cx.notify();
    }

    /// Cursor over the gradient handles (`None` = not over them).
    pub(super) fn gradient_cursor(&self, ev: &MouseMoveEvent, cx: &App) -> Option<CursorStyle> {
        let sp = Vec2::new(f32::from(ev.position.x), f32::from(ev.position.y));
        match self.gradient_hit(sp, cx)? {
            Hit::Target(_) => Some(CursorStyle::OpenHand),
            Hit::Line(_) => Some(CursorStyle::Crosshair),
        }
    }

    /// Paints the handles of the edited gradient.
    pub(super) fn paint_gradient(
        &self,
        window: &mut Window,
        to_s: &dyn Fn(Vec2) -> Point<Pixels>,
        cx: &App,
    ) {
        let Some((_, fe, g, obb)) = self.gradient_edit(cx) else {
            return;
        };
        let pt = |p: Vec2| -> Vec2 {
            let s = to_s(p);
            Vec2::new(f32::from(s.x), f32::from(s.y))
        };
        let gp = |p: Vec2| point(px(p.x), px(p.y));
        let local = |p: [f32; 2]| pt(obb.t.transform_point2(v(p)));
        let hp = fill::handle_points(&g, obb.w, obb.h);
        let (s0, s1) = (local(hp.start), local(hp.end));
        let white: Hsla = rgb_u(0xffffff);
        let shade: Hsla = Hsla::from(rgb(0x000000)).opacity(0.35);
        let line = |window: &mut Window, pts: &[Vec2], close: bool| {
            for (w, c) in [(3.0, shade), (1.0, white)] {
                let mut pb = PathBuilder::stroke(px(w));
                pb.move_to(gp(pts[0]));
                for p in &pts[1..] {
                    pb.line_to(gp(*p));
                }
                if close {
                    pb.close();
                }
                if let Ok(path) = pb.build() {
                    window.paint_path(path, c);
                }
            }
        };
        line(window, &[s0, s1], false);
        if let (Some(wp), Some(knob)) = (hp.width, hp.width_knob()) {
            line(window, &[s0, local(knob)], false);
            // Ellipse outline through both radii.
            let c = v(hp.start);
            let a = v(hp.end) - c;
            let b = v(wp) - c;
            let ring: Vec<Vec2> = (0..64)
                .map(|i| {
                    let t = i as f32 / 64.0 * std::f32::consts::TAU;
                    let q = c + a * t.cos() + b * t.sin();
                    local([q.x, q.y])
                })
                .collect();
            line(window, &ring, true);
        }
        let circle = |window: &mut Window, c: Vec2, d: f32, fill: Hsla, border: Hsla, bw: f32| {
            window.paint_quad(quad(
                Bounds {
                    origin: point(px(c.x - d / 2.0), px(c.y - d / 2.0)),
                    size: size(px(d), px(d)),
                },
                px(d / 2.0),
                fill,
                px(bw),
                border,
                BorderStyle::Solid,
            ));
        };
        let mut knobs = vec![s0, s1];
        if let Some(wp) = hp.width_knob() {
            knobs.push(local(wp));
        }
        for k in knobs {
            circle(
                window,
                k + Vec2::new(0.0, 1.0),
                KNOB + 1.0,
                shade,
                shade,
                0.0,
            );
            circle(window, k, KNOB, white, rgb_u(BLUE), 1.5);
        }
        // Stops: color swatches beside the line, joined to their spot.
        let markers = Self::stop_markers(&g, &obb, &pt);
        let mut order: Vec<usize> = (0..markers.len()).collect();
        // The selected stop on top.
        order.sort_by_key(|&i| fe.stop == Some(i));
        for i in order {
            let (anchor, m) = markers[i];
            let sel = fe.stop == Some(i);
            line(window, &[anchor, m], false);
            let d = if sel { STOP_SELECTED } else { STOP };
            circle(window, m + Vec2::new(0.0, 1.0), d + 1.0, shade, shade, 0.0);
            circle(
                window,
                m,
                d,
                rgba(g.stops[i].color).into(),
                if sel { rgb_u(BLUE) } else { white },
                if sel { 3.0 } else { 2.0 },
            );
        }
    }
}
