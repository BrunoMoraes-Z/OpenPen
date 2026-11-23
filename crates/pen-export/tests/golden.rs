//! SVG fidelity tests: the scenes of pen-raster's `tests/golden.rs` are exported to SVG,
//! rasterized with resvg and compared with pen-raster's own rendering of the same list.
//!
//! - Every scene: pixel comparison within a per-scene tolerance. On mismatch both renderings
//!   are written as `tests/golden/<name>.actual.png` (resvg) and `<name>.raster.actual.png`.
//! - A few simple scenes also keep a reference `tests/golden/<name>.svg` (exact text). A
//!   missing one is written (bootstrap); `PEN_EXPORT_UPDATE_GOLDEN=1` rewrites them; a
//!   divergent output is written as `<name>.actual.svg`.

use pen_export::{ExportOptions, TextMode};
use pen_raster::{Pixmap, RasterContext};
use pen_render::*;
use smallvec::smallvec;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use svg2pdf::usvg;

// ------------------------------------------------------------------ harness

fn ctx() -> &'static Mutex<RasterContext> {
    static CTX: OnceLock<Mutex<RasterContext>> = OnceLock::new();
    CTX.get_or_init(|| Mutex::new(RasterContext::new()))
}

fn usvg_options() -> &'static usvg::Options<'static> {
    static OPTS: OnceLock<usvg::Options<'static>> = OnceLock::new();
    OPTS.get_or_init(|| {
        let mut db = pen_fonts::system::bundled_db();
        pen_fonts::system::load_family(&mut db, "Arial");
        usvg::Options {
            fontdb: Arc::new(db),
            font_family: "Inter".into(),
            ..usvg::Options::default()
        }
    })
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

/// Pixel tolerance: max channel difference, and the fraction of pixels allowed beyond it.
#[derive(Clone, Copy)]
struct Tol {
    channel: i32,
    fraction: f64,
}

/// Vector-only scenes: resvg and pen-raster both use tiny-skia, only AA/curve flattening differ.
const TIGHT: Tol = Tol {
    channel: 16,
    fraction: 0.01,
};
/// Blurs (box-blur approximations differ), rasterized fallbacks resampled.
const BLUR: Tol = Tol {
    channel: 32,
    fraction: 0.04,
};
/// Real text shaped by usvg (rustybuzz) vs cosmic-text: sub-pixel glyph placement differs.
const TEXT: Tol = Tol {
    channel: 96,
    fraction: 0.06,
};

fn export(list: &DisplayList, viewport: Rect, scale: f32, bg: Color, text: TextMode) -> String {
    let opts = ExportOptions {
        region: Some(viewport),
        scale,
        background: Some(bg),
        text,
        ..ExportOptions::default()
    };
    let mut c = ctx().lock().unwrap_or_else(|e| e.into_inner());
    pen_export::svg::export_with(&mut c, list, &opts)
}

fn rasterize_svg(svg: &str) -> Pixmap {
    let tree = usvg::Tree::from_str(svg, usvg_options()).expect("exported SVG parses");
    let size = tree.size().to_int_size();
    let mut p = Pixmap::new(size.width(), size.height()).unwrap();
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut p.as_mut());
    p
}

fn check(name: &str, list: &DisplayList, viewport: Rect, scale: f32, bg: Color, tol: Tol) {
    check_mode(name, list, viewport, scale, bg, tol, TextMode::Text);
}

fn check_mode(
    name: &str,
    list: &DisplayList,
    viewport: Rect,
    scale: f32,
    bg: Color,
    tol: Tol,
    mode: TextMode,
) {
    let svg = export(list, viewport, scale, bg, mode);
    let actual = rasterize_svg(&svg);
    let expected = ctx()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .render(list, viewport, scale, bg);
    let dir = golden_dir();
    let actual_path = dir.join(format!("{name}.actual.png"));
    let raster_path = dir.join(format!("{name}.raster.actual.png"));
    let fail = |msg: String| -> ! {
        std::fs::create_dir_all(&dir).unwrap();
        actual.save_png(&actual_path).unwrap();
        expected.save_png(&raster_path).unwrap();
        std::fs::write(dir.join(format!("{name}.actual.svg")), &svg).unwrap();
        panic!("{name}: {msg}; wrote {}", actual_path.display());
    };
    if (actual.width(), actual.height()) != (expected.width(), expected.height()) {
        fail(format!(
            "size {}x{} != raster {}x{}",
            actual.width(),
            actual.height(),
            expected.width(),
            expected.height()
        ));
    }
    let (bad, worst) = diff(&actual, &expected, tol.channel);
    let total = (actual.width() * actual.height()) as usize;
    let allowed = ((total as f64 * tol.fraction) as usize).max(2);
    eprintln!(
        "{name}: {bad}/{total} pixels beyond {} (worst {worst})",
        tol.channel
    );
    if bad > allowed {
        fail(format!(
            "{bad} pixels differ (allowed {allowed}, worst channel diff {worst})"
        ));
    }
    let _ = std::fs::remove_file(&actual_path);
    let _ = std::fs::remove_file(&raster_path);
    let _ = std::fs::remove_file(dir.join(format!("{name}.actual.svg")));
}

