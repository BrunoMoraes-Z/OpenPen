//! GPUI design canvas: draws a [`pen_render::DisplayList`] on the GPU with pan/zoom,
//! viewport culling, LOD and per-chunk caches.
//!
//! Embed [`DesignCanvas`] as a view (`cx.new(|cx| DesignCanvas::new(cx))`) and feed it with
//! [`DesignCanvas::set_display_list`]. Coordinates: "window" points are GPUI window
//! coordinates (logical pixels), "doc" points are design units.
//! See `docs/pen-canvas.md` for the capability matrix.

pub mod fallback;
pub mod geom;
pub mod lod;
pub mod order;
mod render;
pub mod viewport;

use std::sync::Arc;
use std::time::Instant;

use gpui_kit::{
    App, Bounds, Context, CursorStyle, DispatchPhase, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, PinchEvent, Pixels, Point, Render, Rgba,
    ScrollDelta, ScrollEvent, Styled, Window, canvas, div, fill, point, px, rgb,
};
use pen_render::{DisplayList, Rect, Vec2};

pub use lod::LodPolicy;
pub use render::FrameStats;
pub use viewport::{MAX_ZOOM, MIN_ZOOM, Viewport};

/// Visual / behavioral options.
#[derive(Debug, Clone)]
pub struct CanvasOptions {
    /// Canvas background (Pen light theme `--canvas-bg: #f6f6f6`).
    pub background: Hsla,
    pub show_pixel_grid: bool,
    /// Pixel grid appears at or above this zoom (800%).
    pub pixel_grid_min_zoom: f32,
    pub pixel_grid_color: Hsla,
    pub lod: LodPolicy,
    /// Padding (screen px) used by zoom-to-fit.
    pub fit_padding: f32,
    /// Handle Shift+1 (fit), Ctrl/Cmd+0 (100%), Ctrl/Cmd +/- (zoom steps) on key down.
    pub builtin_shortcuts: bool,
    /// Draw content GPUI cannot (blend modes, blurs, group opacity, shaped clips, path
    /// effects) from `pen-raster` textures rendered on the background executor.
    pub raster_fallback: bool,
    /// Memory budget of the fallback texture cache (LRU), in bytes.
    pub fallback_budget_bytes: usize,
    /// Fallback textures rendering at the same time.
    pub fallback_max_jobs: usize,
}

impl Default for CanvasOptions {
    fn default() -> Self {
        Self {
            background: rgb(0xf6f6f6).into(),
            show_pixel_grid: true,
            pixel_grid_min_zoom: 8.0,
            pixel_grid_color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.07,
            }
            .into(),
            lod: LodPolicy::default(),
            fit_padding: 48.0,
            builtin_shortcuts: true,
            raster_fallback: true,
            fallback_budget_bytes: 192 << 20,
            fallback_max_jobs: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PanDrag {
    last: Point<Pixels>,
}

/// The interactive design canvas view.
pub struct DesignCanvas {
    list: Arc<DisplayList>,
    viewport: Viewport,
    /// Element bounds in window coordinates from the last paint.
    bounds: Bounds<Pixels>,
    has_layout: bool,
    pending_fit: bool,
    focus_handle: FocusHandle,
    space_down: bool,
    drag: Option<PanDrag>,
    options: CanvasOptions,
    cache: render::RenderCache,
    stats: FrameStats,
}

impl DesignCanvas {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            list: Arc::new(DisplayList::default()),
            viewport: Viewport::default(),
            bounds: Bounds::default(),
            has_layout: false,
            pending_fit: false,
            focus_handle: cx.focus_handle(),
            space_down: false,
            drag: None,
            options: CanvasOptions::default(),
            cache: render::RenderCache::default(),
            stats: FrameStats::default(),
        }
    }

    // --- content -------------------------------------------------------------------------

    /// Replace the display list. Prepared data (tessellations, shaped text) is kept for
    /// chunks whose `(root, version)` did not change.
    pub fn set_display_list(&mut self, list: Arc<DisplayList>, cx: &mut Context<Self>) {
        self.cache.retain_roots(&list);
        self.list = list;
        cx.notify();
    }

