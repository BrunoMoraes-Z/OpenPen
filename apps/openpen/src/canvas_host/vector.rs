//! Pen tool (P) and vector edit mode of the canvas (docs/vector-editing.md).
//!
//! A [`Session`] edits one `path` node. Its geometry lives in the document:
//! every gesture writes `geometry` (plus the box, see [`write_path`]) through
//! a `DocOp` inside `begin_gesture` / `end_gesture`, so each gesture is one
//! undo step and undo/redo simply re-parse the node ([`Session::sync`]).
//! While the first point of a new path is placed the node does not exist yet:
//! the session then holds that point in world space.

use std::sync::Arc;

use gpui_kit::*;
use pen_doc::{DocOp, Kind, KindData, NodeFlags, NodeKey, Origin};
use pen_layout::path::parse_svg_path;
use pen_layout::vector::{
    Anchor, AnchorRef, Contour, Mirror, SegmentHit, Side, VectorPath, constrain_45,
};
use pen_layout::{NodeLayout, local_transform};
use pen_render::{Affine2, Rect, Vec2};
use pen_schema::{self as ps, Bind};

use super::{CanvasHost, Gesture, rgb_u};
use crate::geom::{Guide, rect_from_points, snap_rect};
use crate::scene_document::{SceneDocument, id_of};
use crate::tools::Tool;

/// Pen's vector-edit sizes (screen px) and colors.
const ANCHOR: f32 = 8.0;
const ANCHOR_HOVER: f32 = 11.0;
const HANDLE_KNOB: f32 = 9.0;
const HANDLE_KNOB_HOVER: f32 = 12.0;
/// Pick radius around anchors and handle knobs.
const PICK: f32 = ANCHOR / 2.0 + 5.0;
const SEGMENT_PICK: f32 = 5.0;
/// Pen-tool placement snapping (Pen: 8 px).
const PEN_SNAP: f32 = 8.0;
const DRAG_START: f32 = 3.0;
const BLUE: u32 = 0x3d99ff;
const GRAY: u32 = 0xaaaaaa;
const SNAP_RED: u32 = 0xf24518;

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Hit {
    Anchor(AnchorRef),
    Handle(AnchorRef, Side),
    Segment(SegmentHit),
    /// Midpoint dot of a straight segment `(contour, segment)`: inserts a point.
    Midpoint(usize, usize),
}

pub(super) struct Session {
    /// Edited node (`None`: first point of a new path, not in the document).
    pub node: Option<NodeKey>,
    /// Frame receiving a new path (while `node` is `None`).
    parent: Option<NodeKey>,
    /// Geometry `path` was parsed from, and its tight bounds.
    geom: Arc<str>,
    bounds: [f32; 4],
    /// Geometry space (world space while `node` is `None`).
    pub path: VectorPath,
    /// Selected anchors (sorted).
    pub sel: Vec<AnchorRef>,
    /// Pen tool: contour being extended and at which end.
    pub append: Option<(usize, Side)>,
    pub hover: Option<Hit>,
    /// Pen tool: where the next point would go (world).
    preview: Option<Vec2>,
    guides: Vec<Guide>,
    /// Snap targets (world) and the document revision they belong to.
    targets: Vec<Rect>,
    targets_rev: u64,
}

pub(super) enum Drag {
    /// Pulling handle `side` of anchor `r` (new pen point, handle knob, or
    /// `connect`: the closing anchor, whose handle on `side` mirrors the
    /// pointer).
    Handle {
        r: AnchorRef,
        side: Side,
        mirror: Mirror,
        orig: VectorPath,
        to_world: Affine2,
        screen0: Point<Pixels>,
        moved: bool,
        connect: bool,
    },
    /// Moving anchors (with their handles).
    Points {
        start: Vec2,
        /// World position of the grabbed point at `start` (snapped).
        grab: Vec2,
        refs: Vec<AnchorRef>,
        orig: VectorPath,
        to_world: Affine2,
        screen0: Point<Pixels>,
        moved: bool,
        /// Selection to apply on release without a drag.
        click_select: Option<Vec<AnchorRef>>,
    },
    Marquee {
        start: Vec2,
        cur: Vec2,
        base: Vec<AnchorRef>,
    },
}

fn path_geometry(doc: &SceneDocument, k: NodeKey) -> Option<Arc<str>> {
    match &doc.get(k)?.data {
        KindData::Path(p) => Some(p.geometry.clone()),
        _ => None,
    }
}

/// A `path` node of the document itself (not inside an instance).
pub(super) fn editable_path(doc: &SceneDocument, k: NodeKey) -> bool {
    doc.get(k).is_some_and(|n| {
        n.kind == Kind::Path
            && matches!(n.origin, Origin::Doc)
            && !n.flags.has(NodeFlags::INSTANCE)
            && matches!(n.data, KindData::Path(_))
    })
}

/// Geometry -> world map of path node `k` (same mapping as the renderer's
/// `fit_path`: `viewBox`, or the tight bounds, scaled to the node box).
fn geometry_to_world(doc: &SceneDocument, k: NodeKey, bounds: [f32; 4]) -> Option<Affine2> {
    let n = doc.get(k)?;
    let KindData::Path(spec) = &n.data else {
        return None;
    };
    let o = doc.world_obb(k)?;
    let (x, y, w, h) = match spec.view_box {
        Some([x, y, w, h]) if w > 0.0 && h > 0.0 => (x, y, w, h),
        _ => (
            bounds[0],
            bounds[1],
            bounds[2] - bounds[0],
            bounds[3] - bounds[1],
        ),
    };
    let s = Vec2::new(
        if w > 1e-6 { o.w / w } else { 1.0 },
        if h > 1e-6 { o.h / h } else { 1.0 },
    );
    Some(o.t * Affine2::from_scale(s) * Affine2::from_translation(-Vec2::new(x, y)))
}

fn round3(v: f32) -> f64 {
    ((v as f64) * 1000.0).round() / 1000.0
}

