//! Stress test: a synthetic DisplayList (200 frames × 50 children ≈ 10k+ items) with
//! automatic pan/zoom for N seconds; prints frame-time stats and exits.
//!
//! `cargo run -p pen-canvas --example canvas_stress --release -- [seconds] [frames]`

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Styled, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use pen_canvas::{DesignCanvas, FrameStats, Viewport};
use pen_render::{
    Affine2, BlendMode, Chunk, Color, CornerRadii, DisplayItem, DisplayList, GradientStop, LineCap,
    LineJoin, NodeKey, Paint, Path, PathEl, Rect, Shadow, Shape, Stroke, StrokeAlign, TextAlign,
    TextBlock, TextLine, TextRun, TextStyle, Vec2,
};
use smallvec::smallvec;

const FRAME_W: f32 = 400.0;
const FRAME_H: f32 = 300.0;
const GAP: f32 = 80.0;
const COLS: usize = 20;

fn hsl(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color::rgba(r + m, g + m, b + m, 1.0)
}

fn star(points: usize, r0: f32, r1: f32) -> Arc<Path> {
    let mut els = Vec::new();
    for i in 0..points * 2 {
        let a =
            i as f32 / (points * 2) as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let r = if i % 2 == 0 { r0 } else { r1 };
        let p = Vec2::new(r0 + a.cos() * r, r0 + a.sin() * r);
        els.push(if i == 0 {
            PathEl::MoveTo(p)
        } else {
            PathEl::LineTo(p)
        });
    }
    els.push(PathEl::Close);
    Arc::new(Path {
        els,
        fill_rule: Default::default(),
    })
}

fn wave(w: f32, h: f32) -> Arc<Path> {
    let mut els = vec![PathEl::MoveTo(Vec2::new(0.0, h * 0.5))];
    let n = 4;
    for i in 0..n {
        let x0 = w * i as f32 / n as f32;
        let x1 = w * (i + 1) as f32 / n as f32;
        els.push(PathEl::CubicTo(
            Vec2::new(x0 + (x1 - x0) * 0.3, 0.0),
            Vec2::new(x0 + (x1 - x0) * 0.7, h),
            Vec2::new(x1, h * 0.5),
        ));
    }
    Arc::new(Path {
        els,
        fill_rule: Default::default(),
    })
}

fn stroke(color: Color, w: f32) -> Stroke {
    Stroke {
        paints: smallvec![Paint::Solid(color)],
        widths: [w; 4],
        align: StrokeAlign::Inside,
        cap: LineCap::Round,
        join: LineJoin::Round,
        miter_limit: 4.0,
        dash: smallvec![],
    }
}

fn text_item(node: u32, at: Vec2, s: &str, size: f32, color: Color, weight: u16) -> DisplayItem {
    let width = s.chars().count() as f32 * size * 0.55;
    let text: Arc<str> = s.into();
    DisplayItem::Text {
        node: NodeKey(node),
        transform: Affine2::from_translation(at),
        block: Arc::new(TextBlock {
            runs: smallvec![TextRun {
                range: 0..text.len(),
                style: TextStyle {
                    family: "Segoe UI".into(),
                    size,
                    weight,
                    italic: false,
                    letter_spacing: 0.0,
                    underline: false,
                    strikethrough: false,
                    paint: Paint::Solid(color),
                },
            }],
            lines: vec![TextLine {
                range: 0..text.len(),
                x: 0.0,
                baseline: size,
                width,
            }],
            align: TextAlign::Left,
            size: Vec2::new(width, size * 1.3),
            clip: false,
            text,
        }),
    }
}

const WORDS: [&str; 12] = [
    "Dashboard",
    "Revenue this month",
    "Active users",
    "Settings",
    "Sign in to continue",
    "Quarterly report",
    "Pricing",
    "Team members",
    "Notifications",
    "Upload files",
    "Recent activity",
    "Get started",
];