/// (pixels whose max channel difference exceeds `tol`, worst difference); straight alpha.
fn diff(a: &Pixmap, b: &Pixmap, tol: i32) -> (usize, i32) {
    let a = Pixmap::decode_png(&a.encode_png().unwrap()).unwrap();
    let b = Pixmap::decode_png(&b.encode_png().unwrap()).unwrap();
    let mut bad = 0;
    let mut worst = 0;
    for (x, y) in a
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.data().as_chunks::<4>().0)
    {
        let d = (0..4)
            .map(|i| (x[i] as i32 - y[i] as i32).abs())
            .max()
            .unwrap();
        worst = worst.max(d);
        if d > tol {
            bad += 1;
        }
    }
    (bad, worst)
}

/// Exact-text SVG golden.
fn check_svg_golden(name: &str, list: &DisplayList, viewport: Rect, scale: f32, bg: Color) {
    let svg = export(list, viewport, scale, bg, TextMode::Text);
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.svg"));
    let actual_path = dir.join(format!("{name}.actual.svg"));
    let update = std::env::var("PEN_EXPORT_UPDATE_GOLDEN").is_ok_and(|v| v == "1");
    if update || !path.exists() {
        std::fs::write(&path, &svg).unwrap();
        let _ = std::fs::remove_file(&actual_path);
        eprintln!("NOTICE: golden written: {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    if expected != svg {
        std::fs::write(&actual_path, &svg).unwrap();
        panic!(
            "{name}: SVG differs from golden (wrote {})",
            actual_path.display()
        );
    }
    let _ = std::fs::remove_file(&actual_path);
}

// ----------------------------------------------------------------- builders

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a)
}

fn at(x: f32, y: f32) -> Affine2 {
    Affine2::from_translation(Vec2::new(x, y))
}

fn shape_item(transform: Affine2, shape: Shape, fills: Vec<Paint>) -> DisplayItem {
    DisplayItem::Shape {
        node: NodeKey(0),
        transform,
        shape,
        fills: fills.into_iter().collect(),
        stroke: None,
        shadows: smallvec![],
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32, c: Color) -> DisplayItem {
    shape_item(
        at(x, y),
        Shape::Rect {
            size: Vec2::new(w, h),
            radii: CornerRadii::default(),
        },
        vec![Paint::Solid(c)],
    )
}

fn with_stroke(mut item: DisplayItem, s: Stroke) -> DisplayItem {
    if let DisplayItem::Shape { stroke, .. } = &mut item {
        *stroke = Some(s);
    }
    item
}

fn with_shadows(mut item: DisplayItem, sh: Vec<Shadow>) -> DisplayItem {
    if let DisplayItem::Shape { shadows, .. } = &mut item {
        *shadows = sh.into_iter().collect();
    }
    item
}

fn stroke(c: Color, w: f32, align: StrokeAlign) -> Stroke {
    Stroke {
        paints: smallvec![Paint::Solid(c)],
        widths: [w; 4],
        align,
        cap: LineCap::Butt,
        join: LineJoin::Miter,
        miter_limit: 4.0,
        dash: smallvec![],
    }
}

fn list(items: Vec<DisplayItem>) -> DisplayList {
    DisplayList::from_chunks([Chunk {
        root: NodeKey(0),
        bounds: Rect::new(-1e5, -1e5, 2e5, 2e5),
        version: 0,
        items,
    }])
}

fn stops2(a: Color, b: Color) -> smallvec::SmallVec<[GradientStop; 4]> {
    smallvec![
        GradientStop {
            offset: 0.0,
            color: a
        },
        GradientStop {
            offset: 1.0,
            color: b
        }
    ]
}

fn path(els: Vec<PathEl>, fill_rule: FillRule) -> Shape {
    Shape::Path(Arc::new(Path { els, fill_rule }))
}

fn star(cx: f32, cy: f32, r: f32) -> Vec<PathEl> {
    let mut els = Vec::new();
    for i in 0..5 {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * 4.0 * std::f32::consts::PI / 5.0;
        let p = Vec2::new(cx + r * a.cos(), cy + r * a.sin());
        els.push(if i == 0 {
            PathEl::MoveTo(p)
        } else {
            PathEl::LineTo(p)
        });
    }
    els.push(PathEl::Close);
    els
}

const VIEW: Rect = Rect::new(0.0, 0.0, 96.0, 64.0);
const BG: Color = Color::WHITE;

fn test_image() -> Arc<ImageData> {
    let (w, h) = (8u32, 4u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let c = match (x < w / 2, y < h / 2) {
                (true, true) => [230, 40, 40, 255],
                (false, true) => [40, 180, 60, 255],
                (true, false) => [40, 80, 220, 255],
                (false, false) => [240, 200, 30, 255],
            };
            let c = if x == y * 2 { [20, 20, 20, 255] } else { c };
            px.extend_from_slice(&c);
        }
    }
    Arc::new(ImageData {
        id: 7,
        width: w,
        height: h,
        rgba_premul: px,
    })
}

// ------------------------------------------------------------------- scenes