/// Writes `path` into node `k`. Without an explicit `viewBox` the node box
/// follows the geometry's tight bounds and moves so the drawing stays where
/// it is (Pen: the geometry keeps its coordinates, `x`/`y`/`width`/`height`
/// change). Returns false when nothing changed.
pub(super) fn write_path(doc: &mut SceneDocument, k: NodeKey, path: &VectorPath) -> bool {
    let d = path.to_svg();
    let Some(n) = doc.get(k) else { return false };
    let KindData::Path(spec) = &n.data else {
        return false;
    };
    if *spec.geometry == *d {
        return false;
    }
    let mut patch = ps::Node {
        geometry: Some(d.clone()),
        ..Default::default()
    };
    let explicit = spec.view_box.is_some_and(|[_, _, w, h]| w > 0.0 && h > 0.0);
    if !explicit && let Some(l) = doc.pipe.layout.get(k) {
        let old = parse_svg_path(&spec.geometry).bounds;
        let new = parse_svg_path(&d).bounds;
        let (ow, oh) = (old[2] - old[0], old[3] - old[1]);
        let s = Vec2::new(
            if ow > 1e-6 && l.w > 1e-6 {
                l.w / ow
            } else {
                1.0
            },
            if oh > 1e-6 && l.h > 1e-6 {
                l.h / oh
            } else {
                1.0
            },
        );
        let off = (Vec2::new(new[0], new[1]) - Vec2::new(old[0], old[1])) * s;
        let w = (new[2] - new[0]) * s.x;
        let h = (new[3] - new[1]) * s.y;
        let t_old = local_transform(n, l);
        let t_new = local_transform(
            n,
            &NodeLayout {
                x: 0.0,
                y: 0.0,
                w,
                h,
                text: None,
            },
        );
        let xy = t_old.transform_point2(off) - t_new.transform_point2(Vec2::ZERO);
        if !doc.in_flex(k) {
            if (xy.x - l.x).abs() > 1e-4 {
                patch.x = Some(round3(xy.x));
            }
            if (xy.y - l.y).abs() > 1e-4 {
                patch.y = Some(round3(xy.y));
            }
        }
        if (w - l.w).abs() > 1e-4 {
            patch.width = Some(ps::Sizing::Fixed(round3(w.max(0.0))));
        }
        if (h - l.h).abs() > 1e-4 {
            patch.height = Some(ps::Sizing::Fixed(round3(h.max(0.0))));
        }
    }
    match doc.scene.node_ref(k) {
        Some(t) => doc.apply_op(DocOp::set(t, patch)),
        None => false,
    }
}

/// Creates a path node from `world` (world-space contours) inside `parent`
/// (a frame) or at the top level, with Pen's defaults: 1 px black stroke,
/// no fill, geometry with its first point at the origin.
pub(super) fn create_path(
    doc: &mut SceneDocument,
    parent: Option<NodeKey>,
    world: &VectorPath,
) -> Option<NodeKey> {
    let parent = parent.filter(|&p| {
        doc.get(p)
            .is_some_and(|n| matches!(n.origin, Origin::Doc) && !n.flags.has(NodeFlags::INSTANCE))
    });
    let parent_t = parent
        .and_then(|p| doc.world_obb(p))
        .map_or(Affine2::IDENTITY, |o| o.t);
    let mut path = world.clone();
    path.transform(&parent_t.inverse());
    let origin = path.contours.first()?.anchors.first()?.p;
    path.transform(&Affine2::from_translation(-origin));
    let d = path.to_svg();
    if d.is_empty() {
        return None;
    }
    let b = parse_svg_path(&d).bounds;
    let id = ps::new_id();
    let mut node = ps::Node::new(ps::NodeKind::Path, id.clone());
    node.x = Some(round3(origin.x + b[0]));
    node.y = Some(round3(origin.y + b[1]));
    node.width = Some(ps::Sizing::Fixed(round3(b[2] - b[0])));
    node.height = Some(ps::Sizing::Fixed(round3(b[3] - b[1])));
    node.geometry = Some(d);
    node.stroke = Some(ps::OneOrMany::One(ps::Fill::Solid(ps::ColorOrVar(
        "#000000".into(),
    ))));
    node.stroke_width = Some(ps::StrokeWidth::Uniform(Bind::Value(1.0)));
    let (pid, index) = match parent {
        Some(p) => (
            doc.get(p).map(|n| n.id.clone()),
            doc.scene.children(p).len(),
        ),
        None => (None, doc.scene.roots().len()),
    };
    let op = DocOp::Insert {
        parent: pid,
        index,
        node: Box::new(node),
    };
    if !doc.apply_op(op) {
        return None;
    }
    doc.scene.find(&id)
}

impl Session {
    fn new(node: Option<NodeKey>, parent: Option<NodeKey>) -> Session {
        Session {
            node,
            parent,
            geom: Arc::from(""),
            bounds: [0.0; 4],
            path: VectorPath::default(),
            sel: Vec::new(),
            append: None,
            hover: None,
            preview: None,
            guides: Vec::new(),
            targets: Vec::new(),
            targets_rev: 0,
        }
    }

    /// Re-reads the node after document changes (undo, agents, our own
    /// writes). False when the node is gone.
    pub(super) fn sync(&mut self, doc: &SceneDocument) -> bool {
        let Some(k) = self.node else { return true };
        let Some(g) = path_geometry(doc, k) else {
            return false;
        };
        if !Arc::ptr_eq(&g, &self.geom) {
            if *g != *self.geom {
                let p = VectorPath::parse(&g);
                if !p.same_structure(&self.path) {
                    self.hover = None;
                }
                self.path = p;
                self.bounds = parse_svg_path(&g).bounds;
            }
            self.geom = g;
        }
        let path = &self.path;
        self.sel.retain(|&r| path.anchor(r).is_some());
        if let Some((ci, _)) = self.append
            && path
                .contours
                .get(ci)
                .is_none_or(|c| c.closed || c.anchors.is_empty())
        {
            self.append = None;
        }
        true
    }

    fn to_world(&self, doc: &SceneDocument) -> Affine2 {
        self.node
            .and_then(|k| geometry_to_world(doc, k, self.bounds))
            .unwrap_or(Affine2::IDENTITY)
    }

    /// Anchor the pen extends, and the side its next segment leaves from.
    fn extending(&self) -> Option<(AnchorRef, Side)> {
        let (ci, side) = self.append?;
        let c = self.path.contours.get(ci)?;
        let ai = match side {
            Side::Out => c.anchors.len().checked_sub(1)?,
            Side::In => 0,
        };
        Some(((ci, ai), side))
    }

    /// Anchor that closes the contour being extended (its other end).
    fn closing(&self) -> Option<AnchorRef> {
        let (ci, side) = self.append?;
        let c = self.path.contours.get(ci)?;
        if c.anchors.len() < 2 {
            return None;
        }
        Some(match side {
            Side::Out => (ci, 0),
            Side::In => (ci, c.anchors.len() - 1),
        })
    }

    /// Anchors whose handles are shown: the selection, their neighbors and
    /// the anchor being extended.
    fn shows_handles(&self, r: AnchorRef) -> bool {
        if self.sel.contains(&r) || self.extending().is_some_and(|(e, _)| e == r) {
            return true;
        }
        let Some(c) = self.path.contours.get(r.0) else {
            return false;
        };
        let (p, n) = c.neighbors(r.1);
        [p, n]
            .into_iter()
            .flatten()
            .any(|i| self.sel.contains(&(r.0, i)))
    }

    fn select(&mut self, r: AnchorRef, additive: bool) {
        if additive {
            if let Some(i) = self.sel.iter().position(|&s| s == r) {
                self.sel.remove(i);
            } else {
                self.sel.push(r);
                self.sel.sort_unstable();
            }
        } else {
            self.sel = vec![r];
        }
    }