fn synthetic(frames: usize) -> DisplayList {
    let mut chunks = Vec::with_capacity(frames);
    let mut node = 0u32;
    for f in 0..frames {
        let col = f % COLS;
        let row = f / COLS;
        let origin = Vec2::new(col as f32 * (FRAME_W + GAP), row as f32 * (FRAME_H + GAP));
        let t = |x: f32, y: f32| Affine2::from_translation(origin + Vec2::new(x, y));
        let hue = (f * 37) as f32;
        let mut items = Vec::with_capacity(56);
        node += 1;
        items.push(DisplayItem::Shape {
            node: NodeKey(node),
            transform: t(0.0, 0.0),
            shape: Shape::Rect {
                size: Vec2::new(FRAME_W, FRAME_H),
                radii: CornerRadii::uniform(12.0),
            },
            fills: smallvec![Paint::Solid(Color::WHITE)],
            stroke: Some(stroke(Color::rgba(0.0, 0.0, 0.0, 0.1), 1.0)),
            shadows: smallvec![Shadow::Drop {
                offset: Vec2::new(0.0, 4.0),
                blur: 16.0,
                spread: 0.0,
                color: Color::rgba(0.0, 0.0, 0.0, 0.12),
            }],
        });
        items.push(DisplayItem::PushClip {
            transform: t(0.0, 0.0),
            shape: Shape::Rect {
                size: Vec2::new(FRAME_W, FRAME_H),
                radii: CornerRadii::uniform(12.0),
            },
        });
        // Header with a 2-stop linear gradient.
        node += 1;
        items.push(DisplayItem::Shape {
            node: NodeKey(node),
            transform: t(0.0, 0.0),
            shape: Shape::Rect {
                size: Vec2::new(FRAME_W, 48.0),
                radii: CornerRadii::default(),
            },
            fills: smallvec![Paint::Linear {
                start: Vec2::ZERO,
                end: Vec2::new(FRAME_W, 48.0),
                stops: smallvec![
                    GradientStop {
                        offset: 0.0,
                        color: hsl(hue, 0.7, 0.55)
                    },
                    GradientStop {
                        offset: 1.0,
                        color: hsl(hue + 60.0, 0.7, 0.45)
                    },
                ],
            }],
            stroke: None,
            shadows: smallvec![],
        });
        node += 1;
        items.push(text_item(
            node,
            origin + Vec2::new(16.0, 12.0),
            WORDS[f % WORDS.len()],
            18.0,
            Color::WHITE,
            600,
        ));
        for i in 0..48usize {
            node += 1;
            let cx = (i % 8) as f32 * 48.0 + 12.0;
            let cy = (i / 8) as f32 * 40.0 + 60.0;
            let color = hsl(hue + i as f32 * 7.0, 0.55, 0.6);
            let item = match i % 8 {
                0..=2 => DisplayItem::Shape {
                    node: NodeKey(node),
                    transform: t(cx, cy),
                    shape: Shape::Rect {
                        size: Vec2::new(40.0, 30.0),
                        radii: CornerRadii([6.0, 6.0, 2.0, 2.0]),
                    },
                    fills: smallvec![Paint::Solid(color)],
                    stroke: if i % 3 == 0 {
                        Some(stroke(Color::BLACK.with_alpha_mul(0.3), 1.0))
                    } else {
                        None
                    },
                    shadows: if i % 8 == 1 {
                        smallvec![Shadow::Drop {
                            offset: Vec2::new(0.0, 2.0),
                            blur: 6.0,
                            spread: 0.0,
                            color: Color::rgba(0.0, 0.0, 0.0, 0.2)
                        }]
                    } else {
                        smallvec![]
                    },
                },
                3..=4 => text_item(
                    node,
                    origin + Vec2::new(cx, cy + 4.0),
                    WORDS[(f + i) % WORDS.len()],
                    11.0,
                    Color::rgba(0.1, 0.1, 0.12, 1.0),
                    400,
                ),
                5 => DisplayItem::Shape {
                    node: NodeKey(node),
                    transform: t(cx + 4.0, cy),
                    shape: Shape::Path(star(5, 15.0, 7.0)),
                    fills: smallvec![Paint::Solid(color)],
                    stroke: Some(stroke(Color::BLACK.with_alpha_mul(0.5), 1.0)),
                    shadows: smallvec![],
                },
                6 => DisplayItem::Shape {
                    node: NodeKey(node),
                    transform: Affine2::from_translation(origin + Vec2::new(cx + 20.0, cy + 15.0))
                        * Affine2::from_angle(0.3 + i as f32 * 0.1)
                        * Affine2::from_translation(Vec2::new(-15.0, -10.0)),
                    shape: Shape::Rect {
                        size: Vec2::new(30.0, 20.0),
                        radii: CornerRadii::uniform(4.0),
                    },
                    fills: smallvec![Paint::radial(
                        Vec2::new(15.0, 10.0),
                        Vec2::new(15.0, 10.0),
                        smallvec![
                            GradientStop {
                                offset: 0.0,
                                color: Color::WHITE
                            },
                            GradientStop { offset: 1.0, color },
                        ]
                    )],
                    stroke: None,
                    shadows: smallvec![],
                },
                _ => {
                    if i % 16 == 7 {
                        DisplayItem::Shape {
                            node: NodeKey(node),
                            transform: t(cx, cy),
                            shape: Shape::Ellipse {
                                size: Vec2::new(30.0, 30.0),
                            },
                            fills: smallvec![Paint::radial(
                                Vec2::new(15.0, 15.0),
                                Vec2::new(15.0, 15.0),
                                smallvec![
                                    GradientStop {
                                        offset: 0.0,
                                        color: Color::WHITE
                                    },
                                    GradientStop { offset: 0.6, color },
                                    GradientStop {
                                        offset: 1.0,
                                        color: Color::BLACK
                                    },
                                ]
                            )],
                            stroke: None,
                            shadows: smallvec![],
                        }
                    } else {
                        let mut s = stroke(color, 2.0);
                        s.align = StrokeAlign::Center;
                        s.dash = smallvec![4.0, 3.0];
                        DisplayItem::Shape {
                            node: NodeKey(node),
                            transform: t(cx, cy),
                            shape: Shape::Path(wave(40.0, 30.0)),
                            fills: smallvec![],
                            stroke: Some(s),
                            shadows: smallvec![],
                        }
                    }
                }
            };
            items.push(item);
        }
        // A translucent layer on top of every 4th frame.
        if f % 4 == 0 {
            items.push(DisplayItem::PushLayer {
                opacity: 0.6,
                blend: BlendMode::Normal,
                blur: 0.0,
                backdrop_blur: 0.0,
                bounds: Rect::default(),
            });
            node += 1;
            items.push(DisplayItem::Shape {
                node: NodeKey(node),
                transform: t(FRAME_W - 120.0, FRAME_H - 60.0),
                shape: Shape::Rect {
                    size: Vec2::new(100.0, 40.0),
                    radii: CornerRadii::uniform(20.0),
                },
                fills: smallvec![Paint::Solid(hsl(hue + 180.0, 0.8, 0.5))],
                stroke: None,
                shadows: smallvec![],
            });
            items.push(DisplayItem::PopLayer);
        }
        if f % 8 == 2 {
            fallback_extras(&mut items, origin, &mut node, hue);
        }
        items.push(DisplayItem::PopClip);
        chunks.push(Chunk {
            root: NodeKey(f as u32 + 1_000_000),
            bounds: Rect::new(
                origin.x - 30.0,
                origin.y - 30.0,
                FRAME_W + 60.0,
                FRAME_H + 60.0,
            ),
            version: 1,
            items,
        });
    }
    DisplayList::from_chunks(chunks)
}