fn s01() -> DisplayList {
    list(vec![
        rect(8.0, 8.0, 40.0, 24.0, rgb(220, 60, 60)),
        rect(30.0, 20.0, 50.0, 36.0, rgba(40, 90, 220, 0.6)),
    ])
}

fn s02() -> DisplayList {
    list(vec![
        shape_item(
            at(6.0, 6.0),
            Shape::Rect {
                size: Vec2::new(40.0, 52.0),
                radii: CornerRadii([0.0, 8.0, 20.0, 4.0]),
            },
            vec![Paint::Solid(rgb(30, 140, 90))],
        ),
        shape_item(
            at(52.0, 6.0),
            Shape::Rect {
                size: Vec2::new(38.0, 24.0),
                radii: CornerRadii::uniform(100.0),
            },
            vec![Paint::Solid(rgb(200, 100, 30))],
        ),
    ])
}

fn s03() -> DisplayList {
    list(vec![
        shape_item(
            at(8.0, 8.0),
            Shape::Ellipse {
                size: Vec2::new(48.0, 48.0),
            },
            vec![Paint::Solid(rgb(120, 60, 200))],
        ),
        shape_item(
            at(50.0, 18.0),
            Shape::Ellipse {
                size: Vec2::new(40.0, 20.0),
            },
            vec![Paint::Solid(rgba(240, 160, 0, 0.8))],
        ),
    ])
}

fn s04() -> DisplayList {
    list(vec![
        shape_item(
            at(0.0, 0.0),
            path(star(24.0, 34.0, 26.0), FillRule::NonZero),
            vec![Paint::Solid(rgb(20, 20, 20))],
        ),
        shape_item(
            at(0.0, 0.0),
            path(star(72.0, 34.0, 26.0), FillRule::EvenOdd),
            vec![Paint::Solid(rgb(20, 20, 20))],
        ),
    ])
}

fn s05() -> DisplayList {
    let t = Affine2::from_translation(Vec2::new(48.0, 32.0))
        * Affine2::from_angle(0.4)
        * at(-36.0, -18.0);
    list(vec![shape_item(
        t,
        Shape::Rect {
            size: Vec2::new(72.0, 36.0),
            radii: CornerRadii::uniform(6.0),
        },
        vec![Paint::Linear {
            start: Vec2::new(0.0, 0.0),
            end: Vec2::new(72.0, 0.0),
            stops: smallvec![
                GradientStop {
                    offset: 0.0,
                    color: rgb(255, 92, 0)
                },
                GradientStop {
                    offset: 0.5,
                    color: rgb(255, 230, 120)
                },
                GradientStop {
                    offset: 1.0,
                    color: rgb(40, 60, 200)
                },
            ],
        }],
    )])
}

fn s10() -> DisplayList {
    let mk = |x: f32, align: StrokeAlign| {
        with_stroke(
            shape_item(
                at(x, 16.0),
                Shape::Rect {
                    size: Vec2::new(20.0, 32.0),
                    radii: CornerRadii::uniform(4.0),
                },
                vec![Paint::Solid(rgb(250, 210, 80))],
            ),
            stroke(rgba(0, 0, 160, 0.6), 6.0, align),
        )
    };
    list(vec![
        mk(8.0, StrokeAlign::Inside),
        mk(38.0, StrokeAlign::Center),
        mk(68.0, StrokeAlign::Outside),
    ])
}

fn s11() -> DisplayList {
    let mut s1 = stroke(rgb(200, 0, 0), 0.0, StrokeAlign::Inside);
    s1.widths = [0.0, 0.0, 4.0, 0.0];
    let mut s2 = stroke(rgb(0, 120, 0), 0.0, StrokeAlign::Center);
    s2.widths = [1.0, 2.0, 3.0, 6.0];
    let mut s3 = stroke(rgb(0, 0, 200), 0.0, StrokeAlign::Outside);
    s3.widths = [2.0, 0.0, 2.0, 0.0];
    let fill = rgb(235, 235, 235);
    list(vec![
        with_stroke(rect(6.0, 8.0, 24.0, 20.0, fill), s1),
        with_stroke(
            shape_item(
                at(38.0, 8.0),
                Shape::Rect {
                    size: Vec2::new(24.0, 20.0),
                    radii: CornerRadii::uniform(6.0),
                },
                vec![Paint::Solid(fill)],
            ),
            s2,
        ),
        with_stroke(rect(70.0, 8.0, 20.0, 20.0, fill), s3),
    ])
}

fn s17() -> DisplayList {
    list(vec![
        DisplayItem::PushClip {
            transform: at(8.0, 8.0),
            shape: Shape::Rect {
                size: Vec2::new(80.0, 48.0),
                radii: CornerRadii::uniform(14.0),
            },
        },
        rect(0.0, 0.0, 96.0, 64.0, rgb(60, 60, 90)),
        DisplayItem::PushClip {
            transform: at(30.0, 4.0),
            shape: Shape::Ellipse {
                size: Vec2::new(70.0, 70.0),
            },
        },
        rect(0.0, 0.0, 96.0, 32.0, rgb(255, 160, 0)),
        rect(0.0, 32.0, 96.0, 32.0, rgb(0, 200, 160)),
        DisplayItem::PopClip,
        rect(0.0, 28.0, 96.0, 8.0, rgba(255, 255, 255, 0.8)),
        DisplayItem::PopClip,
        rect(0.0, 58.0, 96.0, 6.0, rgb(200, 0, 0)),
    ])
}