    fn hit(&self, to_world: &Affine2, p: Vec2, zoom: f32, pen: bool) -> Option<Hit> {
        let path = &self.path;
        let r = PICK / zoom;
        if let Some((a, s)) = path.hit_handle(to_world, p, r, ANCHOR / 2.0 / zoom, |a| {
            self.shows_handles(a)
        }) {
            return Some(Hit::Handle(a, s));
        }
        if let Some(a) = path.hit_anchor(to_world, p, r) {
            return Some(Hit::Anchor(a));
        }
        let seg = path.hit_segment(to_world, p, SEGMENT_PICK / zoom)?;
        let s = path.contours[seg.contour].segment(seg.segment)?;
        if !pen && !s.curve {
            let mid = to_world.transform_point2(s.at(0.5));
            if mid.distance(p) <= ANCHOR / zoom {
                return Some(Hit::Midpoint(seg.contour, seg.segment));
            }
        }
        Some(Hit::Segment(seg))
    }
}

impl CanvasHost {
    /// Starts editing path `k` (double click / Enter).
    pub(super) fn enter_vector(&mut self, k: NodeKey, cx: &mut Context<Self>) -> bool {
        let ok = editable_path(&self.state.read(cx).doc, k);
        if !ok {
            return false;
        }
        let mut s = Session::new(Some(k), None);
        if !s.sync(&self.state.read(cx).doc) {
            return false;
        }
        self.vector = Some(s);
        self.hover = None;
        self.hover_handle = None;
        self.state.update(cx, |st, cx| {
            st.set_selection(vec![id_of(k)], cx);
            if !matches!(st.tool, Tool::Move | Tool::Pen) {
                st.set_tool(Tool::Move, cx);
            }
        });
        cx.notify();
        true
    }

    /// Leaves vector edit mode, selecting the edited path.
    pub(super) fn exit_vector(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.vector.take() else { return };
        if matches!(self.gesture, Gesture::Vector(_)) {
            self.gesture = Gesture::None;
            self.state.update(cx, |st, cx| {
                if st.doc.in_gesture() {
                    st.end_gesture(cx);
                }
            });
        }
        self.state.update(cx, |st, cx| {
            if let Some(k) = s.node
                && st.doc.get(k).is_some()
            {
                st.set_selection(vec![id_of(k)], cx);
            }
            st.set_tool(Tool::Move, cx);
        });
        cx.notify();
    }

    /// After document / tool changes: keeps the session in step or ends it.
    pub(super) fn vector_sync(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &mut self.vector else { return };
        let st = self.state.read(cx);
        let alive = s.sync(&st.doc);
        let tool_ok = matches!(st.tool, Tool::Move | Tool::Pen);
        let sel_ok = match s.node {
            Some(k) => st.selection.is_empty() || st.selection == [id_of(k)],
            None => st.tool == Tool::Pen,
        };
        if !alive || !tool_ok || !sel_ok {
            let had_node = s.node.is_some() && alive;
            if had_node {
                self.exit_vector(cx);
            } else {
                self.vector = None;
                if matches!(self.gesture, Gesture::Vector(_)) {
                    self.gesture = Gesture::None;
                }
            }
            cx.notify();
        }
    }

    fn vec_targets(&mut self, cx: &App) -> Vec<Rect> {
        let st = self.state.read(cx);
        let rev = st.doc.revision();
        let Some(s) = &mut self.vector else {
            return Vec::new();
        };
        if s.targets_rev != rev {
            let doc = &st.doc;
            let mut out: Vec<Rect> = match s.node {
                Some(k) => doc.snap_targets(&[k]),
                None => {
                    let sibs: &[NodeKey] = match s.parent {
                        Some(p) => doc.scene.children(p),
                        None => doc.scene.roots(),
                    };
                    let mut v: Vec<Rect> = sibs
                        .iter()
                        .filter(|k| doc.get(**k).is_some_and(|n| n.enabled()))
                        .filter_map(|&k| doc.world_aabb(k))
                        .collect();
                    if let Some(r) = s.parent.and_then(|p| doc.world_aabb(p)) {
                        v.push(r);
                    }
                    v
                }
            };
            // Large documents: keep the snap pass cheap.
            out.truncate(4096);
            s.targets = out;
            s.targets_rev = rev;
        }
        s.targets.clone()
    }

    /// Snaps world point `p` to the edited path's other anchors and the
    /// sibling boxes; unsnapped axes are rounded to 0.5 (Pen).
    fn vec_snap(
        &mut self,
        p: Vec2,
        exclude: &[AnchorRef],
        snapping: bool,
        cx: &App,
    ) -> (Vec2, Vec<Guide>) {
        if !snapping {
            return (p, Vec::new());
        }
        let zoom = self.zoom(cx);
        let mut targets = self.vec_targets(cx);
        if let Some(s) = &self.vector {
            let m = s.to_world(&self.state.read(cx).doc);
            for r in s.path.anchor_refs() {
                if !exclude.contains(&r) {
                    let a = m.transform_point2(s.path.anchor(r).unwrap().p);
                    targets.push(Rect::new(a.x, a.y, 0.0, 0.0));
                }
            }
        }
        let sn = snap_rect(&Rect::new(p.x, p.y, 0.0, 0.0), &targets, PEN_SNAP / zoom);
        let mut q = p + Vec2::new(sn.dx, sn.dy);
        if sn.dx == 0.0 {
            q.x = (q.x * 2.0).round() / 2.0;
        }
        if sn.dy == 0.0 {
            q.y = (q.y * 2.0).round() / 2.0;
        }
        (q, sn.guides)
    }

    /// Where a pen click at `p` puts the next point (Shift: 45° from the
    /// extended anchor; otherwise snapped).
    fn pen_target(&mut self, p: Vec2, m: Modifiers, cx: &App) -> (Vec2, Vec<Guide>) {
        let from = self.vector.as_ref().and_then(|s| {
            let (r, _) = s.extending()?;
            let w = s.to_world(&self.state.read(cx).doc);
            Some(w.transform_point2(s.path.anchor(r)?.p))
        });
        match from {
            Some(o) if m.shift => (constrain_45(o, p), Vec::new()),
            _ => self.vec_snap(p, &[], !(m.control || m.platform), cx),
        }
    }

    // ---------------------------------------------------------- pointer

