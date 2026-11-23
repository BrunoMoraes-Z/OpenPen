//! Canvas host: pen-canvas's `DesignCanvas` plus the editing layer on top of
//! it (hit-testing, selection, hover, marquee, move/resize/rotate gestures
//! with snapping, creation tools, inline text editing) and the overlay
//! painting (selection box, handles, snap guides, frame names, size label).
//!
//! Gestures run through `EditorState::begin_gesture` / `end_gesture`, so each
//! drag is one undo step; every pointer move applies absolute values and runs
//! the incremental pipeline update.

mod gradient;
mod vector;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use pen_canvas::DesignCanvas;
use pen_doc::{Kind, NodeFlags, NodeKey};
use pen_render::{Affine2, Rect, Vec2};

use crate::actions;
use crate::editor_state::{ContextMenuReq, EditorState};
use crate::fonts;
use crate::geom::{
    self, BoxGeom, Guide, Handle, Obb, ResizeMods, SNAP_THRESHOLD_PX, rect_from_points, resize,
    rotate_around_center, round_box, snap_rect, snap_resize, snap_spacing,
};
use crate::layers::model::LayerId;
use crate::overlay::{self, Measure};
use crate::scene_document::{NewKind, id_of, key_of};
use crate::select::{self, PickTree};
use crate::theme::PaletteExt;
use crate::tools::Tool;
use crate::ui::drag;

/// Pen's fixed canvas colors (docs/ui-spec.md §2.3).
const SELECTION: u32 = 0x3d99ff;
const INSTANCE: u32 = 0x9580ff;
const COMPONENT: u32 = 0xd480ff;
const SNAP: u32 = 0xf24518;
const FRAME_LABEL: u32 = 0xaaaaaa;

/// Screen distances (px).
const HANDLE: f32 = 7.0;
const HANDLE_HIT: f32 = 6.0;
const ROTATE_ZONE: f32 = 18.0;
const DRAG_START: f32 = 3.0;
const HIT_SLOP: f32 = 3.0;

struct MoveItem {
    k: NodeKey,
    g0: BoxGeom,
    /// World → parent-space linear part.
    inv: glam::Mat2,
}

struct ResizeItem {
    k: NodeKey,
    g0: BoxGeom,
    parent_world: Affine2,
    aabb0: Rect,
}

enum Gesture {
    None,
    /// Button down on a node; becomes a move after a few pixels.
    Press {
        start: Vec2,
        screen: Point<Pixels>,
        target: NodeKey,
        reselect: bool,
    },
    Move {
        start: Vec2,
        items: Vec<MoveItem>,
        union0: Rect,
        targets: Vec<Rect>,
        guides: Vec<Guide>,
        /// Equal-spacing gaps shown while snapped to them.
        gaps: Vec<Measure>,
    },
    Resize {
        handle: Handle,
        items: Vec<ResizeItem>,
        union0: Rect,
        targets: Vec<Rect>,
        guides: Vec<Guide>,
    },
    Rotate {
        start: Vec2,
        item: MoveItem,
        parent_world: Affine2,
    },
    Marquee {
        start: Vec2,
        cur: Vec2,
        base: Vec<LayerId>,
        deep: bool,
    },
    Create {
        kind: NewKind,
        start: Vec2,
        cur: Vec2,
        square: bool,
    },
    Pan {
        last: Point<Pixels>,
    },
    /// Pen tool / vector edit mode (`vector.rs`).
    Vector(Box<vector::Drag>),
}

struct TextEdit {
    /// Edited node (`None` = a new text at `origin`).
    node: Option<NodeKey>,
    origin: Vec2,
    /// New text: fixed width (drag-created) or auto width (`None`).
    width: Option<f32>,
    font_size: f32,
    input: Entity<InputState>,
    _sub: Subscription,
}

pub struct CanvasHost {
    state: Entity<EditorState>,
    canvas: Entity<DesignCanvas>,
    shown_rev: u64,
    /// Zoom last exchanged with `EditorState` (detects who changed it).
    synced_zoom: f32,
    gesture: Gesture,
    hover: Option<NodeKey>,
    hover_handle: Option<CursorStyle>,
    labels: Vec<(Bounds<Pixels>, NodeKey)>,
    text_edit: Option<TextEdit>,
    dark: bool,
    /// Alt held: distance measurements to the hovered node.
    alt: bool,
    /// Vector edit mode / pen tool session.
    vector: Option<vector::Session>,
    /// Drag of a gradient handle / stop (see `gradient.rs`).
    grad: Option<gradient::GradDrag>,
    _subs: Vec<Subscription>,
}

fn rgb_u(c: u32) -> Hsla {
    rgb(c).into()
}

fn node_color(n: Option<&pen_doc::SceneNode>) -> Hsla {
    match n {
        Some(n) if n.flags.has(NodeFlags::REUSABLE) => rgb_u(COMPONENT),
        Some(n) if n.flags.has(NodeFlags::INSTANCE) => rgb_u(INSTANCE),
        _ => rgb_u(SELECTION),
    }
}

