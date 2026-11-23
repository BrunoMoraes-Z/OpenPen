//! Renders a synthetic list of ~5k mixed items at 1920x1080.
//!
//! `cargo bench -p pen-raster`

use criterion::{Criterion, criterion_group, criterion_main};
use pen_raster::RasterContext;
use pen_render::*;
use smallvec::smallvec;
use std::hint::black_box;
use std::sync::Arc;

fn lcg(seed: &mut u64) -> f32 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*seed >> 33) as f32) / (1u64 << 31) as f32
}

fn color(seed: &mut u64, a: f32) -> Color {
    Color::rgba(lcg(seed), lcg(seed), lcg(seed), a)
}

/// ~5k items: 40 "cards" (chunks), each a clipped frame with shadowed background, rects, ellipses,
/// paths, gradients, strokes and text.
pub fn synthetic_list() -> DisplayList {
    let mut seed = 42u64;
    let mut chunks = Vec::new();
    let star: Arc<Path> = Arc::new(Path {
        els: (0..5)
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 * 4.0 * std::f32::consts::PI / 5.0;
                let p = Vec2::new(8.0 + 8.0 * a.cos(), 8.0 + 8.0 * a.sin());
                if i == 0 {
                    PathEl::MoveTo(p)
                } else {
                    PathEl::LineTo(p)
                }
            })
            .chain([PathEl::Close])
            .collect(),
        fill_rule: FillRule::NonZero,
    });
    let text = |s: &str, size: f32, c: Color| {
        Arc::new(TextBlock {
            text: s.into(),
            runs: smallvec![TextRun {
                range: 0..s.len(),
                style: TextStyle {
                    family: "Inter".into(),
                    size,
                    weight: 400,
                    italic: false,
                    letter_spacing: 0.0,
                    underline: false,
                    strikethrough: false,
                    paint: Paint::Solid(c),
                },
            }],
            lines: vec![TextLine {
                range: 0..s.len(),
                x: 0.0,
                baseline: size,
                width: 0.0,
            }],
            align: TextAlign::Left,
            size: Vec2::new(200.0, size * 1.4),
            clip: false,
        })
    };
    let words = [
        "Dashboard",
        "Revenue 12,340",
        "Settings",
        "Profile",
        "Inbox (3)",
        "Search…",
        "Total",
        "OK",
    ];
    for cy in 0..5 {
        for cx in 0..8 {
            let (x0, y0) = (16.0 + cx as f32 * 238.0, 16.0 + cy as f32 * 212.0);
            let (w, h) = (222.0, 196.0);
            let mut items = Vec::with_capacity(130);
            items.push(DisplayItem::Shape {
                node: NodeKey(0),
                transform: Affine2::from_translation(Vec2::new(x0, y0)),
                shape: Shape::Rect {
                    size: Vec2::new(w, h),
                    radii: CornerRadii::uniform(12.0),
                },
                fills: smallvec![Paint::Solid(Color::WHITE)],
                stroke: Some(Stroke {
                    paints: smallvec![Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 0.1))],
                    widths: [1.0; 4],
                    align: StrokeAlign::Inside,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    miter_limit: 4.0,
                    dash: smallvec![],
                }),
                shadows: smallvec![Shadow::Drop {
                    offset: Vec2::new(0.0, 4.0),
                    blur: 16.0,
                    spread: 0.0,
                    color: Color::rgba(0.0, 0.0, 0.0, 0.15)
                }],
            });
            items.push(DisplayItem::PushClip {
                transform: Affine2::from_translation(Vec2::new(x0, y0)),
                shape: Shape::Rect {
                    size: Vec2::new(w, h),
                    radii: CornerRadii::uniform(12.0),
                },
            });
            for i in 0..120 {
                let k = i % 6;
                let (px, py) = (
                    x0 + lcg(&mut seed) * (w - 30.0),
                    y0 + lcg(&mut seed) * (h - 20.0),
                );
                let t = Affine2::from_translation(Vec2::new(px, py));
                let item = match k {
                    0 | 1 => DisplayItem::Shape {
                        node: NodeKey(i),
                        transform: t,
                        shape: Shape::Rect {
                            size: Vec2::new(
                                10.0 + lcg(&mut seed) * 40.0,
                                6.0 + lcg(&mut seed) * 20.0,
                            ),
                            radii: CornerRadii::uniform(if k == 0 { 0.0 } else { 4.0 }),
                        },
                        fills: smallvec![Paint::Solid(color(&mut seed, 0.9))],
                        stroke: None,
                        shadows: smallvec![],
                    },
                    2 => DisplayItem::Shape {
                        node: NodeKey(i),
                        transform: t,
                        shape: Shape::Ellipse {
                            size: Vec2::new(12.0, 12.0),
                        },
                        fills: smallvec![Paint::Linear {
                            start: Vec2::ZERO,
                            end: Vec2::new(12.0, 12.0),
                            stops: smallvec![
                                GradientStop {
                                    offset: 0.0,
                                    color: color(&mut seed, 1.0)
                                },
                                GradientStop {
                                    offset: 1.0,
                                    color: color(&mut seed, 1.0)
                                }
                            ],
                        }],
                        stroke: None,
                        shadows: smallvec![],
                    },
                    3 => DisplayItem::Shape {
                        node: NodeKey(i),
                        transform: t * Affine2::from_angle(lcg(&mut seed)),
                        shape: Shape::Path(star.clone()),
                        fills: smallvec![Paint::Solid(color(&mut seed, 1.0))],
                        stroke: Some(Stroke {
                            paints: smallvec![Paint::Solid(Color::BLACK)],
                            widths: [1.0; 4],
                            align: StrokeAlign::Center,
                            cap: LineCap::Round,
                            join: LineJoin::Round,
                            miter_limit: 4.0,
                            dash: smallvec![],
                        }),
                        shadows: smallvec![],
                    },
                    _ => DisplayItem::Text {
                        node: NodeKey(i),
                        transform: t,
                        block: text(
                            words[(i as usize / 6) % words.len()],
                            11.0 + (i % 3) as f32 * 2.0,
                            Color::rgba(0.1, 0.1, 0.15, 1.0),
                        ),
                    },
                };
                items.push(item);
            }
            items.push(DisplayItem::PopClip);
            chunks.push(Chunk {
                root: NodeKey((cy * 8 + cx) as u32),
                bounds: Rect::new(x0 - 24.0, y0 - 24.0, w + 48.0, h + 48.0),
                version: 0,
                items,
            });
        }
    }
    DisplayList::from_chunks(chunks)
}

fn bench(c: &mut Criterion) {
    let list = synthetic_list();
    let viewport = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    let mut ctx = RasterContext::new();
    // Warm caches (fonts, shaping, glyphs).
    let mut out = ctx.render(&list, viewport, 1.0, Color::WHITE);
    eprintln!("items: {}", list.item_count());
    let mut g = c.benchmark_group("render");
    g.sample_size(20);
    g.bench_function("5k_mixed_1080p", |b| {
        b.iter(|| ctx.render_into(black_box(&list), viewport, 1.0, Color::WHITE, &mut out));
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