    /// Mouse down while the pen tool is active or a path is edited. True when
    /// handled.
    pub(super) fn vector_down(
        &mut self,
        ev: &MouseDownEvent,
        p: Vec2,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let _ = window;
        let tool = self.state.read(cx).tool;
        if self.vector.is_none() {
            if tool != Tool::Pen {
                return false;
            }
            // First point of a new path.
            let parent = self.state.update(cx, |s, cx| {
                s.clear_selection(cx);
                // Not inside auto layout: the layout would move the drawing.
                let mut parent = s.doc.drop_parent(p);
                while let Some(n) = parent.and_then(|k| s.doc.get(k))
                    && n.layout.mode != pen_doc::LayoutMode::None
                {
                    parent = n
                        .parent
                        .filter(|&q| s.doc.get(q).is_some_and(|n| n.kind == Kind::Frame));
                }
                parent
            });
            self.vector = Some(Session::new(None, parent));
        }
        self.vector_sync(cx);
        if self.vector.is_none() {
            return tool == Tool::Pen;
        }
        let zoom = self.zoom(cx);
        let m = ev.modifiers;
        let pen = tool == Tool::Pen;
        let (to_world, hit) = {
            let s = self.vector.as_ref().unwrap();
            let w = s.to_world(&self.state.read(cx).doc);
            (w, s.hit(&w, p, zoom, pen && !m.alt))
        };
        if pen {
            self.pen_down(ev, p, hit, to_world, cx);
        } else {
            self.select_down(ev, p, hit, to_world, cx);
        }
        cx.notify();
        true
    }

    fn begin_drag(&mut self, d: Drag, cx: &mut Context<Self>) {
        if !self.state.read(cx).doc.in_gesture() {
            self.state.update(cx, |s, _| s.begin_gesture());
        }
        self.gesture = Gesture::Vector(Box::new(d));
    }

    /// Writes the session path into the document (no-op without a node or
    /// when only a contour with one point changed).
    fn vec_write(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &self.vector else { return };
        let Some(k) = s.node else { return };
        let path = s.path.clone();
        self.state.update(cx, |st, cx| {
            if write_path(&mut st.doc, k, &path) {
                st.changed(cx);
            }
        });
        if let Some(s) = &mut self.vector {
            let doc = &self.state.read(cx).doc;
            // Our own write: keep the in-memory model (it may hold a contour
            // with a single point that is not written yet).
            if let Some(g) = path_geometry(doc, k) {
                s.bounds = parse_svg_path(&g).bounds;
                s.geom = g;
            }
        }
    }

    fn pen_down(
        &mut self,
        ev: &MouseDownEvent,
        p: Vec2,
        hit: Option<Hit>,
        to_world: Affine2,
        cx: &mut Context<Self>,
    ) {
        let m = ev.modifiers;
        let screen0 = ev.position;
        let s = self.vector.as_mut().unwrap();
        // Handle knobs stay draggable with the pen.
        if let Some(Hit::Handle(r, side)) = hit {
            let a = *s.path.anchor(r).unwrap();
            let orig = s.path.clone();
            self.begin_drag(
                Drag::Handle {
                    r,
                    side,
                    mirror: handle_mirror(&a, m),
                    orig,
                    to_world,
                    screen0,
                    moved: false,
                    connect: false,
                },
                cx,
            );
            return;
        }
        if s.append.is_some() {
            // Close on the other end of the contour.
            if let (Some(Hit::Anchor(r)), Some(c)) = (hit, s.closing())
                && r == c
            {
                let (ci, side) = s.append.unwrap();
                let close_side = side.opposite();
                s.path.contours[ci].closed = true;
                if let Some(a) = s.path.anchor_mut(r) {
                    a.set_handle(close_side, None);
                }
                s.append = None;
                s.sel = vec![r];
                s.preview = None;
                let orig = s.path.clone();
                self.begin_drag(
                    Drag::Handle {
                        r,
                        side: close_side,
                        mirror: Mirror::None,
                        orig,
                        to_world,
                        screen0,
                        moved: false,
                        connect: true,
                    },
                    cx,
                );
                self.vec_write(cx);
                return;
            }
            // Place the next point.
            let (target, _) = self.pen_target(p, m, cx);
            self.pen_place(target, to_world, screen0, cx);
            return;
        }
        match hit {
            Some(Hit::Anchor(r)) => {
                let s = self.vector.as_mut().unwrap();
                if let Some(side) = s.path.contours[r.0].endpoint(r.1) {
                    // Resume an open contour from this end.
                    if let Some(a) = s.path.anchor_mut(r) {
                        a.set_handle(side, None);
                    }
                    s.append = Some((r.0, side));
                    s.sel = vec![r];
                    let orig = s.path.clone();
                    self.begin_drag(
                        Drag::Handle {
                            r,
                            side,
                            mirror: Mirror::None,
                            orig,
                            to_world,
                            screen0,
                            moved: false,
                            connect: false,
                        },
                        cx,
                    );
                } else {
                    s.sel = vec![r];
                    self.start_points_drag(p, Some(r), screen0, to_world, None, cx);
                }
            }
            Some(Hit::Segment(h)) => self.insert_point(h.contour, h.segment, h.t, p, screen0, cx),
            Some(Hit::Midpoint(ci, si)) => self.insert_point(ci, si, 0.5, p, screen0, cx),
            Some(Hit::Handle(..)) => {}
            None => {
                // A new contour (in the edited path, or the first point of a
                // new path).
                let (target, _) = self.pen_target(p, m, cx);
                let s = self.vector.as_mut().unwrap();
                let g = to_world.inverse().transform_point2(target);
                s.path
                    .contours
                    .push(Contour::new(vec![Anchor::corner(g)], false));
                let ci = s.path.contours.len() - 1;
                s.append = Some((ci, Side::Out));
                s.sel = vec![(ci, 0)];
                let orig = s.path.clone();
                self.begin_drag(
                    Drag::Handle {
                        r: (ci, 0),
                        side: Side::Out,
                        mirror: Mirror::Full,
                        orig,
                        to_world,
                        screen0,
                        moved: false,
                        connect: false,
                    },
                    cx,
                );
            }
        }
    }

    /// Adds a point at world `target` to the contour being extended; creates
    /// the node when this is the second point of a new path.
    fn pen_place(
        &mut self,
        target: Vec2,
        mut to_world: Affine2,
        screen0: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let s = self.vector.as_mut().unwrap();
        let Some((ci, side)) = s.append else { return };
        let g = to_world.inverse().transform_point2(target);
        let c = &mut s.path.contours[ci];
        let ai = match side {
            Side::Out => {
                c.anchors.push(Anchor::corner(g));
                c.anchors.len() - 1
            }
            Side::In => {
                c.anchors.insert(0, Anchor::corner(g));
                0
            }
        };
        s.sel = vec![(ci, ai)];
        s.preview = None;
        // One undo step: the new point (and the node) plus its handle drag.
        self.state.update(cx, |st, _| st.begin_gesture());
        let s = self.vector.as_mut().unwrap();
        if s.node.is_none() {
            let world = s.path.clone();
            let parent = s.parent;
            let made = self.state.update(cx, |st, cx| {
                let k = create_path(&mut st.doc, parent, &world);
                st.changed(cx);
                k
            });
            let Some(k) = made else {
                self.state.update(cx, |st, cx| st.end_gesture(cx));
                self.vector = None;
                return;
            };
            let s = self.vector.as_mut().unwrap();
            s.node = Some(k);
            s.geom = Arc::from("");
            let doc = &self.state.read(cx).doc;
            s.sync(doc);
            s.sel = vec![(ci, ai)];
            to_world = s.to_world(doc);
            self.state
                .update(cx, |st, cx| st.set_selection(vec![id_of(k)], cx));
        } else {
            self.vec_write(cx);
        }
        let orig = self.vector.as_ref().unwrap().path.clone();
        self.gesture = Gesture::Vector(Box::new(Drag::Handle {
            r: (ci, ai),
            side,
            mirror: Mirror::Full,
            orig,
            to_world,
            screen0,
            moved: false,
            connect: false,
        }));
    }