fn s18() -> DisplayList {
    list(vec![
        DisplayItem::PushLayer {
            opacity: 0.5,
            blend: BlendMode::Normal,
            blur: 0.0,
            backdrop_blur: 0.0,
            bounds: Rect::new(4.0, 4.0, 44.0, 56.0),
        },
        rect(4.0, 4.0, 30.0, 40.0, rgb(0, 0, 200)),
        rect(18.0, 20.0, 30.0, 40.0, rgb(0, 0, 200)),
        DisplayItem::PopLayer,
        rect(52.0, 4.0, 30.0, 40.0, rgba(0, 0, 200, 0.5)),
        rect(64.0, 20.0, 30.0, 40.0, rgba(0, 0, 200, 0.5)),
    ])
}

// ------------------------------------------------------------- pixel tests

#[test]
fn g01_rect_solid() {
    check("01_rect_solid", &s01(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g02_corner_radii() {
    check("02_corner_radii", &s02(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g03_ellipse() {
    check("03_ellipse", &s03(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g04_path_fill_rules() {
    check("04_path_fill_rules", &s04(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g05_linear_gradient() {
    check("05_linear_gradient", &s05(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g06_radial_gradient() {
    let l = list(vec![shape_item(
        at(4.0, 4.0),
        Shape::Rect {
            size: Vec2::new(88.0, 56.0),
            radii: CornerRadii::default(),
        },
        vec![Paint::Radial {
            center: Vec2::new(44.0, 28.0),
            radius: Vec2::new(44.0, 20.0),
            stops: stops2(rgb(255, 255, 255), rgb(0, 80, 160)),
        }],
    )]);
    check("06_radial_gradient", &l, VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g07_angular_gradient() {
    let l = list(vec![shape_item(
        at(16.0, 0.0),
        Shape::Ellipse {
            size: Vec2::new(64.0, 64.0),
        },
        vec![Paint::Angular {
            center: Vec2::new(32.0, 32.0),
            start_angle: -std::f32::consts::FRAC_PI_2,
            stops: smallvec![
                GradientStop {
                    offset: 0.0,
                    color: rgb(255, 0, 0)
                },
                GradientStop {
                    offset: 0.33,
                    color: rgb(0, 200, 0)
                },
                GradientStop {
                    offset: 0.66,
                    color: rgb(0, 0, 255)
                },
                GradientStop {
                    offset: 1.0,
                    color: rgb(255, 0, 0)
                },
            ],
        }],
    )]);
    check("07_angular_gradient", &l, VIEW, 1.0, BG, BLUR);
}

#[test]
fn g08_multiple_fills() {
    let l = list(vec![shape_item(
        at(8.0, 8.0),
        Shape::Rect {
            size: Vec2::new(80.0, 48.0),
            radii: CornerRadii::uniform(10.0),
        },
        vec![
            Paint::Solid(rgb(30, 30, 60)),
            Paint::Linear {
                start: Vec2::new(0.0, 0.0),
                end: Vec2::new(0.0, 48.0),
                stops: stops2(rgba(255, 255, 255, 0.0), rgba(255, 120, 200, 0.9)),
            },
            Paint::Radial {
                center: Vec2::new(20.0, 14.0),
                radius: Vec2::new(16.0, 16.0),
                stops: stops2(rgba(255, 255, 0, 0.8), rgba(255, 255, 0, 0.0)),
            },
        ],
    )]);
    check("08_multiple_fills", &l, VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g09_image_fits() {
    let img = test_image();
    let mk = |x: f32, y: f32, fit: ImageFit, opacity: f32, scale: f32| {
        with_stroke(
            shape_item(
                at(x, y),
                Shape::Rect {
                    size: Vec2::new(40.0, 24.0),
                    radii: CornerRadii::uniform(4.0),
                },
                vec![
                    Paint::Solid(rgb(220, 220, 220)),
                    Paint::Image {
                        image: img.clone(),
                        fit,
                        opacity,
                        scale,
                    },
                ],
            ),
            stroke(rgb(0, 0, 0), 1.0, StrokeAlign::Inside),
        )
    };
    let l = list(vec![
        mk(4.0, 4.0, ImageFit::Cover, 1.0, 1.0),
        mk(52.0, 4.0, ImageFit::Contain, 1.0, 1.0),
        mk(4.0, 36.0, ImageFit::Stretch, 0.5, 1.0),
        mk(52.0, 36.0, ImageFit::Tile, 1.0, 1.5),
    ]);
    // Upscaled 8x4 image: resvg filters magnified pixels bicubically, pen-raster bilinearly,
    // so the soft edges between the image's color blocks differ.
    let image_tol = Tol {
        channel: 32,
        fraction: 0.1,
    };
    check("09_image_fits", &l, VIEW, 1.0, BG, image_tol);
}

#[test]
fn g10_stroke_align() {
    check("10_stroke_align", &s10(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g11_stroke_per_side() {
    check("11_stroke_per_side", &s11(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g12_caps_and_joins() {
    let zig = |y: f32| {
        vec![
            PathEl::MoveTo(Vec2::new(0.0, y + 12.0)),
            PathEl::LineTo(Vec2::new(10.0, y)),
            PathEl::LineTo(Vec2::new(20.0, y + 12.0)),
        ]
    };
    let mut items = Vec::new();
    let combos = [
        (LineCap::Butt, LineJoin::Miter),
        (LineCap::Round, LineJoin::Round),
        (LineCap::Square, LineJoin::Bevel),
    ];
    for (i, (cap, join)) in combos.iter().enumerate() {
        let mut s = stroke(rgb(30, 30, 30), 5.0, StrokeAlign::Center);
        s.cap = *cap;
        s.join = *join;
        let x = 8.0 + i as f32 * 30.0;
        items.push(with_stroke(
            shape_item(at(x, 0.0), path(zig(10.0), FillRule::NonZero), vec![]),
            s,
        ));
        items.push(with_stroke(
            shape_item(at(x, 0.0), path(zig(10.0), FillRule::NonZero), vec![]),
            stroke(rgb(255, 0, 0), 0.5, StrokeAlign::Center),
        ));
    }
    for (x, limit, c) in [(10.0, 1.5, rgb(0, 90, 200)), (50.0, 10.0, rgb(0, 160, 90))] {
        let mut s = stroke(c, 4.0, StrokeAlign::Center);
        s.miter_limit = limit;
        items.push(with_stroke(
            shape_item(
                at(x, 0.0),
                path(
                    vec![
                        PathEl::MoveTo(Vec2::new(0.0, 56.0)),
                        PathEl::LineTo(Vec2::new(36.0, 40.0)),
                        PathEl::LineTo(Vec2::new(0.0, 44.0)),
                    ],
                    FillRule::NonZero,
                ),
                vec![],
            ),
            s,
        ));
    }
    check("12_caps_joins", &list(items), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g13_dashes() {
    let mut s = stroke(rgb(40, 40, 40), 2.0, StrokeAlign::Center);
    s.dash = smallvec![6.0, 3.0];
    let mut s2 = stroke(rgb(200, 40, 120), 3.0, StrokeAlign::Inside);
    s2.dash = smallvec![4.0, 5.0];
    s2.cap = LineCap::Round;
    let l = list(vec![
        with_stroke(
            shape_item(
                at(8.0, 8.0),
                Shape::Rect {
                    size: Vec2::new(36.0, 48.0),
                    radii: CornerRadii::uniform(6.0),
                },
                vec![],
            ),
            s,
        ),
        with_stroke(
            shape_item(
                at(52.0, 12.0),
                Shape::Ellipse {
                    size: Vec2::new(40.0, 40.0),
                },
                vec![],
            ),
            s2,
        ),
    ]);
    // Dash phase along arcs depends on the curve length estimate (arcs vs cubics).
    check("13_dashes", &l, VIEW, 1.0, BG, BLUR);
}

#[test]
fn g14_path_stroke_inside_outside() {
    let tri = |cx: f32| {
        vec![
            PathEl::MoveTo(Vec2::new(cx, 10.0)),
            PathEl::LineTo(Vec2::new(cx + 18.0, 50.0)),
            PathEl::LineTo(Vec2::new(cx - 18.0, 50.0)),
            PathEl::Close,
        ]
    };
    let fill = vec![Paint::Solid(rgb(190, 230, 255))];
    let mut s = stroke(rgb(255, 0, 0), 3.0, StrokeAlign::Outside);
    s.paints = smallvec![Paint::Linear {
        start: Vec2::ZERO,
        end: Vec2::new(16.0, 0.0),
        stops: stops2(rgb(255, 0, 0), rgb(0, 0, 255)),
    }];
    let l = list(vec![
        with_stroke(
            shape_item(
                at(0.0, 0.0),
                path(tri(24.0), FillRule::NonZero),
                fill.clone(),
            ),
            stroke(rgba(0, 0, 0, 0.7), 4.0, StrokeAlign::Inside),
        ),
        with_stroke(
            shape_item(
                at(0.0, 0.0),
                path(tri(70.0), FillRule::NonZero),
                fill.clone(),
            ),
            stroke(rgba(0, 0, 0, 0.7), 4.0, StrokeAlign::Outside),
        ),
        with_stroke(
            shape_item(
                at(40.0, 50.0),
                Shape::Ellipse {
                    size: Vec2::new(16.0, 10.0),
                },
                fill,
            ),
            s,
        ),
    ]);
    check("14_path_stroke_align", &l, VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g15_drop_shadow() {
    let a = with_shadows(
        shape_item(
            at(14.0, 10.0),
            Shape::Rect {
                size: Vec2::new(30.0, 30.0),
                radii: CornerRadii::uniform(6.0),
            },
            vec![Paint::Solid(rgb(255, 255, 255))],
        ),
        vec![Shadow::Drop {
            offset: Vec2::new(0.0, 6.0),
            blur: 12.0,
            spread: 0.0,
            color: rgba(0, 0, 0, 0.5),
        }],
    );
    let b = with_shadows(
        shape_item(
            at(0.0, 0.0),
            path(star(72.0, 30.0, 18.0), FillRule::NonZero),
            vec![Paint::Solid(rgb(250, 200, 60))],
        ),
        vec![
            Shadow::Drop {
                offset: Vec2::new(3.0, 3.0),
                blur: 0.0,
                spread: 2.0,
                color: rgb(200, 0, 0),
            },
            Shadow::Drop {
                offset: Vec2::new(0.0, 0.0),
                blur: 8.0,
                spread: 0.0,
                color: rgba(0, 0, 255, 0.6),
            },
        ],
    );
    check(
        "15_drop_shadow",
        &list(vec![a, b]),
        VIEW,
        1.0,
        rgb(230, 230, 235),
        BLUR,
    );
}

#[test]
fn g16_inner_shadow() {
    let a = with_shadows(
        shape_item(
            at(8.0, 8.0),
            Shape::Rect {
                size: Vec2::new(36.0, 48.0),
                radii: CornerRadii::uniform(10.0),
            },
            vec![Paint::Solid(rgb(120, 200, 255))],
        ),
        vec![Shadow::Inner {
            offset: Vec2::new(4.0, 4.0),
            blur: 6.0,
            spread: 0.0,
            color: rgba(0, 0, 60, 0.7),
        }],
    );
    let b = with_shadows(
        shape_item(
            at(52.0, 12.0),
            Shape::Ellipse {
                size: Vec2::new(40.0, 40.0),
            },
            vec![Paint::Solid(rgb(255, 230, 200))],
        ),
        vec![
            Shadow::Drop {
                offset: Vec2::new(0.0, 2.0),
                blur: 4.0,
                spread: 0.0,
                color: rgba(0, 0, 0, 0.4),
            },
            Shadow::Inner {
                offset: Vec2::new(0.0, 0.0),
                blur: 4.0,
                spread: 3.0,
                color: rgb(200, 80, 0),
            },
        ],
    );
    check("16_inner_shadow", &list(vec![a, b]), VIEW, 1.0, BG, BLUR);
}

#[test]
fn g17_nested_clips() {
    check("17_nested_clips", &s17(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g18_layer_opacity() {
    check("18_layer_opacity", &s18(), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g19_blend_modes() {
    let modes = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];
    let mut items = vec![shape_item(
        at(0.0, 0.0),
        Shape::Rect {
            size: Vec2::new(96.0, 64.0),
            radii: CornerRadii::default(),
        },
        vec![Paint::Linear {
            start: Vec2::ZERO,
            end: Vec2::new(96.0, 0.0),
            stops: stops2(rgb(20, 120, 220), rgb(240, 200, 40)),
        }],
    )];
    for (i, m) in modes.iter().enumerate() {
        let (cx, cy) = ((i % 4) as f32 * 24.0, (i / 4) as f32 * 16.0);
        let b = Rect::new(cx + 2.0, cy + 2.0, 20.0, 12.0);
        items.push(DisplayItem::PushLayer {
            opacity: 1.0,
            blend: *m,
            blur: 0.0,
            backdrop_blur: 0.0,
            bounds: b,
        });
        items.push(rect(b.x, b.y, b.w, b.h, rgb(220, 60, 120)));
        items.push(DisplayItem::PopLayer);
    }
    check("19_blend_modes", &list(items), VIEW, 1.0, BG, TIGHT);
}

#[test]
fn g20_layer_blur() {
    let l = list(vec![
        DisplayItem::PushLayer {
            opacity: 1.0,
            blend: BlendMode::Normal,
            blur: 6.0,
            backdrop_blur: 0.0,
            bounds: Rect::new(10.0, 10.0, 76.0, 44.0),
        },
        rect(10.0, 10.0, 36.0, 44.0, rgb(220, 30, 30)),
        shape_item(
            at(50.0, 14.0),
            Shape::Ellipse {
                size: Vec2::new(36.0, 36.0),
            },
            vec![Paint::Solid(rgb(30, 30, 220))],
        ),
        DisplayItem::PopLayer,
    ]);
    check("20_layer_blur", &l, VIEW, 1.0, BG, BLUR);
}

#[test]
fn g21_backdrop_blur() {
    let mut items = Vec::new();
    for i in 0..12 {
        let c = if i % 2 == 0 {
            rgb(20, 20, 20)
        } else {
            rgb(250, 250, 250)
        };
        items.push(rect(i as f32 * 8.0, 0.0, 8.0, 64.0, c));
    }
    items.push(DisplayItem::PushClip {
        transform: at(16.0, 12.0),
        shape: Shape::Rect {
            size: Vec2::new(64.0, 40.0),
            radii: CornerRadii::uniform(10.0),
        },
    });
    items.push(DisplayItem::PushLayer {
        opacity: 1.0,
        blend: BlendMode::Normal,
        blur: 0.0,
        backdrop_blur: 8.0,
        bounds: Rect::new(16.0, 12.0, 64.0, 40.0),
    });
    items.push(shape_item(
        at(16.0, 12.0),
        Shape::Rect {
            size: Vec2::new(64.0, 40.0),
            radii: CornerRadii::uniform(10.0),
        },
        vec![Paint::Solid(rgba(255, 255, 255, 0.25))],
    ));
    items.push(DisplayItem::PopLayer);
    items.push(DisplayItem::PopClip);
    check("21_backdrop_blur", &list(items), VIEW, 1.0, BG, BLUR);
}

#[test]
fn g22_transforms_and_scale() {
    let t = Affine2::from_translation(Vec2::new(24.0, 16.0))
        * Affine2::from_angle(std::f32::consts::FRAC_PI_6)
        * Affine2::from_scale(Vec2::new(1.5, 0.75));
    let l = list(vec![
        with_stroke(
            shape_item(
                t,
                Shape::Rect {
                    size: Vec2::new(16.0, 16.0),
                    radii: CornerRadii::uniform(3.0),
                },
                vec![Paint::Solid(rgb(90, 160, 60))],
            ),
            stroke(rgb(0, 0, 0), 1.0, StrokeAlign::Inside),
        ),
        shape_item(
            Affine2::from_translation(Vec2::new(10.0, 20.0)) * Affine2::from_angle(-0.5),
            Shape::Ellipse {
                size: Vec2::new(14.0, 6.0),
            },
            vec![Paint::Solid(rgb(200, 50, 50))],
        ),
    ]);
    check(
        "22_transforms_scale",
        &l,
        Rect::new(4.0, 4.0, 48.0, 32.0),
        2.0,
        BG,
        TIGHT,
    );
}

// --------------------------------------------------------------------- text

fn text_block(
    text: &str,
    runs: Vec<(std::ops::Range<usize>, TextStyle)>,
    lines: Vec<TextLine>,
    size: Vec2,
) -> Arc<TextBlock> {
    Arc::new(TextBlock {
        text: text.into(),
        runs: runs
            .into_iter()
            .map(|(range, style)| TextRun { range, style })
            .collect(),
        lines,
        align: TextAlign::Left,
        size,
        clip: false,
    })
}

fn style(family: &str, size: f32, weight: u16, color: Color) -> TextStyle {
    TextStyle {
        family: family.into(),
        size,
        weight,
        italic: false,
        letter_spacing: 0.0,
        underline: false,
        strikethrough: false,
        paint: Paint::Solid(color),
    }
}

fn text_font() -> Option<&'static str> {
    let mut c = ctx().lock().unwrap_or_else(|e| e.into_inner());
    ["Arial"]
        .into_iter()
        .find(|f| c.has_font_family(f))
        .or_else(|| {
            eprintln!("NOTICE: skipping text test (font 'Arial' not installed)");
            None
        })
}

fn s23(font: &str) -> DisplayList {
    let text = "Hello Bold italic\nunder strike spaced";
    let bold = style(font, 14.0, 700, rgb(200, 30, 30));
    let mut ital = style(font, 12.0, 400, rgb(30, 30, 200));
    ital.italic = true;
    let mut under = style(font, 12.0, 400, rgb(0, 0, 0));
    under.underline = true;
    let mut strike = style(font, 12.0, 400, rgb(0, 120, 0));
    strike.strikethrough = true;
    let mut spaced = style(font, 12.0, 400, rgb(90, 90, 90));
    spaced.letter_spacing = 2.0;
    let block = text_block(
        text,
        vec![
            (0..6, style(font, 14.0, 400, rgb(0, 0, 0))),
            (6..11, bold),
            (11..17, ital),
            (18..24, under),
            (24..31, strike),
            (31..37, spaced),
        ],
        vec![
            TextLine {
                range: 0..17,
                x: 2.0,
                baseline: 16.0,
                width: 0.0,
            },
            TextLine {
                range: 18..37,
                x: 2.0,
                baseline: 38.0,
                width: 0.0,
            },
        ],
        Vec2::new(150.0, 48.0),
    );
    list(vec![DisplayItem::Text {
        node: NodeKey(1),
        transform: at(4.0, 4.0),
        block,
    }])
}

fn s24(font: &str) -> DisplayList {
    let mut s = style(font, 22.0, 700, Color::BLACK);
    s.paint = Paint::Linear {
        start: Vec2::ZERO,
        end: Vec2::new(90.0, 0.0),
        stops: stops2(rgb(255, 60, 0), rgb(120, 0, 255)),
    };
    let block = text_block(
        "Gradient",
        vec![(0..8, s)],
        vec![TextLine {
            range: 0..8,
            x: 0.0,
            baseline: 22.0,
            width: 0.0,
        }],
        Vec2::new(100.0, 28.0),
    );
    let t = Affine2::from_translation(Vec2::new(10.0, 14.0)) * Affine2::from_angle(0.25);
    let mut s2 = style(font, 11.0, 400, rgba(0, 0, 0, 0.8));
    s2.letter_spacing = 0.5;
    let block2 = text_block(
        "clip me please",
        vec![(0..14, s2)],
        vec![TextLine {
            range: 0..14,
            x: 0.0,
            baseline: 11.0,
            width: 0.0,
        }],
        Vec2::new(40.0, 14.0),
    );
    let block2 = Arc::new(TextBlock {
        clip: true,
        ..(*block2).clone()
    });
    list(vec![
        DisplayItem::Text {
            node: NodeKey(1),
            transform: t,
            block,
        },
        rect(4.0, 46.0, 40.0, 14.0, rgb(240, 240, 200)),
        DisplayItem::Text {
            node: NodeKey(2),
            transform: at(4.0, 46.0),
            block: block2,
        },
    ])
}

#[test]
fn g23_text_runs() {
    let Some(font) = text_font() else { return };
    let vp = Rect::new(0.0, 0.0, 160.0, 56.0);
    let l = s23(font);
    check_mode(
        "23_text_runs_outlines",
        &l,
        vp,
        1.0,
        BG,
        BLUR,
        TextMode::Outlines,
    );
    check_mode("23_text_runs", &l, vp, 1.0, BG, TEXT, TextMode::Text);
}

#[test]
fn g24_text_transformed() {
    let Some(font) = text_font() else { return };
    let vp = Rect::new(0.0, 0.0, 120.0, 64.0);
    let l = s24(font);
    check_mode(
        "24_text_transformed_outlines",
        &l,
        vp,
        1.0,
        BG,
        BLUR,
        TextMode::Outlines,
    );
    check_mode("24_text_transformed", &l, vp, 1.0, BG, TEXT, TextMode::Text);
}

// -------------------------------------------------------------- SVG goldens

#[test]
fn svg_goldens() {
    check_svg_golden("01_rect_solid", &s01(), VIEW, 1.0, BG);
    check_svg_golden("02_corner_radii", &s02(), VIEW, 1.0, BG);
    check_svg_golden("03_ellipse", &s03(), VIEW, 1.0, BG);
    check_svg_golden("04_path_fill_rules", &s04(), VIEW, 1.0, BG);
    check_svg_golden("05_linear_gradient", &s05(), VIEW, 1.0, BG);
    check_svg_golden("10_stroke_align", &s10(), VIEW, 1.0, BG);
    check_svg_golden("11_stroke_per_side", &s11(), VIEW, 1.0, BG);
    check_svg_golden("17_nested_clips", &s17(), VIEW, 1.0, BG);
    check_svg_golden("18_layer_opacity", &s18(), VIEW, 1.0, BG);
}

// ------------------------------------------------------------- API checks

#[test]
fn output_is_deterministic() {
    let l = s17();
    let a = export(&l, VIEW, 1.0, BG, TextMode::Text);
    let b = export(&l, VIEW, 1.0, BG, TextMode::Text);
    assert_eq!(a, b);
}

#[test]
fn region_defaults_to_selected_chunk_bounds() {
    let mut l = s01();
    l.chunks[0] = Arc::new(Chunk {
        bounds: Rect::new(8.0, 8.0, 72.0, 48.0),
        ..(*l.chunks[0]).clone()
    });
    l.chunks.push(Arc::new(Chunk {
        root: NodeKey(5),
        bounds: Rect::new(200.0, 0.0, 10.0, 10.0),
        version: 0,
        items: vec![rect(200.0, 0.0, 10.0, 10.0, rgb(0, 0, 0))],
    }));
    let opts = ExportOptions {
        roots: Some(vec![NodeKey(0)]),
        scale: 2.0,
        ..ExportOptions::default()
    };
    assert_eq!(opts.resolve_region(&l), Rect::new(8.0, 8.0, 72.0, 48.0));
    let svg = pen_export::svg::export(&l, &opts);
    assert!(svg.contains("width=\"144\" height=\"96\" viewBox=\"8 8 72 48\""));
    assert!(
        !svg.contains("translate(200 0)"),
        "unselected chunk exported"
    );
    // Whole document: both chunks.
    let all = ExportOptions::default().resolve_region(&l);
    assert_eq!(all, Rect::new(8.0, 0.0, 202.0, 56.0));
}

#[test]
fn unbalanced_scopes_produce_well_formed_svg() {
    let l = list(vec![
        DisplayItem::PopClip,
        DisplayItem::PopLayer,
        DisplayItem::PushLayer {
            opacity: 0.5,
            blend: BlendMode::Multiply,
            blur: 2.0,
            backdrop_blur: 0.0,
            bounds: Rect::default(),
        },
        DisplayItem::PushClip {
            transform: Affine2::IDENTITY,
            shape: Shape::Ellipse {
                size: Vec2::new(10.0, 10.0),
            },
        },
        rect(0.0, 0.0, 10.0, 10.0, rgb(0, 0, 0)),
        DisplayItem::PushClip {
            transform: Affine2::IDENTITY,
            shape: Shape::Rect {
                size: Vec2::ZERO,
                radii: CornerRadii::default(),
            },
        },
        rect(0.0, 0.0, 10.0, 10.0, rgb(255, 0, 0)),
    ]);
    let svg = export(&l, Rect::new(0.0, 0.0, 10.0, 10.0), 1.0, BG, TextMode::Text);
    let p = rasterize_svg(&svg);
    let c = p.pixel(5, 5).unwrap();
    assert!(c.red() < 200 && c.red() == c.green(), "got {c:?}");
}