fn solid_shape(node: u32, t: Affine2, shape: Shape, color: Color) -> DisplayItem {
    DisplayItem::Shape {
        node: NodeKey(node),
        transform: t,
        shape,
        fills: smallvec![Paint::Solid(color)],
        stroke: None,
        shadows: smallvec![],
    }
}

fn layer(opacity: f32, blend: BlendMode, blur: f32, backdrop: f32, bounds: Rect) -> DisplayItem {
    DisplayItem::PushLayer {
        opacity,
        blend,
        blur,
        backdrop_blur: backdrop,
        bounds,
    }
}

/// Content GPUI cannot draw natively (CPU-raster fallback): layer blur, multiply blend,
/// backdrop blur, a circular clip with a star and a gradient bar crossing it, and an opacity
/// group over overlapping circles.
fn fallback_extras(items: &mut Vec<DisplayItem>, origin: Vec2, node: &mut u32, hue: f32) {
    let t = |x: f32, y: f32| Affine2::from_translation(origin + Vec2::new(x, y));
    let mut n = || {
        *node += 1;
        *node
    };
    // Layer blur: an orange disc and a label, blurred.
    items.push(layer(1.0, BlendMode::Normal, 8.0, 0.0, Rect::default()));
    items.push(solid_shape(
        n(),
        t(300.0, 70.0),
        Shape::Ellipse {
            size: Vec2::new(80.0, 80.0),
        },
        Color::rgba(1.0, 0.55, 0.1, 1.0),
    ));
    items.push(text_item(
        n(),
        origin + Vec2::new(316.0, 96.0),
        "Blur",
        20.0,
        Color::WHITE,
        700,
    ));
    items.push(DisplayItem::PopLayer);
    // Multiply blend over the grid.
    items.push(layer(1.0, BlendMode::Multiply, 0.0, 0.0, Rect::default()));
    items.push(solid_shape(
        n(),
        t(20.0, 150.0),
        Shape::Rect {
            size: Vec2::new(180.0, 50.0),
            radii: CornerRadii::uniform(10.0),
        },
        Color::rgba(1.0, 0.85, 0.2, 1.0),
    ));
    items.push(DisplayItem::PopLayer);
    // Backdrop blur (frosted panel).
    let panel = Rect::new(origin.x + 220.0, origin.y + 170.0, 150.0, 70.0);
    items.push(layer(1.0, BlendMode::Normal, 0.0, 6.0, panel));
    items.push(solid_shape(
        n(),
        Affine2::from_translation(Vec2::new(panel.x, panel.y)),
        Shape::Rect {
            size: Vec2::new(panel.w, panel.h),
            radii: CornerRadii::default(),
        },
        Color::rgba(1.0, 1.0, 1.0, 0.35),
    ));
    items.push(DisplayItem::PopLayer);
    // Circular clip with content crossing it.
    items.push(DisplayItem::PushClip {
        transform: t(40.0, 215.0),
        shape: Shape::Ellipse {
            size: Vec2::new(70.0, 70.0),
        },
    });
    items.push(DisplayItem::Shape {
        node: NodeKey(n()),
        transform: Affine2::from_translation(origin + Vec2::new(75.0, 250.0))
            * Affine2::from_angle(0.5)
            * Affine2::from_translation(Vec2::new(-60.0, -15.0)),
        shape: Shape::Rect {
            size: Vec2::new(120.0, 30.0),
            radii: CornerRadii::default(),
        },
        fills: smallvec![Paint::Linear {
            start: Vec2::ZERO,
            end: Vec2::new(120.0, 0.0),
            stops: smallvec![
                GradientStop {
                    offset: 0.0,
                    color: hsl(hue + 90.0, 0.8, 0.5)
                },
                GradientStop {
                    offset: 1.0,
                    color: hsl(hue + 200.0, 0.8, 0.4)
                },
            ],
        }],
        stroke: None,
        shadows: smallvec![],
    });
    items.push(solid_shape(
        n(),
        t(45.0, 220.0),
        Shape::Path(star(5, 35.0, 15.0)),
        Color::rgba(0.2, 0.2, 0.8, 0.8),
    ));
    items.push(DisplayItem::PopClip);
    // Opacity group over overlapping circles (no double alpha where they overlap).
    items.push(layer(0.5, BlendMode::Normal, 0.0, 0.0, Rect::default()));
    for (i, c) in [
        Color::rgba(0.9, 0.1, 0.3, 1.0),
        Color::rgba(0.1, 0.6, 0.9, 1.0),
    ]
    .into_iter()
    .enumerate()
    {
        items.push(solid_shape(
            n(),
            t(130.0 + i as f32 * 35.0, 215.0),
            Shape::Ellipse {
                size: Vec2::new(60.0, 60.0),
            },
            c,
        ));
    }
    items.push(DisplayItem::PopLayer);
}