    fn insert_point(
        &mut self,
        ci: usize,
        si: usize,
        t: f32,
        p: Vec2,
        screen0: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let s = self.vector.as_mut().unwrap();
        let Some(r) = s.path.split_segment(ci, si, t) else {
            return;
        };
        // The extended end keeps its meaning (the new point is never first).
        s.sel = vec![r];
        let to_world = s.to_world(&self.state.read(cx).doc);
        self.state.update(cx, |st, _| st.begin_gesture());
        self.vec_write(cx);
        self.start_points_drag(p, Some(r), screen0, to_world, None, cx);
    }

    fn start_points_drag(
        &mut self,
        p: Vec2,
        grabbed: Option<AnchorRef>,
        screen0: Point<Pixels>,
        to_world: Affine2,
        click_select: Option<Vec<AnchorRef>>,
        cx: &mut Context<Self>,
    ) {
        let s = self.vector.as_ref().unwrap();
        let grab = grabbed
            .and_then(|r| s.path.anchor(r))
            .map_or(p, |a| to_world.transform_point2(a.p));
        let d = Drag::Points {
            start: p,
            grab,
            refs: s.sel.clone(),
            orig: s.path.clone(),
            to_world,
            screen0,
            moved: false,
            click_select,
        };
        self.begin_drag(d, cx);
    }

    fn select_down(
        &mut self,
        ev: &MouseDownEvent,
        p: Vec2,
        hit: Option<Hit>,
        to_world: Affine2,
        cx: &mut Context<Self>,
    ) {
        let m = ev.modifiers;
        let screen0 = ev.position;
        let s = self.vector.as_mut().unwrap();
        if ev.click_count >= 2 {
            match hit {
                Some(Hit::Anchor(r)) => {
                    let mut path = s.path.clone();
                    if path.toggle_smooth(r) {
                        s.path = path;
                        s.sel = vec![r];
                        self.state.update(cx, |st, _| st.begin_gesture());
                        self.vec_write(cx);
                        self.state.update(cx, |st, cx| st.end_gesture(cx));
                    }
                }
                Some(Hit::Segment(h)) => {
                    let n = s.path.contours[h.contour].anchors.len();
                    s.sel = (0..n).map(|i| (h.contour, i)).collect();
                }
                Some(_) => {}
                None => self.exit_vector(cx),
            }
            return;
        }
        match hit {
            Some(Hit::Handle(r, side)) => {
                let a = *s.path.anchor(r).unwrap();
                let orig = s.path.clone();
                self.begin_drag(
                    Drag::Handle {
                        r,
                        side,
                        mirror: handle_mirror(&a, m),
                        orig,
                        to_world,
                        screen0,
                        moved: false,
                        connect: false,
                    },
                    cx,
                );
            }
            Some(Hit::Anchor(r)) => {
                let was = s.sel.contains(&r);
                let mut click = None;
                if m.shift {
                    s.select(r, true);
                } else if !was {
                    s.sel = vec![r];
                } else if s.sel.len() > 1 {
                    click = Some(vec![r]);
                }
                if s.sel.contains(&r) {
                    self.start_points_drag(p, Some(r), screen0, to_world, click, cx);
                }
            }
            Some(Hit::Midpoint(ci, si)) => self.insert_point(ci, si, 0.5, p, screen0, cx),
            Some(Hit::Segment(h)) => {
                if m.alt {
                    self.insert_point(h.contour, h.segment, h.t, p, screen0, cx);
                    return;
                }
                let n = s.path.contours[h.contour].anchors.len();
                let ends = [(h.contour, h.segment), (h.contour, (h.segment + 1) % n)];
                if !m.shift {
                    s.sel.clear();
                }
                for r in ends {
                    if !s.sel.contains(&r) {
                        s.sel.push(r);
                    }
                }
                s.sel.sort_unstable();
                self.start_points_drag(p, None, screen0, to_world, None, cx);
            }
            None => {
                let base = if m.shift { s.sel.clone() } else { Vec::new() };
                s.sel = base.clone();
                self.gesture = Gesture::Vector(Box::new(Drag::Marquee {
                    start: p,
                    cur: p,
                    base,
                }));
            }
        }
    }