    pub fn display_list(&self) -> &Arc<DisplayList> {
        &self.list
    }

    /// Drop every cached tessellation / shaped line (e.g. after fonts change).
    pub fn invalidate_caches(&mut self, cx: &mut Context<Self>) {
        self.cache.clear();
        cx.notify();
    }

    pub fn options(&self) -> &CanvasOptions {
        &self.options
    }

    pub fn options_mut(&mut self, cx: &mut Context<Self>) -> &mut CanvasOptions {
        cx.notify();
        &mut self.options
    }

    /// Counters of the last painted frame.
    pub fn last_frame_stats(&self) -> FrameStats {
        self.stats
    }

    // --- viewport ------------------------------------------------------------------------

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn set_viewport(&mut self, viewport: Viewport, cx: &mut Context<Self>) {
        self.viewport = Viewport::new(viewport.offset, viewport.zoom);
        self.pending_fit = false;
        cx.notify();
    }

    pub fn zoom(&self) -> f32 {
        self.viewport.zoom
    }

    /// Canvas size in logical pixels (zero before the first paint).
    pub fn canvas_size(&self) -> Vec2 {
        Vec2::new(
            self.bounds.size.width.as_f32(),
            self.bounds.size.height.as_f32(),
        )
    }

    /// Canvas bounds in window coordinates (from the last paint).
    pub fn canvas_bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    fn local(&self, p: Point<Pixels>) -> Vec2 {
        Vec2::new(
            (p.x - self.bounds.origin.x).as_f32(),
            (p.y - self.bounds.origin.y).as_f32(),
        )
    }

    /// Window point → document point.
    pub fn screen_to_doc(&self, p: Point<Pixels>) -> Vec2 {
        self.viewport.screen_to_doc(self.local(p))
    }

    /// Document point → window point.
    pub fn doc_to_screen(&self, p: Vec2) -> Point<Pixels> {
        let s = self.viewport.doc_to_screen(p);
        point(
            self.bounds.origin.x + px(s.x),
            self.bounds.origin.y + px(s.y),
        )
    }

    /// Document rect → window rect.
    pub fn doc_rect_to_screen(&self, r: &Rect) -> Bounds<Pixels> {
        let s = self.viewport.doc_rect_to_screen(r);
        Bounds {
            origin: point(
                self.bounds.origin.x + px(s.x),
                self.bounds.origin.y + px(s.y),
            ),
            size: gpui_kit::size(px(s.w), px(s.h)),
        }
    }

    /// Document rect currently visible.
    pub fn visible_doc_rect(&self) -> Rect {
        self.viewport.visible_doc_rect(self.canvas_size())
    }

    pub fn pan_by(&mut self, delta: Vec2, cx: &mut Context<Self>) {
        self.viewport.pan_by(delta);
        cx.notify();
    }

    /// Zoom to `zoom` keeping the document point under `anchor` (window coords) fixed; with
    /// `None` the canvas center is the anchor.
    pub fn zoom_to(&mut self, zoom: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let a = anchor
            .map(|p| self.local(p))
            .unwrap_or(self.canvas_size() * 0.5);
        self.viewport.zoom_at(a, zoom);
        cx.notify();
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.zoom_to(viewport::step_zoom(self.viewport.zoom, true), None, cx);
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.zoom_to(viewport::step_zoom(self.viewport.zoom, false), None, cx);
    }

    /// Fit the whole display list. Deferred until the canvas has been laid out.
    pub fn zoom_to_fit(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = self.list.bounds() {
            self.zoom_to_rect(b, cx);
        }
    }

    /// Fit a document rect (e.g. the selection).
    pub fn zoom_to_rect(&mut self, rect: Rect, cx: &mut Context<Self>) {
        if !self.has_layout {
            self.pending_fit = true;
            cx.notify();
            return;
        }
        self.viewport = Viewport::fit(
            &rect,
            self.canvas_size(),
            self.options.fit_padding,
            MAX_ZOOM,
        );
        cx.notify();
    }