/// `STRESS_STILL="x,y,zoom"`: hold a fixed camera (doc center + zoom) instead of animating.
fn still_camera() -> Option<(Vec2, f32)> {
    let v = std::env::var("STRESS_STILL").ok()?;
    let p: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    (p.len() == 3).then(|| (Vec2::new(p[0], p[1]), p[2]))
}

struct Driver {
    canvas: Entity<DesignCanvas>,
    duration: Duration,
    start: Option<Instant>,
    last_render: Option<Instant>,
    intervals: Vec<f64>,
    cpu: Vec<f64>,
    paint: Vec<f64>,
    shape_ms: Vec<f64>,
    text_ms: Vec<f64>,
    frames: u32,
    sum: FrameStats,
    content: Rect,
    done: bool,
    list: Arc<DisplayList>,
    still: Option<(Vec2, f32)>,
    raster_written: bool,
}

fn summarize(name: &str, v: &mut [f64]) {
    if v.is_empty() {
        println!("{name}: no samples");
        return;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = v.iter().sum::<f64>() / v.len() as f64;
    let p = |q: f64| v[((v.len() as f64 - 1.0) * q).round() as usize];
    println!(
        "{name:<28} avg {avg:6.2} ms | p50 {:6.2} | p95 {:6.2} | p99 {:6.2} | max {:6.2}  (n={})",
        p(0.5),
        p(0.95),
        p(0.99),
        v[v.len() - 1],
        v.len()
    );
}

impl Render for Driver {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let start = *self.start.get_or_insert(now);
        let stats = self.canvas.read(cx).last_frame_stats();
        const WARMUP: u32 = 20;
        if let Some(last) = self.last_render {
            self.frames += 1;
            if self.frames > WARMUP {
                self.intervals.push((now - last).as_secs_f64() * 1000.0);
                self.paint.push(stats.paint_time.as_secs_f64() * 1000.0);
                self.shape_ms.push(stats.shape_time.as_secs_f64() * 1000.0);
                if std::env::var_os("STRESS_VERBOSE").is_some()
                    && stats.paint_time.as_secs_f64() > 0.008
                {
                    let z = self.canvas.read(cx).zoom();
                    println!("slow frame: zoom {z:.3} {stats:?}");
                }
                self.text_ms.push(stats.text_time.as_secs_f64() * 1000.0);
                if let Some(p) = stats.painted_at {
                    self.cpu.push((p - last).as_secs_f64() * 1000.0);
                }
                let s = &mut self.sum;
                s.items_drawn += stats.items_drawn;
                s.items_culled += stats.items_culled;
                s.chunks_visible += stats.chunks_visible;
                s.glyphs += stats.glyphs;
                s.paths += stats.paths;
                s.quads += stats.quads;
                s.text_bars += stats.text_bars;
                s.retessellated += stats.retessellated;
                s.images += stats.images;
                s.shadows += stats.shadows;
                s.layers += stats.layers;
                s.direct_segments += stats.direct_segments;
                s.fallback_scopes += stats.fallback_scopes;
                s.fallback_drawn += stats.fallback_drawn;
                s.fallback_standin += stats.fallback_standin;
                s.fallback_waiting += stats.fallback_waiting;
                s.fallback_jobs += stats.fallback_jobs;
                s.fallback_bytes = s.fallback_bytes.max(stats.fallback_bytes);
            }
        }
        self.last_render = Some(now);

        let t = (now - start).as_secs_f64() as f32;
        if t >= self.duration.as_secs_f32() && !self.done {
            self.done = true;
            let n = self.intervals.len().max(1) as u32;
            println!(
                "--- canvas_stress: {} frames measured over {:.1}s ---",
                self.intervals.len(),
                t
            );
            summarize("frame interval", &mut self.intervals);
            summarize("cpu (render->paint end)", &mut self.cpu);
            summarize("canvas paint", &mut self.paint);
            summarize("  shapes", &mut self.shape_ms);
            summarize("  text", &mut self.text_ms);
            let s = self.sum;
            println!(
                "per frame avg: chunks {} | items drawn {} culled {} | quads {} paths {} glyphs {} bars {} images {} shadows {} | retess total {}",
                s.chunks_visible / n,
                s.items_drawn / n,
                s.items_culled / n,
                s.quads / n,
                s.paths / n,
                s.glyphs / n,
                s.text_bars / n,
                s.images / n,
                s.shadows / n,
                s.retessellated
            );
            println!(
                "per frame avg: layers {} direct {} | fallback scopes {} drawn {} stand-in {} waiting {} | jobs total {} | cache peak {:.1} MB",
                s.layers / n,
                s.direct_segments / n,
                s.fallback_scopes / n,
                s.fallback_drawn / n,
                s.fallback_standin / n,
                s.fallback_waiting / n,
                s.fallback_jobs,
                s.fallback_bytes as f64 / (1u64 << 20) as f64
            );
            cx.quit();
        }

        // Camera: zoom oscillates between "fit all" and 250% while the center wanders.
        let size = self.canvas.read(cx).canvas_size();
        if size.x > 0.0
            && let Some((focus, zoom)) = self.still
        {
            let vp = Viewport {
                offset: size * 0.5 - focus * zoom,
                zoom,
            };
            self.canvas.update(cx, |c, cx| c.set_viewport(vp, cx));
            // Reference render of exactly the same view with pen-raster.
            if !self.raster_written
                && self.frames > 60
                && let Ok(path) = std::env::var("STRESS_RASTER_OUT")
            {
                self.raster_written = true;
                let view = vp.visible_doc_rect(size);
                let scale = zoom * window.scale_factor();
                let g = 0xf6 as f32 / 255.0;
                match pen_raster::render_to_png(&self.list, view, scale, Color::rgba(g, g, g, 1.0))
                {
                    Ok(png) => {
                        let _ = std::fs::write(&path, png);
                        println!("pen-raster reference written to {path} (scale {scale})");
                    }
                    Err(e) => println!("raster failed: {e}"),
                }
            }
        } else if size.x > 0.0 {
            let fit = Viewport::fit(&self.content, size, 40.0, 8.0);
            let phase = 0.5 - 0.5 * (t * std::f32::consts::TAU / 5.0).cos();
            let zoom = (fit.zoom.ln() + (2.5f32.ln() - fit.zoom.ln()) * phase).exp();
            let c = Vec2::new(
                self.content.x + self.content.w * (0.5 + 0.35 * (t * 0.7).sin()),
                self.content.y + self.content.h * (0.5 + 0.35 * (t * 0.43).cos()),
            );
            let center = Vec2::new(
                self.content.x + self.content.w * 0.5,
                self.content.y + self.content.h * 0.5,
            );
            let focus = center + (c - center) * phase;
            let vp = Viewport {
                offset: size * 0.5 - focus * zoom,
                zoom,
            };
            self.canvas.update(cx, |c, cx| c.set_viewport(vp, cx));
        }
        window.request_animation_frame();
        div().size_full().child(self.canvas.clone())
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let secs: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(10.0);
    let frames: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(200);
    let list = Arc::new(synthetic(frames));
    println!(
        "synthetic list: {} chunks, {} items",
        list.chunks.len(),
        list.item_count()
    );
    let content = list.bounds().unwrap_or_default();

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(1600.0), px(1000.0)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        gpui_kit::open_window(opts, cx, move |_window, cx| {
            let canvas = cx.new(|cx| {
                let mut c = DesignCanvas::new(cx);
                c.set_display_list(list.clone(), cx);
                if std::env::var_os("STRESS_NO_FALLBACK").is_some() {
                    c.options_mut(cx).raster_fallback = false;
                }
                if std::env::var_os("STRESS_NO_LAYERS").is_some() {
                    c.options_mut(cx).lod.flatten_below_zoom = 0.0;
                }
                if std::env::var_os("STRESS_ALWAYS_LAYERS").is_some() {
                    c.options_mut(cx).lod.flatten_below_zoom = f32::INFINITY;
                }
                c
            });
            cx.new(|_| Driver {
                canvas,
                duration: Duration::from_secs_f64(secs),
                start: None,
                last_render: None,
                intervals: Vec::new(),
                cpu: Vec::new(),
                paint: Vec::new(),
                shape_ms: Vec::new(),
                text_ms: Vec::new(),
                frames: 0,
                sum: FrameStats::default(),
                content,
                done: false,
                list: list.clone(),
                still: still_camera(),
                raster_written: false,
            })
        })
        .expect("open window");
    });
}