    pub(super) fn vector_drag_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let p = self.to_doc(ev.position, cx);
        let m = ev.modifiers;
        let snapping = !(m.control || m.platform);
        let Gesture::Vector(drag) = &mut self.gesture else {
            return;
        };
        match &mut **drag {
            Drag::Handle {
                r,
                side,
                mirror,
                orig,
                to_world,
                screen0,
                moved,
                connect,
            } => {
                if !*moved {
                    let d = ev.position - *screen0;
                    if f32::from(d.x).abs().max(f32::from(d.y).abs()) < DRAG_START {
                        return;
                    }
                    *moved = true;
                }
                if m.alt {
                    *mirror = Mirror::None;
                } else if m.secondary(cx) {
                    *mirror = Mirror::Full;
                }
                let (r, side, mirror, connect, to_world) =
                    (*r, *side, *mirror, *connect, *to_world);
                let mut path = orig.clone();
                let anchor_w = path
                    .anchor(r)
                    .map(|a| to_world.transform_point2(a.p))
                    .unwrap_or(p);
                let target = if m.shift {
                    constrain_45(anchor_w, p)
                } else if snapping {
                    // Handles snap to the other anchors and boxes too.
                    self.vec_snap(p, &[r], true, cx).0
                } else {
                    p
                };
                let g = to_world.inverse().transform_point2(target);
                if let Some(a) = path.anchor_mut(r) {
                    if connect {
                        a.set_handle(side, Some(a.p * 2.0 - g));
                    } else {
                        a.drag_handle(side, g, mirror);
                    }
                }
                if let Some(s) = &mut self.vector {
                    s.path = path;
                }
                self.vec_write(cx);
            }
            Drag::Points {
                start,
                grab,
                refs,
                orig,
                to_world,
                screen0,
                moved,
                ..
            } => {
                if !*moved {
                    let d = ev.position - *screen0;
                    if f32::from(d.x).abs().max(f32::from(d.y).abs()) < DRAG_START {
                        return;
                    }
                    *moved = true;
                }
                let raw = p - *start;
                // Shift: keep the dominant axis only (`true` = horizontal).
                let lock = m.shift.then_some(raw.x.abs() > raw.y.abs());
                let axis = |mut d: Vec2| {
                    match lock {
                        Some(true) => d.y = 0.0,
                        Some(false) => d.x = 0.0,
                        None => {}
                    }
                    d
                };
                let (grab, refs, orig, to_world) = (*grab, refs.clone(), orig.clone(), *to_world);
                let (snapped, guides) = self.vec_snap(grab + axis(raw), &refs, snapping, cx);
                let d = axis(snapped - grab);
                let gd = to_world.inverse().transform_vector2(d);
                let mut path = orig;
                path.translate_anchors(&refs, gd);
                if let Some(s) = &mut self.vector {
                    s.path = path;
                    s.guides = guides;
                }
                self.vec_write(cx);
            }
            Drag::Marquee { start, cur, base } => {
                *cur = p;
                let r = rect_from_points(*start, p);
                let base = base.clone();
                if let Some(s) = &mut self.vector {
                    let w = s.to_world(&self.state.read(cx).doc);
                    let mut sel = base;
                    for a in s.path.anchor_refs() {
                        let q = w.transform_point2(s.path.anchor(a).unwrap().p);
                        if crate::geom::rect_contains(&r, q) && !sel.contains(&a) {
                            sel.push(a);
                        }
                    }
                    sel.sort_unstable();
                    s.sel = sel;
                }
            }
        }
        cx.notify();
    }

    pub(super) fn vector_up(&mut self, drag: Drag, cx: &mut Context<Self>) {
        let zoom = self.zoom(cx);
        match drag {
            Drag::Handle {
                r,
                side,
                moved,
                to_world,
                connect,
                ..
            } => {
                // A handle dropped on its anchor is removed (Pen).
                if moved && let Some(s) = &mut self.vector {
                    let mut changed = false;
                    if let Some(a) = s.path.anchor_mut(r) {
                        let ap = to_world.transform_point2(a.p);
                        let sides: &[Side] = if connect {
                            &[side]
                        } else {
                            &[Side::In, Side::Out]
                        };
                        for &sd in sides {
                            if let Some(h) = a.handle(sd)
                                && to_world.transform_point2(h).distance(ap) <= ANCHOR / 2.0 / zoom
                            {
                                a.set_handle(sd, None);
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        self.vec_write(cx);
                    }
                }
            }
            Drag::Points {
                moved,
                click_select,
                ..
            } => {
                if !moved
                    && let Some(sel) = click_select
                    && let Some(s) = &mut self.vector
                {
                    s.sel = sel;
                }
            }
            Drag::Marquee { .. } => {}
        }
        if let Some(s) = &mut self.vector {
            s.guides.clear();
        }
        self.state.update(cx, |st, cx| {
            if st.doc.in_gesture() {
                st.end_gesture(cx);
            }
        });
        self.vector_sync(cx);
        cx.notify();
    }

    /// Hover in vector mode: hit highlight and the pen's rubber band.
    pub(super) fn vector_hover(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let tool = self.state.read(cx).tool;
        let pen = tool == Tool::Pen;
        let p = self.to_doc(ev.position, cx);
        let zoom = self.zoom(cx);
        let m = ev.modifiers;
        let appending = self.vector.as_ref().is_some_and(|s| s.append.is_some());
        let (preview, guides) = if pen && appending {
            let (q, g) = self.pen_target(p, m, cx);
            (Some(q), g)
        } else {
            (None, Vec::new())
        };
        let Some(s) = &mut self.vector else { return };
        let w = s.to_world(&self.state.read(cx).doc);
        let hit = s.hit(&w, p, zoom, pen && !m.alt);
        let hit = if appending {
            // While drawing only the closing anchor and handles react.
            match hit {
                Some(Hit::Anchor(r)) if Some(r) == s.closing() => hit,
                Some(Hit::Handle(..)) => hit,
                _ => None,
            }
        } else {
            hit
        };
        if s.hover != hit || s.preview != preview || s.guides != guides {
            s.hover = hit;
            s.preview = preview;
            s.guides = guides;
            cx.notify();
        }
    }

    // ---------------------------------------------------------- keyboard

    /// Escape: stop drawing / clear the point selection / leave the mode.
    pub(super) fn vector_escape(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(s) = &mut self.vector else {
            return false;
        };
        if s.node.is_none() {
            self.vector = None;
            self.state.update(cx, |st, cx| st.set_tool(Tool::Move, cx));
        } else if s.append.is_some() || s.sel.is_empty() {
            self.exit_vector(cx);
        } else {
            s.sel.clear();
        }
        cx.notify();
        true
    }

    /// Enter: finish (leave the mode).
    pub(super) fn vector_enter(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(s) = &self.vector else {
            return false;
        };
        if s.node.is_none() {
            self.vector = None;
            self.state.update(cx, |st, cx| st.set_tool(Tool::Move, cx));
        } else {
            self.exit_vector(cx);
        }
        cx.notify();
        true
    }

    /// Delete / Backspace: removes the selected points (the node when
    /// nothing is left). One undo step.
    pub(super) fn vector_delete(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(s) = &mut self.vector else {
            return false;
        };
        let Some(k) = s.node else {
            self.vector = None;
            return true;
        };
        if s.sel.is_empty() {
            return true;
        }
        let mut path = s.path.clone();
        if !path.delete_anchors(&s.sel) {
            return true;
        }
        s.sel.clear();
        s.hover = None;
        s.append = None;
        if path.contours.iter().all(|c| !c.is_drawable()) {
            self.vector = None;
            self.state.update(cx, |st, cx| {
                st.doc.delete(&[k]);
                st.changed(cx);
                st.set_tool(Tool::Move, cx);
            });
        } else {
            s.path = path;
            self.state.update(cx, |st, _| st.begin_gesture());
            self.vec_write(cx);
            self.state.update(cx, |st, cx| st.end_gesture(cx));
        }
        cx.notify();
        true
    }

    pub(super) fn vector_select_all(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(s) = &mut self.vector else {
            return false;
        };
        s.sel = s.path.anchor_refs().collect();
        cx.notify();
        true
    }

    /// Arrow keys: nudge the selected points by (`dx`, `dy`) world units.
    pub(super) fn vector_nudge(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) -> bool {
        let Some(s) = &mut self.vector else {
            return false;
        };
        if s.sel.is_empty() || s.node.is_none() {
            return true;
        }
        let w = s.to_world(&self.state.read(cx).doc);
        let gd = w.inverse().transform_vector2(Vec2::new(dx, dy));
        let refs = s.sel.clone();
        s.path.translate_anchors(&refs, gd);
        self.state.update(cx, |st, _| st.begin_gesture());
        self.vec_write(cx);
        self.state.update(cx, |st, cx| st.end_gesture(cx));
        cx.notify();
        true
    }

    // ---------------------------------------------------------- painting

    pub(super) fn paint_vector(
        &self,
        window: &mut Window,
        to_s: &dyn Fn(Vec2) -> Point<Pixels>,
        cx: &App,
    ) {
        let Some(s) = &self.vector else { return };
        let doc = &self.state.read(cx).doc;
        let w = s.to_world(doc);
        let pt = |g: Vec2| to_s(w.transform_point2(g));
        let blue = rgb_u(BLUE);
        let white = rgb_u(0xffffff);
        let path = &s.path;
        let tool_pen = self.state.read(cx).tool == Tool::Pen;

        // Outline of the whole path.
        let mut pb = PathBuilder::stroke(px(1.));
        let mut any = false;
        for c in &path.contours {
            let Some(first) = c.anchors.first() else {
                continue;
            };
            pb.move_to(pt(first.p));
            for i in 0..c.segment_count() {
                let sg = c.segment(i).unwrap();
                if sg.curve {
                    pb.cubic_bezier_to(pt(sg.p3), pt(sg.c1), pt(sg.c2));
                } else {
                    pb.line_to(pt(sg.p3));
                }
                any = true;
            }
        }
        if any && let Ok(p) = pb.build() {
            window.paint_path(p, rgb_u(GRAY));
        }

        // Hovered segment (and the midpoint dot of straight ones).
        let seg_hover = match s.hover {
            Some(Hit::Segment(h)) => Some((h.contour, h.segment, false)),
            Some(Hit::Midpoint(ci, si)) => Some((ci, si, true)),
            _ => None,
        };
        if let Some((ci, si, on_mid)) = seg_hover
            && let Some(sg) = path.contours.get(ci).and_then(|c| c.segment(si))
            && s.append.is_none()
        {
            let mut pb = PathBuilder::stroke(px(2.5));
            pb.move_to(pt(sg.p0));
            if sg.curve {
                pb.cubic_bezier_to(pt(sg.p3), pt(sg.c1), pt(sg.c2));
            } else {
                pb.line_to(pt(sg.p3));
            }
            if let Ok(p) = pb.build() {
                window.paint_path(p, blue.opacity(0.4));
            }
            if !sg.curve && !tool_pen {
                let d = if on_mid { ANCHOR_HOVER } else { ANCHOR };
                paint_dot(window, pt(sg.at(0.5)), d, white, blue);
            }
        }
        if let Some(Hit::Segment(h)) = s.hover
            && tool_pen
            && s.append.is_none()
            && let Some(sg) = path
                .contours
                .get(h.contour)
                .and_then(|c| c.segment(h.segment))
        {
            // Pen over a segment: where the inserted point goes.
            paint_dot(window, pt(sg.at(h.t)), ANCHOR, white, blue);
        }

        // Selection bounds of several points.
        if s.sel.len() >= 2 {
            let pts: Vec<Point<Pixels>> = s
                .sel
                .iter()
                .filter_map(|&r| path.anchor(r))
                .map(|a| pt(a.p))
                .collect();
            if let (Some(x0), Some(x1), Some(y0), Some(y1)) = (
                pts.iter().map(|p| p.x).reduce(|a, b| a.min(b)),
                pts.iter().map(|p| p.x).reduce(|a, b| a.max(b)),
                pts.iter().map(|p| p.y).reduce(|a, b| a.min(b)),
                pts.iter().map(|p| p.y).reduce(|a, b| a.max(b)),
            ) {
                window.paint_quad(quad(
                    Bounds::from_corners(point(x0, y0), point(x1, y1)),
                    px(0.),
                    transparent_black(),
                    px(1.),
                    blue,
                    BorderStyle::Solid,
                ));
            }
        }

        // Handles: line + diamond knob.
        for r in path.anchor_refs().filter(|&r| s.shows_handles(r)) {
            let a = path.anchor(r).unwrap();
            let ap = pt(a.p);
            for side in [Side::In, Side::Out] {
                let Some(h) = a.handle(side) else { continue };
                let hp = pt(h);
                let mut pb = PathBuilder::stroke(px(1.));
                pb.move_to(ap);
                pb.line_to(hp);
                if let Ok(p) = pb.build() {
                    window.paint_path(p, blue);
                }
                let size = if s.hover == Some(Hit::Handle(r, side)) {
                    HANDLE_KNOB_HOVER
                } else {
                    HANDLE_KNOB
                };
                paint_diamond(window, hp, size, white, blue);
            }
        }

        // Pen rubber band from the extended anchor to the next point.
        if let (Some(q), Some((r, side))) = (s.preview, s.extending())
            && !matches!(self.gesture, Gesture::Vector(_))
            && let Some(a) = path.anchor(r)
        {
            let mut pb = PathBuilder::stroke(px(1.));
            pb.move_to(pt(a.p));
            let qs = to_s(q);
            match a.handle(side) {
                Some(h) => pb.cubic_bezier_to(qs, pt(h), qs),
                None => pb.line_to(qs),
            }
            if let Ok(p) = pb.build() {
                window.paint_path(p, blue);
            }
            let closing = s.closing().is_some_and(|c| s.hover == Some(Hit::Anchor(c)));
            if !closing {
                paint_dot(window, qs, ANCHOR, white, blue);
            }
        }

        // Anchors.
        for r in path.anchor_refs() {
            let a = path.anchor(r).unwrap();
            let selected = s.sel.contains(&r);
            let size = if s.hover == Some(Hit::Anchor(r)) {
                ANCHOR_HOVER
            } else {
                ANCHOR
            };
            let (fill, border) = if selected {
                (blue, white)
            } else {
                (white, blue)
            };
            paint_dot(window, pt(a.p), size, fill, border);
        }

        // Marquee and snap guides.
        if let Gesture::Vector(d) = &self.gesture
            && let Drag::Marquee { start, cur, .. } = &**d
        {
            let (a, b) = (to_s(*start), to_s(*cur));
            window.paint_quad(quad(
                Bounds::from_corners(
                    point(a.x.min(b.x), a.y.min(b.y)),
                    point(a.x.max(b.x), a.y.max(b.y)),
                ),
                px(0.),
                blue.opacity(0.08),
                px(1.),
                blue,
                BorderStyle::Solid,
            ));
        }
        for g in &s.guides {
            let (a, b) = if g.vertical {
                (Vec2::new(g.pos, g.from), Vec2::new(g.pos, g.to))
            } else {
                (Vec2::new(g.from, g.pos), Vec2::new(g.to, g.pos))
            };
            let (a, b) = (to_s(a), to_s(b));
            let bounds = if g.vertical {
                Bounds {
                    origin: point(a.x.round() - px(0.5), a.y.min(b.y)),
                    size: size(px(1.), (b.y - a.y).abs().max(px(1.))),
                }
            } else {
                Bounds {
                    origin: point(a.x.min(b.x), a.y.round() - px(0.5)),
                    size: size((b.x - a.x).abs().max(px(1.)), px(1.)),
                }
            };
            window.paint_quad(fill(bounds, rgb_u(SNAP_RED)));
        }
    }
}

/// Initial mirroring of a dragged handle: symmetric handles stay symmetric,
/// smooth ones stay collinear; Alt breaks, Ctrl/Cmd forces symmetry.
fn handle_mirror(a: &Anchor, m: Modifiers) -> Mirror {
    if m.alt {
        Mirror::None
    } else if m.control || m.platform || a.is_symmetric() {
        Mirror::Full
    } else if a.is_smooth() {
        Mirror::Angle
    } else {
        Mirror::None
    }
}

/// Anchor / preview dot: a circle of diameter `d` with a 1 px border.
fn paint_dot(window: &mut Window, c: Point<Pixels>, d: f32, fill: Hsla, border: Hsla) {
    let r = px(d / 2.);
    window.paint_quad(quad(
        Bounds {
            origin: point(c.x - r, c.y - r),
            size: size(px(d), px(d)),
        },
        r,
        fill,
        px(1.),
        border,
        BorderStyle::Solid,
    ));
}

/// Handle knob: a square of side `d / sqrt 2` rotated 45° (Pen).
fn paint_diamond(window: &mut Window, c: Point<Pixels>, d: f32, fill: Hsla, border: Hsla) {
    let h = px(d / 2.);
    let pts = [
        point(c.x, c.y - h),
        point(c.x + h, c.y),
        point(c.x, c.y + h),
        point(c.x - h, c.y),
    ];
    let mut pb = PathBuilder::fill();
    pb.add_polygon(&pts, true);
    if let Ok(p) = pb.build() {
        window.paint_path(p, fill);
    }
    let mut pb = PathBuilder::stroke(px(1.));
    pb.add_polygon(&pts, true);
    if let Ok(p) = pb.build() {
        window.paint_path(p, border);
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: `gpui_kit::*` would shadow `#[test]` with GPUI's.
    use super::{
        Anchor, Bind, Contour, DocOp, KindData, Mirror, SceneDocument, Vec2, VectorPath,
        create_path, geometry_to_world, handle_mirror, parse_svg_path, path_geometry, ps,
        write_path,
    };
    use gpui_kit::Modifiers;
    use std::sync::Arc;

    fn doc() -> SceneDocument {
        let fonts: Vec<Arc<[u8]>> = Vec::new();
        SceneDocument::empty(&fonts)
    }

    fn world_path(pts: &[(f32, f32)]) -> VectorPath {
        let mut p = VectorPath::default();
        p.contours.push(Contour::new(
            pts.iter()
                .map(|&(x, y)| Anchor::corner(Vec2::new(x, y)))
                .collect(),
            false,
        ));
        p
    }

    #[test]
    fn created_path_is_normalized_like_pen() {
        let mut d = doc();
        let k = create_path(
            &mut d,
            None,
            &world_path(&[(100.0, 50.0), (60.0, 80.0), (140.0, 90.0)]),
        )
        .unwrap();
        d.flush();
        let n = d.get(k).unwrap();
        let KindData::Path(spec) = &n.data else {
            panic!()
        };
        // First point at the geometry origin; the box is the tight bounds.
        assert_eq!(&*spec.geometry, "M0 0 L-40 30 L40 40");
        let l = d.pipe.layout.get(k).unwrap();
        assert_eq!((l.x, l.y, l.w, l.h), (60.0, 50.0, 80.0, 40.0));
        let json = d.save_string();
        assert!(
            json.contains("#000000") && json.contains("\"path\""),
            "{json}"
        );
        // Geometry maps back onto the world points.
        let g = parse_svg_path(&spec.geometry).bounds;
        let m = geometry_to_world(&d, k, g).unwrap();
        assert!(
            m.transform_point2(Vec2::ZERO)
                .distance(Vec2::new(100.0, 50.0))
                < 1e-4
        );
    }

    #[test]
    fn writing_keeps_the_drawing_in_place() {
        let mut d = doc();
        let k = create_path(&mut d, None, &world_path(&[(10.0, 10.0), (50.0, 10.0)])).unwrap();
        d.flush();
        let g0 = path_geometry(&d, k).unwrap();
        let mut p = VectorPath::parse(&g0);
        // Drag the second point down-left of the first one.
        p.contours[0].anchors[1].p = Vec2::new(-20.0, 30.0);
        d.begin();
        assert!(write_path(&mut d, k, &p));
        d.commit();
        d.flush();
        let g = path_geometry(&d, k).unwrap();
        assert_eq!(&*g, "M0 0 L-20 30");
        let l = d.pipe.layout.get(k).unwrap();
        assert_eq!((l.x, l.y, l.w, l.h), (-10.0, 10.0, 20.0, 30.0));
        let m = geometry_to_world(&d, k, parse_svg_path(&g).bounds).unwrap();
        // The untouched first point did not move on the canvas.
        assert!(
            m.transform_point2(Vec2::ZERO)
                .distance(Vec2::new(10.0, 10.0))
                < 1e-3
        );
        assert!(
            m.transform_point2(Vec2::new(-20.0, 30.0))
                .distance(Vec2::new(-10.0, 40.0))
                < 1e-3
        );
        // Same geometry: no op, no undo step.
        assert!(!write_path(&mut d, k, &p));
        // One undo step restores the original.
        assert!(d.undo());
        d.flush();
        assert_eq!(path_geometry(&d, k).unwrap(), g0);
    }

    #[test]
    fn rotated_path_stays_in_place() {
        let mut d = doc();
        let k = create_path(&mut d, None, &world_path(&[(0.0, 0.0), (40.0, 20.0)])).unwrap();
        d.flush();
        let t = d.scene.node_ref(k).unwrap();
        let patch = ps::Node {
            rotation: Some(Bind::Value(30.0)),
            ..Default::default()
        };
        d.apply_op(DocOp::set(t, patch));
        d.flush();
        let g = path_geometry(&d, k).unwrap();
        let before = geometry_to_world(&d, k, parse_svg_path(&g).bounds).unwrap();
        let mut p = VectorPath::parse(&g);
        p.contours[0].anchors[1].p = Vec2::new(-30.0, -10.0);
        assert!(write_path(&mut d, k, &p));
        d.flush();
        let g2 = path_geometry(&d, k).unwrap();
        let after = geometry_to_world(&d, k, parse_svg_path(&g2).bounds).unwrap();
        for q in [Vec2::ZERO, Vec2::new(-30.0, -10.0)] {
            assert!(
                before
                    .transform_point2(q)
                    .distance(after.transform_point2(q))
                    < 2e-3,
                "{q}"
            );
        }
    }

    #[test]
    fn mirror_from_modifiers() {
        let mut a = Anchor::corner(Vec2::ZERO);
        a.handle_in = Some(Vec2::new(-5.0, 0.0));
        a.handle_out = Some(Vec2::new(5.0, 0.0));
        let none = Modifiers::default();
        assert_eq!(handle_mirror(&a, none), Mirror::Full);
        a.handle_out = Some(Vec2::new(9.0, 0.0));
        assert_eq!(handle_mirror(&a, none), Mirror::Angle);
        let alt = Modifiers {
            alt: true,
            ..Default::default()
        };
        assert_eq!(handle_mirror(&a, alt), Mirror::None);
    }
}