fn handle_cursor(h: Handle) -> CursorStyle {
    match h {
        Handle::Top | Handle::Bottom => CursorStyle::ResizeUpDown,
        Handle::Left | Handle::Right => CursorStyle::ResizeLeftRight,
        Handle::TopLeft | Handle::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        Handle::TopRight | Handle::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

fn fmt_num(v: f32) -> String {
    crate::inspector::value::format_number(v)
}

impl CanvasHost {
    pub fn new(state: Entity<EditorState>, cx: &mut Context<Self>) -> Self {
        let canvas = cx.new(|cx| {
            let mut c = DesignCanvas::new(cx);
            c.options_mut(cx).builtin_shortcuts = false;
            c
        });
        let subs = vec![
            cx.observe(&state, |this, _, cx| this.sync_from_state(cx)),
            cx.observe(&canvas, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            state,
            canvas,
            shown_rev: 0,
            synced_zoom: 1.0,
            gesture: Gesture::None,
            hover: None,
            hover_handle: None,
            labels: Vec::new(),
            text_edit: None,
            dark: false,
            alt: false,
            vector: None,
            grad: None,
            _subs: subs,
        };
        this.sync_from_state(cx);
        this
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.canvas.read(cx).focus_handle(cx)
    }

    pub fn canvas(&self) -> &Entity<DesignCanvas> {
        &self.canvas
    }

    /// Pushes document changes / UI zoom requests to the canvas.
    fn sync_from_state(&mut self, cx: &mut Context<Self>) {
        let s = self.state.read(cx);
        let rev = s.doc.revision();
        let list = (rev != self.shown_rev).then(|| s.doc.display_list().clone());
        let fit = s.fit_requested && !s.loading;
        let fit_sel = s.fit_selection_requested;
        let sel_rect = if fit_sel {
            self.selection_aabb(cx)
        } else {
            None
        };
        let z = s.zoom;
        if let Some(list) = list {
            self.shown_rev = rev;
            self.canvas.update(cx, |c, cx| c.set_display_list(list, cx));
        }
        if fit || fit_sel {
            self.state.update(cx, |s, _| {
                s.fit_requested = false;
                s.fit_selection_requested = false;
            });
        }
        let insets = self.state.read(cx).view_insets;
        if fit {
            let all = self.canvas.read(cx).display_list().bounds();
            if let Some(r) = all {
                self.fit_rect(r, insets, 1.0, cx);
            }
        } else if let Some(r) = sel_rect {
            self.fit_rect(r, insets, 4.0, cx);
        } else if (z - self.synced_zoom).abs() > 1e-5 {
            // Zoom changed from the UI (zoom control / shortcuts).
            self.synced_zoom = z;
            self.canvas.update(cx, |c, cx| c.zoom_to(z, None, cx));
        }
        self.vector_sync(cx);
        cx.notify();
    }

    /// Fits `r` into the canvas area not covered by the panels.
    fn fit_rect(&mut self, r: Rect, insets: [f32; 4], max_zoom: f32, cx: &mut Context<Self>) {
        let size = self.canvas.read(cx).canvas_size();
        let [t, rt, b, l] = insets;
        let free = Vec2::new(size.x - l - rt, size.y - t - b);
        if free.x < 64.0 || free.y < 64.0 {
            self.canvas.update(cx, |c, cx| c.zoom_to_rect(r, cx));
            return;
        }
        let mut vp = pen_canvas::Viewport::fit(&r, free, 48.0, max_zoom);
        vp.offset += Vec2::new(l, t);
        self.canvas.update(cx, |c, cx| c.set_viewport(vp, cx));
    }

    /// Visible part of the document (world space).
    pub fn visible_world(&self, cx: &App) -> Rect {
        let c = self.canvas.read(cx);
        let b = c.canvas_bounds();
        c.viewport()
            .visible_doc_rect(Vec2::new(f32::from(b.size.width), f32::from(b.size.height)))
    }

    fn selection_aabb(&self, cx: &App) -> Option<Rect> {
        let s = self.state.read(cx);
        geom::union_all(
            s.selection
                .iter()
                .filter_map(|&id| s.doc.world_aabb(key_of(id))),
        )
    }

    /// Selection box in world space: the node's box for one node, the union
    /// of AABBs for several.
    fn selection_box(&self, cx: &App) -> Option<(Obb, bool)> {
        let s = self.state.read(cx);
        match s.selection.as_slice() {
            [] => None,
            [one] => s.doc.world_obb(key_of(*one)).map(|o| (o, true)),
            many => geom::union_all(many.iter().filter_map(|&id| s.doc.world_aabb(key_of(id))))
                .map(|r| (Obb::from_rect(r), false)),
        }
    }

    fn zoom(&self, cx: &App) -> f32 {
        self.canvas.read(cx).zoom()
    }

    fn to_doc(&self, p: Point<Pixels>, cx: &App) -> Vec2 {
        self.canvas.read(cx).screen_to_doc(p)
    }

    fn to_screen(&self, p: Vec2, cx: &App) -> Vec2 {
        let s = self.canvas.read(cx).doc_to_screen(p);
        Vec2::new(f32::from(s.x), f32::from(s.y))
    }

    /// Handle (or rotation zone) of the selection box under screen point `sp`.
    fn handle_at(&self, sp: Vec2, cx: &App) -> Option<(Handle, bool)> {
        let (obb, single) = self.selection_box(cx)?;
        let sw = (obb.aabb().w * self.zoom(cx)).max(obb.w * self.zoom(cx));
        let sh = (obb.aabb().h * self.zoom(cx)).max(obb.h * self.zoom(cx));
        let tiny = sw < 16.0 || sh < 16.0;
        for h in Handle::ALL {
            if tiny && !h.is_corner() {
                continue;
            }
            let (u, v) = h.uv();
            let hp = self.to_screen(obb.at(u, v), cx);
            let d = (hp - sp).abs();
            if d.x <= HANDLE_HIT && d.y <= HANDLE_HIT {
                return Some((h, false));
            }
        }
        if single {
            // Rotation zone: just outside a corner.
            let center = self.to_screen(obb.at(0.5, 0.5), cx);
            for h in [
                Handle::TopLeft,
                Handle::TopRight,
                Handle::BottomRight,
                Handle::BottomLeft,
            ] {
                let (u, v) = h.uv();
                let hp = self.to_screen(obb.at(u, v), cx);
                let d = hp.distance(sp);
                let outside = sp.distance(center) > hp.distance(center);
                if d > HANDLE_HIT && d <= ROTATE_ZONE && outside {
                    return Some((h, true));
                }
            }
        }
        None
    }

    /// A pointer gesture is in progress (between button down and up).
    fn gesture_active(&self) -> bool {
        !matches!(self.gesture, Gesture::None)
    }

    /// Enter on a single selected path: vector edit mode.
    fn enter_selected_path(&mut self, cx: &mut Context<Self>) -> bool {
        match self.state.read(cx).selection.as_slice() {
            [one] => self.enter_vector(key_of(*one), cx),
            _ => false,
        }
    }

    /// Arrow keys: nudge the selected points in vector edit mode.
    fn nudge_points(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        if !self.vector_nudge(dx, dy, cx) {
            cx.propagate();
        }
    }

    fn label_at(&self, p: Point<Pixels>) -> Option<NodeKey> {
        self.labels
            .iter()
            .rev()
            .find(|(b, _)| b.contains(&p))
            .map(|l| l.1)
    }

    // ---------------------------------------------------------------- input

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_edit.is_some() {
            self.commit_text(window, cx);
        }
        let focus = self.focus_handle(cx);
        window.focus(&focus, cx);
        let p = self.to_doc(ev.position, cx);
        let sp = Vec2::new(f32::from(ev.position.x), f32::from(ev.position.y));
        let tool = self.state.read(cx).tool;
        let m = ev.modifiers;
        let deep = m.secondary(cx);
        if self.gradient_down(ev, cx) {
            return;
        }
        if self.vector_down(ev, p, window, cx) {
            return;
        }
        match tool {
            Tool::Hand => {
                self.gesture = Gesture::Pan { last: ev.position };
            }
            Tool::Rectangle | Tool::Ellipse | Tool::Frame => {
                let kind = match tool {
                    Tool::Rectangle => NewKind::Rectangle,
                    Tool::Ellipse => NewKind::Ellipse,
                    _ => NewKind::Frame,
                };
                self.gesture = Gesture::Create {
                    kind,
                    start: p,
                    cur: p,
                    square: m.shift,
                };
            }
            Tool::Text => {
                self.gesture = Gesture::Create {
                    kind: NewKind::Text,
                    start: p,
                    cur: p,
                    square: false,
                };
            }
            _ => self.on_select_down(ev, p, sp, deep, window, cx),
        }
        cx.notify();
    }

    fn on_select_down(
        &mut self,
        ev: &MouseDownEvent,
        p: Vec2,
        sp: Vec2,
        deep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shift = ev.modifiers.shift;
        // 1. Handles / rotation zone of the current selection.
        // (Hidden while a gradient is edited, like Pen.)
        if ev.click_count < 2
            && !self.gradient_active(cx)
            && let Some((h, rotate)) = self.handle_at(sp, cx)
        {
            self.start_transform(h, rotate, p, cx);
            return;
        }
        // 2. Frame name labels select the frame.
        let zoom = self.zoom(cx);
        let target = if let Some(k) = self.label_at(ev.position) {
            Some(k)
        } else {
            let sel: Vec<NodeKey> = self
                .state
                .read(cx)
                .selection
                .iter()
                .map(|&i| key_of(i))
                .collect();
            self.state.update(cx, |s, _| {
                let path = s.doc.hit_path(p, HIT_SLOP / zoom);
                let tree = &s.doc as &dyn PickTree<NodeKey>;
                if ev.click_count >= 2 {
                    select::pick_enter(tree, &path, &sel)
                } else {
                    select::pick(tree, &path, &sel, deep)
                }
            })
        };
        // Double click on a text node: edit it; on a path: edit its points.
        if ev.click_count >= 2
            && let Some(k) = target
        {
            let s = self.state.read(cx);
            let was_selected = s.selection.contains(&id_of(k));
            if (was_selected || s.selection.len() <= 1) && self.enter_vector(k, cx) {
                return;
            }
            let s = self.state.read(cx);
            if let Some(t) = s.doc.get(k).and_then(|n| n.text())
                && (was_selected || s.selection.len() <= 1)
            {
                let content = t.content.to_string();
                let size = t.size;
                if let Some(o) = s.doc.world_obb(k) {
                    self.state
                        .update(cx, |s, cx| s.set_selection(vec![id_of(k)], cx));
                    self.start_text_edit(Some(k), o.at(0.0, 0.0), None, size, &content, window, cx);
                    return;
                }
            }
        }
        match target {
            Some(k) => {
                let cur = self.state.read(cx).selection.clone();
                let was = cur.contains(&id_of(k));
                let next = select::click_select(&cur, id_of(k), shift);
                let selected_now = next.contains(&id_of(k));
                self.state.update(cx, |s, cx| s.set_selection(next, cx));
                if selected_now {
                    self.gesture = Gesture::Press {
                        start: p,
                        screen: ev.position,
                        target: k,
                        reselect: was && !shift && cur.len() > 1,
                    };
                }
            }
            None => {
                let base = if shift {
                    self.state.read(cx).selection.clone()
                } else {
                    self.state.update(cx, |s, cx| s.clear_selection(cx));
                    Vec::new()
                };
                self.gesture = Gesture::Marquee {
                    start: p,
                    cur: p,
                    base,
                    deep,
                };
            }
        }
    }

    fn start_transform(&mut self, h: Handle, rotate: bool, p: Vec2, cx: &mut Context<Self>) {
        let s = self.state.read(cx);
        let keys: Vec<NodeKey> = select::top_level_only(
            &s.doc as &dyn PickTree<NodeKey>,
            &s.selection.iter().map(|&i| key_of(i)).collect::<Vec<_>>(),
        );
        if rotate {
            let Some(&k) = keys.first() else { return };
            let (Some(g0), pw) = (s.doc.box_geom(k), s.doc.parent_world(k)) else {
                return;
            };
            self.gesture = Gesture::Rotate {
                start: p,
                item: MoveItem {
                    k,
                    g0,
                    inv: pw.matrix2.inverse(),
                },
                parent_world: pw,
            };
        } else {
            let items: Vec<ResizeItem> = keys
                .iter()
                .filter_map(|&k| {
                    Some(ResizeItem {
                        k,
                        g0: s.doc.box_geom(k)?,
                        parent_world: s.doc.parent_world(k),
                        aabb0: s.doc.world_aabb(k)?,
                    })
                })
                .collect();
            let Some(union0) = geom::union_all(items.iter().map(|i| i.aabb0)) else {
                return;
            };
            let targets = s.doc.snap_targets(&keys);
            self.gesture = Gesture::Resize {
                handle: h,
                items,
                union0,
                targets,
                guides: Vec::new(),
            };
        }
        self.state.update(cx, |s, _| s.begin_gesture());
    }

    fn on_drag_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = self.to_doc(ev.position, cx);
        let m = ev.modifiers;
        let zoom = self.zoom(cx);
        // Press -> Move once the pointer travelled far enough.
        if let Gesture::Press {
            start,
            screen,
            target,
            ..
        } = &self.gesture
        {
            let d = ev.position - *screen;
            if f32::from(d.x).abs().max(f32::from(d.y).abs()) < DRAG_START {
                return;
            }
            let (start, target) = (*start, *target);
            let s = self.state.read(cx);
            let mut keys: Vec<NodeKey> = select::top_level_only(
                &s.doc as &dyn PickTree<NodeKey>,
                &s.selection.iter().map(|&i| key_of(i)).collect::<Vec<_>>(),
            );
            if keys.is_empty() {
                keys.push(target);
            }
            let items: Vec<MoveItem> = keys
                .iter()
                .filter(|&&k| !s.doc.in_flex(k))
                .filter_map(|&k| {
                    Some(MoveItem {
                        k,
                        g0: s.doc.box_geom(k)?,
                        inv: s.doc.parent_world(k).matrix2.inverse(),
                    })
                })
                .collect();
            let union0 = geom::union_all(keys.iter().filter_map(|&k| s.doc.world_aabb(k)))
                .unwrap_or_default();
            let targets = s.doc.snap_targets(&keys);
            self.gesture = Gesture::Move {
                start,
                items,
                union0,
                targets,
                guides: Vec::new(),
                gaps: Vec::new(),
            };
            self.state.update(cx, |s, _| s.begin_gesture());
        }
        match &mut self.gesture {
            Gesture::Move {
                start,
                items,
                union0,
                targets,
                guides,
                gaps,
            } => {
                let mut d = p - *start;
                if m.shift {
                    // Constrain to the dominant axis.
                    if d.x.abs() > d.y.abs() {
                        d.y = 0.0;
                    } else {
                        d.x = 0.0;
                    }
                }
                let moved = Rect::new(union0.x + d.x, union0.y + d.y, union0.w, union0.h);
                let snap = if m.control || m.platform {
                    geom::Snap::default()
                } else {
                    snap_rect(&moved, targets, SNAP_THRESHOLD_PX / zoom)
                };
                d += Vec2::new(snap.dx, snap.dy);
                *guides = snap.guides;
                gaps.clear();
                if !(m.control || m.platform) {
                    // Equal spacing on the axes without an edge/center snap.
                    let moved = Rect::new(union0.x + d.x, union0.y + d.y, union0.w, union0.h);
                    let (sd, segs) = snap_spacing(&moved, targets, SNAP_THRESHOLD_PX / zoom);
                    let free_x = !guides.iter().any(|g| g.vertical);
                    let free_y = !guides.iter().any(|g| !g.vertical);
                    let ax = if free_x { sd.x } else { 0.0 };
                    let ay = if free_y { sd.y } else { 0.0 };
                    d += Vec2::new(ax, ay);
                    *gaps = segs
                        .into_iter()
                        .filter(|g| if g.a.y == g.b.y { free_x } else { free_y })
                        .map(|g| {
                            // Segments follow the snapped box on the other axis.
                            let off = if g.a.y == g.b.y {
                                Vec2::new(0.0, ay)
                            } else {
                                Vec2::new(ax, 0.0)
                            };
                            Measure {
                                a: g.a + off,
                                b: g.b + off,
                                ..g
                            }
                        })
                        .collect();
                }
                let edits: Vec<(NodeKey, BoxGeom, BoxGeom)> = items
                    .iter()
                    .map(|it| {
                        let ld = it.inv * d;
                        let mut g = BoxGeom {
                            x: it.g0.x + ld.x,
                            y: it.g0.y + ld.y,
                            ..it.g0
                        };
                        if g.rotation == 0.0 {
                            g.x = g.x.round();
                            g.y = g.y.round();
                        }
                        (it.k, it.g0, g)
                    })
                    .collect();
                self.state.update(cx, |s, cx| {
                    for (k, g0, g) in &edits {
                        s.doc.set_box(*k, g0, g);
                    }
                    s.changed(cx);
                });
            }
            Gesture::Resize {
                handle,
                items,
                union0,
                targets,
                guides,
            } => {
                let mods = ResizeMods {
                    shift: m.shift,
                    alt: m.alt,
                };
                let snapping = !(m.control || m.platform);
                guides.clear();
                let edits: Vec<(NodeKey, BoxGeom, BoxGeom)> = if items.len() == 1 {
                    let it = &items[0];
                    let mx = it.parent_world.matrix2;
                    let axis_aligned = mx.x_axis.y.abs() < 1e-6 && mx.y_axis.x.abs() < 1e-6;
                    let pp = it.parent_world.inverse().transform_point2(p);
                    let mut g = resize(&it.g0, *handle, pp, mods);
                    if snapping && axis_aligned && it.g0.rotation == 0.0 {
                        let r = Rect::new(g.x, g.y, g.w, g.h).transformed(&it.parent_world);
                        let sn = snap_resize(&r, *handle, p, targets, SNAP_THRESHOLD_PX / zoom);
                        if sn.dx != 0.0 || sn.dy != 0.0 {
                            let wp = p + Vec2::new(sn.dx, sn.dy);
                            let pp = it.parent_world.inverse().transform_point2(wp);
                            g = resize(&it.g0, *handle, pp, mods);
                        }
                        *guides = sn.guides;
                    }
                    vec![(it.k, it.g0, round_box(g))]
                } else {
                    let u0 = BoxGeom {
                        x: union0.x,
                        y: union0.y,
                        w: union0.w,
                        h: union0.h,
                        rotation: 0.0,
                    };
                    let mut u = resize(&u0, *handle, p, mods);
                    if snapping {
                        let r = Rect::new(u.x, u.y, u.w, u.h);
                        let sn = snap_resize(&r, *handle, p, targets, SNAP_THRESHOLD_PX / zoom);
                        if sn.dx != 0.0 || sn.dy != 0.0 {
                            u = resize(&u0, *handle, p + Vec2::new(sn.dx, sn.dy), mods);
                        }
                        *guides = sn.guides;
                    }
                    let sx = if u0.w > 0.0 { u.w / u0.w } else { 1.0 };
                    let sy = if u0.h > 0.0 { u.h / u0.h } else { 1.0 };
                    items
                        .iter()
                        .map(|it| {
                            let a = it.aabb0;
                            let wo = Vec2::new(u.x + (a.x - u0.x) * sx, u.y + (a.y - u0.y) * sy);
                            let shift = wo - Vec2::new(a.x, a.y);
                            let ld = it.parent_world.matrix2.inverse() * shift;
                            let mut g = BoxGeom {
                                x: it.g0.x + ld.x,
                                y: it.g0.y + ld.y,
                                ..it.g0
                            };
                            if it.g0.rotation == 0.0 {
                                g.w = it.g0.w * sx;
                                g.h = it.g0.h * sy;
                            }
                            (it.k, it.g0, round_box(g))
                        })
                        .collect()
                };
                self.state.update(cx, |s, cx| {
                    for (k, g0, g) in &edits {
                        s.doc.set_box(*k, g0, g);
                    }
                    s.changed(cx);
                });
            }
            Gesture::Rotate {
                start,
                item,
                parent_world,
            } => {
                let inv = parent_world.inverse();
                let g = rotate_around_center(
                    &item.g0,
                    inv.transform_point2(*start),
                    inv.transform_point2(p),
                    m.shift,
                );
                let (k, g0) = (item.k, item.g0);
                self.state.update(cx, |s, cx| {
                    s.doc.set_box(k, &g0, &g);
                    s.changed(cx);
                });
            }
            Gesture::Marquee {
                start,
                cur,
                base,
                deep,
            } => {
                *cur = p;
                let r = rect_from_points(*start, p);
                let (base, deep) = (base.clone(), *deep);
                self.state.update(cx, |s, cx| {
                    let mut sel = base;
                    for k in s.doc.marquee(&r, deep) {
                        let id = id_of(k);
                        if !sel.contains(&id) {
                            sel.push(id);
                        }
                    }
                    s.set_selection(sel, cx);
                });
            }
            Gesture::Create { cur, square, .. } => {
                *cur = p;
                *square = m.shift;
            }
            Gesture::Pan { last } => {
                let d = ev.position - *last;
                *last = ev.position;
                let v = Vec2::new(f32::from(d.x), f32::from(d.y));
                self.canvas.update(cx, |c, cx| c.pan_by(v, cx));
            }
            Gesture::Vector(_) => self.vector_drag_move(ev, cx),
            Gesture::Press { .. } | Gesture::None => {}
        }
        cx.notify();
    }

    fn on_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let g = std::mem::replace(&mut self.gesture, Gesture::None);
        match g {
            Gesture::Press {
                target, reselect, ..
            } => {
                if reselect {
                    self.state
                        .update(cx, |s, cx| s.set_selection(vec![id_of(target)], cx));
                }
            }
            Gesture::Move { .. } | Gesture::Resize { .. } | Gesture::Rotate { .. } => {
                self.state.update(cx, |s, cx| s.end_gesture(cx));
            }
            Gesture::Create {
                kind: NewKind::Text,
                start,
                cur,
                ..
            } => {
                // Click: auto width; drag: fixed width (like the text tool of Pen).
                let zoom = self.zoom(cx);
                let w = (cur.x - start.x).abs();
                let width = (w * zoom >= 4.0).then_some(w.round().max(1.0));
                let origin = Vec2::new(start.x.min(cur.x), start.y.min(cur.y));
                self.start_text_edit(
                    None,
                    origin,
                    width,
                    crate::scene_document::TEXT_DEFAULT_SIZE as f32,
                    "",
                    window,
                    cx,
                );
            }
            Gesture::Create {
                kind,
                start,
                cur,
                square,
            } => {
                let zoom = self.zoom(cx);
                let mut r = rect_from_points(start, cur);
                if square {
                    let side = r.w.max(r.h);
                    r = rect_from_points(
                        start,
                        start
                            + Vec2::new(
                                side * (cur.x - start.x).signum(),
                                side * (cur.y - start.y).signum(),
                            ),
                    );
                }
                if r.w * zoom < 2.0 && r.h * zoom < 2.0 {
                    // Click: default 100x100 at the cursor.
                    r = Rect::new(start.x.round(), start.y.round(), 100.0, 100.0);
                } else {
                    let b = round_box(BoxGeom {
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: r.h,
                        rotation: 0.0,
                    });
                    r = Rect::new(b.x, b.y, b.w.max(1.0), b.h.max(1.0));
                }
                self.state.update(cx, |s, cx| {
                    let parent = s.doc.drop_parent(start);
                    s.doc.begin();
                    let made = s.doc.create(kind, r, parent, "");
                    s.doc.commit();
                    s.changed(cx);
                    if let Some(k) = made {
                        s.set_selection(vec![id_of(k)], cx);
                    }
                    s.set_tool(Tool::Move, cx);
                });
            }
            Gesture::Vector(d) => self.vector_up(*d, cx),
            Gesture::Marquee { .. } | Gesture::Pan { .. } | Gesture::None => {}
        }
        cx.notify();
    }

    /// Right button: select what is under the pointer (unless it is already
    /// selected) and open the context menu.
    fn on_right_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_edit.is_some() {
            self.commit_text(window, cx);
        }
        let focus = self.focus_handle(cx);
        window.focus(&focus, cx);
        let p = self.to_doc(ev.position, cx);
        let zoom = self.zoom(cx);
        let sel: Vec<NodeKey> = self
            .state
            .read(cx)
            .selection
            .iter()
            .map(|&i| key_of(i))
            .collect();
        let in_sel = self
            .selection_box(cx)
            .is_some_and(|(o, _)| o.contains(p, 0.0));
        let target = if let Some(k) = self.label_at(ev.position) {
            Some(k)
        } else if in_sel {
            sel.first().copied()
        } else {
            self.state.update(cx, |s, _| {
                let path = s.doc.hit_path(p, HIT_SLOP / zoom);
                select::pick(&s.doc as &dyn PickTree<NodeKey>, &path, &sel, false)
            })
        };
        let position = ev.position;
        self.state.update(cx, |s, cx| {
            match target {
                Some(k) if !s.selection.contains(&id_of(k)) => s.set_selection(vec![id_of(k)], cx),
                Some(_) => {}
                None => s.clear_selection(cx),
            }
            s.open_context_menu(
                ContextMenuReq {
                    position,
                    world: Some(p),
                },
                cx,
            );
        });
        cx.notify();
    }

    fn on_hover_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.gesture, Gesture::None) || ev.pressed_button.is_some() {
            return;
        }
        if let Some(c) = self.gradient_cursor(ev, cx) {
            if self.hover_handle != Some(c) || self.hover.is_some() {
                self.hover_handle = Some(c);
                self.hover = None;
                cx.notify();
            }
            return;
        }
        if self.vector.is_some() {
            self.vector_hover(ev, cx);
            return;
        }
        if self.alt != ev.modifiers.alt {
            self.alt = ev.modifiers.alt;
            cx.notify();
        }
        let sp = Vec2::new(f32::from(ev.position.x), f32::from(ev.position.y));
        let tool = self.state.read(cx).tool;
        let handle = if matches!(tool, Tool::Move | Tool::Scale) && !self.gradient_active(cx) {
            self.handle_at(sp, cx)
        } else {
            None
        };
        let cursor = handle.map(|(h, rot)| {
            if rot {
                CursorStyle::Crosshair
            } else {
                handle_cursor(h)
            }
        });
        let hover = if handle.is_some() || !matches!(tool, Tool::Move | Tool::Scale) {
            None
        } else if let Some(k) = self.label_at(ev.position) {
            Some(k)
        } else {
            let p = self.to_doc(ev.position, cx);
            let zoom = self.zoom(cx);
            let deep = ev.modifiers.secondary(cx);
            let sel: Vec<NodeKey> = self
                .state
                .read(cx)
                .selection
                .iter()
                .map(|&i| key_of(i))
                .collect();
            self.state.update(cx, |s, _| {
                let path = s.doc.hit_path(p, HIT_SLOP / zoom);
                select::pick(&s.doc as &dyn PickTree<NodeKey>, &path, &sel, deep)
            })
        };
        if hover != self.hover || cursor != self.hover_handle {
            self.hover = hover;
            self.hover_handle = cursor;
            cx.notify();
        }
    }

    // ----------------------------------------------------------- text edit

    #[allow(clippy::too_many_arguments)]
    fn start_text_edit(
        &mut self,
        node: Option<NodeKey>,
        origin: Vec2,
        width: Option<f32>,
        font_size: f32,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text.to_string()));
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_text(window, cx);
            }
        });
        self.text_edit = Some(TextEdit {
            node,
            origin,
            width,
            font_size,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    fn commit_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(te) = self.text_edit.take() else {
            return;
        };
        let text = te.input.read(cx).value().to_string();
        let dark = cx.pal().dark;
        self.state.update(cx, |s, cx| {
            match te.node {
                Some(k) => {
                    let old = s
                        .doc
                        .get(k)
                        .and_then(|n| n.text())
                        .map(|t| t.content.to_string());
                    if text.is_empty() {
                        s.doc.delete(&[k]);
                    } else if old.as_deref() != Some(text.as_str()) {
                        s.doc.begin();
                        s.doc.set_text(k, &text);
                        s.doc.commit();
                    }
                    s.changed(cx);
                }
                None if !text.is_empty() => {
                    let o = Vec2::new(te.origin.x.round(), te.origin.y.round());
                    let parent = s.doc.drop_parent(te.origin);
                    let fill = s.doc.contrast_text_fill(parent, dark);
                    s.doc.begin();
                    let made = s.doc.create_text(o, te.width, parent, &text, fill);
                    s.doc.commit();
                    s.changed(cx);
                    if let Some(k) = made {
                        s.set_selection(vec![id_of(k)], cx);
                    }
                }
                None => {}
            }
            s.set_tool(Tool::Move, cx);
        });
        let focus = self.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn render_text_edit(&self, cx: &App) -> Option<AnyElement> {
        let te = self.text_edit.as_ref()?;
        let c = self.canvas.read(cx);
        let o = c.doc_to_screen(te.origin);
        let b = c.canvas_bounds();
        let zoom = c.zoom();
        let size = (te.font_size * zoom).clamp(8.0, 200.0);
        let p = cx.pal();
        Some(
            div()
                .absolute()
                .left(o.x - b.origin.x - px(2.))
                .top(o.y - b.origin.y - px(2.))
                .map(|d| match te.width {
                    Some(w) => d.w(px(w * zoom + 4.)),
                    None => d.min_w(px(120.)),
                })
                .bg(p.island)
                .border_1()
                .border_color(rgb_u(SELECTION))
                .rounded(px(2.))
                .text_size(px(size))
                .child(Input::new(&te.input).appearance(false))
                .into_any_element(),
        )
    }

    // -------------------------------------------------------------- overlay

    fn paint_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (vp, vb) = {
            let c = self.canvas.read(cx);
            (c.viewport(), c.canvas_bounds())
        };
        let zoom = vp.zoom;
        // Canvas -> UI zoom sync (wheel, pinch, fit).
        if (zoom - self.synced_zoom).abs() > 1e-5 {
            self.synced_zoom = zoom;
            let st = self.state.clone();
            cx.defer(move |cx| st.update(cx, |s, cx| s.set_zoom(zoom, cx)));
        }
        let to_s = move |p: Vec2| -> Point<Pixels> {
            let s = vp.doc_to_screen(p);
            point(vb.origin.x + px(s.x), vb.origin.y + px(s.y))
        };
        let visible = vp.visible_doc_rect(Vec2::new(
            f32::from(vb.size.width),
            f32::from(vb.size.height),
        ));
        let family = fonts::overlay_family(cx);
        let sel_color = rgb_u(SELECTION);

        // Phase 1: collect what to draw (borrows the state).
        struct Label {
            text: SharedString,
            color: Hsla,
            tl: Point<Pixels>,
            max_w: Pixels,
            key: NodeKey,
        }
        let mut labels: Vec<Label> = Vec::new();
        let mut outlines: Vec<(Obb, Hsla, f32)> = Vec::new();
        let mut handles: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
        let mut size_label: Option<(SharedString, Hsla, Point<Pixels>)> = None;
        let mut measures: Vec<Measure> = Vec::new();
        let mut measure_target: Option<Rect> = None;
        {
            let s = self.state.read(cx);
            let doc = &s.doc;
            let label_view = Rect::new(
                visible.x,
                visible.y - 40.0 / zoom,
                visible.w,
                visible.h + 40.0 / zoom,
            );
            for &r in doc.scene.roots() {
                let Some(n) = doc.get(r) else { continue };
                if n.kind != Kind::Frame || !n.enabled() {
                    continue;
                }
                let Some(o) = doc.world_obb(r) else { continue };
                let a = o.aabb();
                if !a.intersects(&label_view) || a.w * zoom < 12.0 {
                    continue;
                }
                let text: SharedString = match &n.name {
                    Some(s) => s.to_string().into(),
                    None => "Frame".into(),
                };
                let special = n.flags.has(NodeFlags::REUSABLE) || n.flags.has(NodeFlags::INSTANCE);
                let hot = s.selection.contains(&id_of(r)) || self.hover == Some(r);
                labels.push(Label {
                    text,
                    color: if hot || special {
                        node_color(Some(n))
                    } else {
                        rgb_u(FRAME_LABEL)
                    },
                    tl: to_s(o.at(0.0, 0.0)),
                    max_w: px(a.w * zoom),
                    key: r,
                });
            }
            let hover = self
                .hover
                .or(s.hover.map(key_of))
                .filter(|h| !s.selection.contains(&id_of(*h)));
            // Alt over another node: distances from the selection (Pen).
            if self.alt
                && matches!(self.gesture, Gesture::None)
                && let Some(h) = hover
                && let Some(other) = doc.world_aabb(h)
                && let Some(sel) = geom::union_all(
                    s.selection
                        .iter()
                        .filter_map(|&i| doc.world_aabb(key_of(i))),
                )
            {
                measures = overlay::measure(&sel, &other);
                measure_target = Some(other);
            }
            if let Some(h) = hover
                && let Some(o) = doc.world_obb(h)
            {
                outlines.push((o, node_color(doc.get(h)), 1.5));
            }
            for &id in s.selection.iter().filter(|_| self.vector.is_none()) {
                if let Some(o) = doc.world_obb(key_of(id)) {
                    outlines.push((o, node_color(doc.get(key_of(id))), 1.0));
                }
            }
            let sel_box: Option<(Obb, bool)> = match s.selection.as_slice() {
                [] => None,
                [one] => doc.world_obb(key_of(*one)).map(|o| (o, true)),
                many => geom::union_all(many.iter().filter_map(|&i| doc.world_aabb(key_of(i))))
                    .map(|r| (Obb::from_rect(r), false)),
            };
            let creating = matches!(self.gesture, Gesture::Create { .. });
            if let Some((o, single)) = sel_box
                && !creating
                && self.text_edit.is_none()
                && self.vector.is_none()
            {
                let color = if single {
                    node_color(s.selection.first().and_then(|&i| doc.get(key_of(i))))
                } else {
                    sel_color
                };
                if !single {
                    outlines.push((o, color, 1.0));
                }
                let a = o.aabb();
                let tiny = a.w * zoom < 16.0 || a.h * zoom < 16.0;
                if !matches!(self.gesture, Gesture::Move { .. }) && !self.gradient_active(cx) {
                    for h in Handle::ALL {
                        if tiny && !h.is_corner() {
                            continue;
                        }
                        let (u, v) = h.uv();
                        let c = to_s(o.at(u, v));
                        handles.push((
                            Bounds {
                                origin: point(c.x - px(HANDLE / 2.), c.y - px(HANDLE / 2.)),
                                size: size(px(HANDLE), px(HANDLE)),
                            },
                            color,
                        ));
                    }
                }
                let (w, h) = if single { (o.w, o.h) } else { (a.w, a.h) };
                size_label = Some((
                    format!("{} x {}", fmt_num(w), fmt_num(h)).into(),
                    color,
                    to_s(Vec2::new(a.x + a.w / 2., a.bottom())),
                ));
            }
        }

        // Agent flashes and cursors at this instant (pure timing state).
        let now = std::time::Instant::now();
        let (fx_rings, fx_cursors) = {
            let s = self.state.read(cx);
            if s.agent_fx.animating(now) {
                // Keep frames coming only while something moves or fades.
                window.request_animation_frame();
                crate::agent::paint::collect(&s.agent_fx, &s.doc, zoom, now)
            } else {
                (Vec::new(), Vec::new())
            }
        };

        // Phase 2: paint.
        window.with_content_mask(Some(ContentMask { bounds: vb }), |window| {
            let mut hit = Vec::with_capacity(labels.len());
            let mut f = font(family);
            f.weight = FontWeight::MEDIUM;
            let run_for = |len: usize, color: Hsla| TextRun {
                len,
                font: f.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            for l in labels {
                // Truncate to the frame's screen width with an ellipsis; hide
                // when too narrow (Pen).
                let fitted = {
                    let ts = window.text_system();
                    overlay::truncate_label(&l.text, f32::from(l.max_w), |t| {
                        let r = run_for(t.len(), l.color);
                        f32::from(
                            ts.shape_line(SharedString::from(t.to_string()), px(11.), &[r], None)
                                .width,
                        )
                    })
                };
                let Some(text) = fitted else { continue };
                let text = SharedString::from(text);
                let run = run_for(text.len(), l.color);
                let line = window.text_system().shape_line(text, px(11.), &[run], None);
                let lh = px(13.);
                let origin = point(l.tl.x, l.tl.y - lh - px(5.));
                let lb = Bounds {
                    origin,
                    size: size(line.width.min(l.max_w), lh),
                };
                let _ = line.paint(origin, lh, TextAlign::Left, None, window, cx);
                hit.push((lb, l.key));
            }
            self.labels = hit;
            for (o, color, w) in &outlines {
                paint_obb(window, o, &to_s, *color, *w);
            }
            for (b, color) in &handles {
                window.paint_quad(quad(
                    *b,
                    px(1.),
                    rgb_u(0xffffff),
                    px(1.),
                    *color,
                    BorderStyle::Solid,
                ));
            }
            if let Some((text, color, bottom)) = size_label {
                let run = TextRun {
                    len: text.len(),
                    font: font(family),
                    color: rgb_u(0xffffff),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(text, px(12.), &[run], None);
                let lw = line.width + px(8.);
                let lb = Bounds {
                    origin: point(bottom.x - lw / 2., bottom.y + px(6.)),
                    size: size(lw, px(18.)),
                };
                window.paint_quad(quad(lb, px(4.), color, px(0.), color, BorderStyle::Solid));
                let _ = line.paint(
                    point(lb.origin.x + px(4.), lb.origin.y + px(2.)),
                    px(14.),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            match &self.gesture {
                Gesture::Move { guides, .. } | Gesture::Resize { guides, .. } => {
                    let red = rgb_u(SNAP);
                    for g in guides {
                        let (a, b) = if g.vertical {
                            (Vec2::new(g.pos, g.from), Vec2::new(g.pos, g.to))
                        } else {
                            (Vec2::new(g.from, g.pos), Vec2::new(g.to, g.pos))
                        };
                        let (a, b) = (to_s(a), to_s(b));
                        let bounds = if g.vertical {
                            Bounds {
                                origin: point(a.x.round() - px(0.5), a.y.min(b.y)),
                                size: size(px(1.), (b.y - a.y).abs()),
                            }
                        } else {
                            Bounds {
                                origin: point(a.x.min(b.x), a.y.round() - px(0.5)),
                                size: size((b.x - a.x).abs(), px(1.)),
                            }
                        };
                        window.paint_quad(fill(bounds, red));
                    }
                }
                Gesture::Marquee { start, cur, .. } => {
                    let a = to_s(*start);
                    let b = to_s(*cur);
                    let bounds = Bounds::from_corners(
                        point(a.x.min(b.x), a.y.min(b.y)),
                        point(a.x.max(b.x), a.y.max(b.y)),
                    );
                    window.paint_quad(quad(
                        bounds,
                        px(0.),
                        sel_color.opacity(0.08),
                        px(1.),
                        sel_color,
                        BorderStyle::Solid,
                    ));
                }
                Gesture::Create {
                    start, cur, kind, ..
                } => {
                    let a = to_s(*start);
                    let b = to_s(*cur);
                    let bounds = Bounds::from_corners(
                        point(a.x.min(b.x), a.y.min(b.y)),
                        point(a.x.max(b.x), a.y.max(b.y)),
                    );
                    let r = if *kind == NewKind::Ellipse {
                        bounds.size.width.min(bounds.size.height) / 2.
                    } else {
                        px(0.)
                    };
                    window.paint_quad(quad(
                        bounds,
                        r,
                        transparent_black(),
                        px(1.),
                        sel_color,
                        BorderStyle::Solid,
                    ));
                }
                _ => {}
            }
            if let Gesture::Move { gaps, .. } = &self.gesture {
                for g in gaps {
                    paint_measure(window, cx, family, g, &to_s);
                }
            }
            if let Some(t) = measure_target {
                paint_obb(window, &Obb::from_rect(t), &to_s, rgb_u(SNAP), 1.0);
            }
            self.paint_vector(window, &to_s, cx);
            for m in &measures {
                paint_measure(window, cx, family, m, &to_s);
            }
            self.paint_gradient(window, &to_s, cx);
            crate::agent::paint::paint(window, cx, &fx_rings, &fx_cursors, &to_s, zoom, family);
        });
    }
}

/// A red distance line with its value label (Inter 11 medium, white on
/// `#f24518`, radius 3, padding 1/4: docs/ui-spec.md §3).
fn paint_measure(
    window: &mut Window,
    cx: &mut App,
    family: &'static str,
    m: &Measure,
    to_s: &dyn Fn(Vec2) -> Point<Pixels>,
) {
    let red = rgb_u(SNAP);
    let (a, b) = (to_s(m.a), to_s(m.b));
    let vertical = (a.x - b.x).abs() < px(0.5);
    let bounds = if vertical {
        Bounds {
            origin: point(a.x.round() - px(0.5), a.y.min(b.y)),
            size: size(px(1.), (b.y - a.y).abs()),
        }
    } else {
        Bounds {
            origin: point(a.x.min(b.x), a.y.round() - px(0.5)),
            size: size((b.x - a.x).abs(), px(1.)),
        }
    };
    window.paint_quad(fill(bounds, red));
    let text: SharedString = fmt_num(m.value).into();
    let mut f = font(family);
    f.weight = FontWeight::MEDIUM;
    let run = TextRun {
        len: text.len(),
        font: f,
        color: rgb_u(0xffffff),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(text, px(11.), &[run], None);
    let (lw, lh) = (line.width + px(8.), px(14.));
    let c = point((a.x + b.x) / 2., (a.y + b.y) / 2.);
    let origin = if vertical {
        point(c.x + px(4.), c.y - lh / 2.)
    } else {
        point(c.x - lw / 2., c.y + px(4.))
    };
    let lb = Bounds {
        origin,
        size: size(lw, lh),
    };
    window.paint_quad(quad(lb, px(3.), red, px(0.), red, BorderStyle::Solid));
    let _ = line.paint(
        point(origin.x + px(4.), origin.y + px(1.)),
        px(12.),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// Outline of an oriented box (quad when axis-aligned, path otherwise).
fn paint_obb(
    window: &mut Window,
    o: &Obb,
    to_s: &dyn Fn(Vec2) -> Point<Pixels>,
    color: Hsla,
    width: f32,
) {
    let m = o.t.matrix2;
    let axis_aligned = m.x_axis.y.abs() < 1e-6 && m.y_axis.x.abs() < 1e-6;
    if axis_aligned {
        let a = to_s(o.at(0.0, 0.0));
        let b = to_s(o.at(1.0, 1.0));
        let bounds = Bounds::from_corners(
            point(a.x.min(b.x), a.y.min(b.y)),
            point(a.x.max(b.x), a.y.max(b.y)),
        );
        window.paint_quad(quad(
            bounds,
            px(0.),
            transparent_black(),
            px(width),
            color,
            BorderStyle::Solid,
        ));
        return;
    }
    let mut pb = PathBuilder::stroke(px(width));
    let c = o.corners();
    pb.move_to(to_s(c[0]));
    for p in &c[1..] {
        pb.line_to(to_s(*p));
    }
    pb.close();
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

impl Render for CanvasHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.pal();
        if p.dark != self.dark {
            self.dark = p.dark;
            let bg = p.canvas;
            self.canvas
                .update(cx, |c, cx| c.options_mut(cx).background = bg);
        }
        let tool = self.state.read(cx).tool;
        let cursor = self.hover_handle.unwrap_or(match tool {
            Tool::Hand => CursorStyle::OpenHand,
            Tool::Move | Tool::Scale => CursorStyle::Arrow,
            Tool::Text => CursorStyle::IBeam,
            _ => CursorStyle::Crosshair,
        });
        let entity = cx.entity();
        let dragging = !matches!(self.gesture, Gesture::None);
        let drag_cursor = match &self.gesture {
            Gesture::Pan { .. } => CursorStyle::ClosedHand,
            Gesture::Resize { handle, .. } => handle_cursor(*handle),
            Gesture::Rotate { .. } => CursorStyle::Crosshair,
            _ => cursor,
        };
        let loading = self.state.read(cx).loading;
        div()
            .id("canvas-host")
            .relative()
            .size_full()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .on_modifiers_changed(cx.listener(|this, ev: &ModifiersChangedEvent, _, cx| {
                if this.alt != ev.modifiers.alt {
                    this.alt = ev.modifiers.alt;
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(Self::on_hover_move))
            .on_action(cx.listener(|t, _: &actions::Deselect, _, cx| {
                if !t.vector_escape(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|t, _: &actions::SelectChildren, _, cx| {
                if !t.vector_enter(cx) && !t.enter_selected_path(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|t, _: &actions::Delete, _, cx| {
                if !t.vector_delete(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|t, _: &actions::SelectAll, _, cx| {
                if !t.vector_select_all(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|t, _: &actions::Undo, _, cx| {
                // Undo with only the first point placed drops it (Pen).
                if t.vector.as_ref().is_some_and(|v| v.node.is_none()) {
                    t.vector = None;
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|t, _: &actions::NudgeLeft, _, cx| t.nudge_points(-1., 0., cx)))
            .on_action(cx.listener(|t, _: &actions::NudgeRight, _, cx| t.nudge_points(1., 0., cx)))
            .on_action(cx.listener(|t, _: &actions::NudgeUp, _, cx| t.nudge_points(0., -1., cx)))
            .on_action(cx.listener(|t, _: &actions::NudgeDown, _, cx| t.nudge_points(0., 1., cx)))
            .on_action(
                cx.listener(|t, _: &actions::NudgeLeftBig, _, cx| t.nudge_points(-10., 0., cx)),
            )
            .on_action(
                cx.listener(|t, _: &actions::NudgeRightBig, _, cx| t.nudge_points(10., 0., cx)),
            )
            .on_action(
                cx.listener(|t, _: &actions::NudgeUpBig, _, cx| t.nudge_points(0., -10., cx)),
            )
            .on_action(
                cx.listener(|t, _: &actions::NudgeDownBig, _, cx| t.nudge_points(0., 10., cx)),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.hover.is_some() {
                    this.hover = None;
                    cx.notify();
                }
            }))
            .child(self.canvas.clone())
            .child(
                div()
                    .id("canvas-overlay")
                    .absolute()
                    .inset_0()
                    .cursor(cursor)
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |_, (), window, cx| {
                                entity.update(cx, |h, cx| h.paint_overlay(window, cx));
                                // A release that arrives before the next frame
                                // (so before `drag::capture` exists) still ends
                                // the gesture: down + up in one frame used to
                                // leave a marquee stuck.
                                let e = entity.clone();
                                window.on_mouse_event(
                                    move |ev: &MouseUpEvent, phase, window, cx| {
                                        if phase == DispatchPhase::Bubble
                                            && ev.button == MouseButton::Left
                                        {
                                            e.update(cx, |h, cx| {
                                                if h.gesture_active() {
                                                    h.on_up(window, cx);
                                                }
                                            });
                                        }
                                    },
                                );
                            },
                        )
                        .size_full(),
                    ),
            )
            .when_some(self.render_text_edit(cx), |d, e| d.child(e))
            .when(loading, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(p.fg_muted)
                        .child("Loading…"),
                )
            })
            .when(self.grad.is_some(), |d| {
                d.child(drag::capture(
                    cx.entity(),
                    CursorStyle::ClosedHand,
                    Self::on_grad_move,
                    Self::on_grad_up,
                ))
            })
            .when(dragging, |d| {
                d.child(drag::capture(
                    cx.entity(),
                    drag_cursor,
                    Self::on_drag_move,
                    Self::on_up,
                ))
            })
    }
}