    pub fn is_panning(&self) -> bool {
        self.drag.is_some()
    }

    // --- input ---------------------------------------------------------------------------

    fn on_scroll(&mut self, ev: &ScrollEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self.local(ev.position);
        let zoom_mod = ev.modifiers.control || ev.modifiers.platform;
        match ev.delta {
            ScrollDelta::Pixels(d) if zoom_mod => {
                self.viewport
                    .zoom_by(anchor, viewport::wheel_zoom_factor(d.y.as_f32(), true));
            }
            ScrollDelta::Lines(d) if zoom_mod => {
                self.viewport
                    .zoom_by(anchor, viewport::wheel_zoom_factor(d.y, false));
            }
            delta => {
                let d = delta.pixel_delta(px(40.0));
                let mut v = Vec2::new(d.x.as_f32(), d.y.as_f32());
                if ev.modifiers.shift && v.x == 0.0 {
                    v = Vec2::new(v.y, 0.0);
                }
                self.viewport.pan_by(v);
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn on_pinch(&mut self, ev: &PinchEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self.local(ev.position);
        self.viewport.zoom_by(anchor, (1.0 + ev.delta).max(0.01));
        cx.notify();
    }

    fn start_drag(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        self.drag = Some(PanDrag { last: pos });
        cx.notify();
    }

    fn drag_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(d) = &mut self.drag {
            let delta = pos - d.last;
            d.last = pos;
            self.viewport
                .pan_by(Vec2::new(delta.x.as_f32(), delta.y.as_f32()));
            cx.notify();
        }
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = k.modifiers;
        if k.key == "space" && !m.modified() {
            if !self.space_down {
                self.space_down = true;
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if !self.options.builtin_shortcuts {
            return;
        }
        let secondary = m.control || m.platform;
        let handled = match k.key.as_str() {
            "1" if m.shift && !secondary => {
                self.zoom_to_fit(cx);
                true
            }
            "0" if secondary => {
                self.zoom_to(1.0, None, cx);
                true
            }
            "=" | "+" if secondary => {
                self.zoom_in(cx);
                true
            }
            "-" if secondary => {
                self.zoom_out(cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn on_key_up(&mut self, ev: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "space" && self.space_down {
            self.space_down = false;
            cx.notify();
        }
    }

    // --- paint ---------------------------------------------------------------------------

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let t0 = Instant::now();
        self.bounds = bounds;
        self.has_layout = bounds.size.width > px(0.0) && bounds.size.height > px(0.0);
        if self.pending_fit && self.has_layout {
            self.pending_fit = false;
            if let Some(b) = self.list.bounds() {
                self.viewport =
                    Viewport::fit(&b, self.canvas_size(), self.options.fit_padding, MAX_ZOOM);
            }
        }
        let mut stats = FrameStats::default();
        window.paint_quad(fill(bounds, self.options.background));
        let canvas = render::rect_of(&bounds);
        let origin = Vec2::new(canvas.x, canvas.y);
        let bg = self.options.background.to_rgb();
        let params = render::FrameParams {
            canvas,
            doc_to_window: pen_render::Affine2::from_translation(origin)
                * self.viewport.transform(),
            zoom: self.viewport.zoom,
            policy: self.options.lod,
            scale_factor: window.scale_factor(),
            background: pen_render::Color::rgba(bg.r, bg.g, bg.b, bg.a),
            raster_fallback: self.options.raster_fallback,
            limits: fallback::RegionLimits::default(),
        };
        self.cache.fallback.budget = self.options.fallback_budget_bytes;
        self.cache.textures.begin_frame(window);
        let list = self.list.clone();
        let cache = &mut self.cache;
        window.with_content_mask(Some(gpui_kit::ContentMask { bounds }), |window| {
            render::paint_list(&list, cache, &params, window, &mut stats);
            if self.options.show_pixel_grid
                && self.viewport.zoom >= self.options.pixel_grid_min_zoom
            {
                paint_pixel_grid(
                    window,
                    &canvas,
                    &self.viewport,
                    self.options.pixel_grid_color,
                );
            }
        });

        stats.fallback_jobs = self.dispatch_fallback(cx);

        // While panning, follow the pointer even outside the canvas.
        if self.drag.is_some() {
            let this = cx.entity().downgrade();
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _w, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = this.update(cx, |v, cx| v.drag_to(ev.position, cx));
                }
            });
            let this = cx.entity().downgrade();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _w, cx| {
                if phase == DispatchPhase::Bubble {
                    let _ = this.update(cx, |v, cx| v.end_drag(cx));
                }
            });
        }
        stats.paint_time = t0.elapsed();
        stats.painted_at = Some(Instant::now());
        self.stats = stats;
    }
}

impl DesignCanvas {
    /// Start background renders for the fallback textures requested by the last paint
    /// (missing ones without a stand-in first), up to `fallback_max_jobs` in flight.
    fn dispatch_fallback(&mut self, cx: &mut Context<Self>) -> u32 {
        let mut reqs = std::mem::take(&mut self.cache.requests);
        reqs.sort_by_key(|r| r.has_standin);
        let mut started = 0;
        for r in reqs.drain(..) {
            if self.cache.fallback.pending_len() >= self.options.fallback_max_jobs {
                break;
            }
            let key = r.key;
            self.cache.fallback.mark_pending(key);
            let job = fallback::RasterJob {
                key,
                list: self.list.clone(),
                chunk: r.chunk,
                start: r.start,
                end: r.end,
                kind: r.kind,
                region: r.region,
                scale: r.scale,
                alpha: r.alpha,
                background: r.background,
            };
            let executor = cx.background_executor().clone();
            cx.spawn(async move |this, cx| {
                let out = executor
                    .spawn(async move {
                        let o = job.run()?;
                        let bytes = o.bgra.len();
                        let img = render::render_image_from_bgra(o.width, o.height, o.bgra)?;
                        Some((o.doc_rect, img, bytes))
                    })
                    .await;
                let _ = this.update(cx, |v, cx| {
                    v.cache.finish_fallback(key, out);
                    cx.notify();
                });
            })
            .detach();
            started += 1;
        }
        self.cache.requests = reqs;
        started
    }
}

fn paint_pixel_grid(window: &mut Window, canvas: &Rect, vp: &Viewport, color: Hsla) {
    let lw = 1.0 / window.scale_factor();
    let view = vp.visible_doc_rect(Vec2::new(canvas.w, canvas.h));
    let (x0, nx) = lod::grid_lines(view.x, view.right());
    for i in 0..nx {
        let x = canvas.x + vp.doc_to_screen(Vec2::new((x0 + i) as f32, 0.0)).x;
        window.paint_quad(fill(
            Bounds {
                origin: point(px(x), px(canvas.y)),
                size: gpui_kit::size(px(lw), px(canvas.h)),
            },
            color,
        ));
    }
    let (y0, ny) = lod::grid_lines(view.y, view.bottom());
    for i in 0..ny {
        let y = canvas.y + vp.doc_to_screen(Vec2::new(0.0, (y0 + i) as f32)).y;
        window.paint_quad(fill(
            Bounds {
                origin: point(px(canvas.x), px(y)),
                size: gpui_kit::size(px(canvas.w), px(lw)),
            },
            color,
        ));
    }
}

impl Focusable for DesignCanvas {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DesignCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let cursor = if self.drag.is_some() {
            CursorStyle::ClosedHand
        } else if self.space_down {
            CursorStyle::OpenHand
        } else {
            CursorStyle::Arrow
        };
        div()
            .id("pen-design-canvas")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .cursor(cursor)
            .on_scroll(cx.listener(Self::on_scroll))
            .on_pinch(cx.listener(Self::on_pinch))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    this.start_drag(ev.position, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus_handle, cx);
                    if this.space_down {
                        this.start_drag(ev.position, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        this.update(cx, |view, cx| view.paint(bounds, window, cx));
                    },
                )
                .size_full(),
            )
    }
}
