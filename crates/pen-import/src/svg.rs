//! Inline SVG -> flat list of paths (via usvg), or a raster when the SVG uses
//! features the `.pen` model can't express (masks, filters, patterns, images).

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use resvg::tiny_skia;
use resvg::usvg::{self, Node, Paint, Transform};
use serde_json::{Value, json};

use crate::css::Color;

/// One flattened path in the SVG element's own pixel space.
#[derive(Debug, Clone)]
pub struct FlatPath {
    pub d: String,
    /// `[x, y, w, h]` of the geometry.
    pub bbox: [f64; 4],
    pub fill: Option<Value>,
    pub evenodd: bool,
    pub stroke: Option<Value>,
    pub stroke_width: f64,
    pub linecap: &'static str,
    pub linejoin: &'static str,
    pub opacity: f64,
}

pub struct Flattened {
    pub paths: Vec<FlatPath>,
    /// Unsupported features were found: a raster renders it more faithfully.
    pub complex: bool,
}

/// Paths above this count are rasterized instead (when assets can be written).
pub const MAX_PATHS: usize = 300;

fn options() -> usvg::Options<'static> {
    usvg::Options::default()
}

/// Options with system fonts, only built when an SVG actually contains text.
fn text_options() -> usvg::Options<'static> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let db = DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });
    usvg::Options {
        fontdb: db.clone(),
        ..usvg::Options::default()
    }
}

pub fn parse(markup: &str) -> Option<usvg::Tree> {
    let opts = if markup.contains("<text") {
        text_options()
    } else {
        options()
    };
    usvg::Tree::from_str(markup, &opts).ok()
}

/// Flattens `tree`; `override_fill` paints every shape with one color (mask icons).
pub fn flatten(tree: &usvg::Tree, override_fill: Option<Color>) -> Flattened {
    let mut out = Flattened {
        paths: Vec::new(),
        complex: false,
    };
    walk(tree.root(), 1.0, override_fill, &mut out);
    out
}

fn walk(g: &usvg::Group, opacity: f64, ov: Option<Color>, out: &mut Flattened) {
    let opacity = opacity * g.opacity().get() as f64;
    if g.mask().is_some() || !g.filters().is_empty() {
        out.complex = true;
    }
    if g.clip_path().is_some() && ov.is_none() {
        out.complex = true;
    }
    for n in g.children() {
        match n {
            Node::Group(c) => walk(c, opacity, ov, out),
            Node::Text(t) => walk(t.flattened(), opacity, ov, out),
            Node::Image(_) => out.complex = true,
            Node::Path(p) => {
                if !p.is_visible() {
                    continue;
                }
                if let Some(fp) = path(p, opacity, ov, out) {
                    out.paths.push(fp);
                }
            }
        }
    }
}

fn path(p: &usvg::Path, opacity: f64, ov: Option<Color>, out: &mut Flattened) -> Option<FlatPath> {
    let ts = p.abs_transform();
    let data = p.data().clone().transform(ts)?;
    let b = data.bounds();
    let bbox = [
        b.left() as f64,
        b.top() as f64,
        (b.width() as f64).max(0.01),
        (b.height() as f64).max(0.01),
    ];
    let mut d = String::new();
    let r = |v: f32| (v as f64 * 1000.0).round() / 1000.0;
    for seg in data.segments() {
        use tiny_skia::PathSegment as S;
        let _ = match seg {
            S::MoveTo(a) => write!(d, "M{} {}", r(a.x), r(a.y)),
            S::LineTo(a) => write!(d, "L{} {}", r(a.x), r(a.y)),
            S::QuadTo(a, b) => write!(d, "Q{} {} {} {}", r(a.x), r(a.y), r(b.x), r(b.y)),
            S::CubicTo(a, b, c) => write!(
                d,
                "C{} {} {} {} {} {}",
                r(a.x),
                r(a.y),
                r(b.x),
                r(b.y),
                r(c.x),
                r(c.y)
            ),
            S::Close => write!(d, "Z"),
        };
    }
    let scale = {
        let det = (ts.sx * ts.sy - ts.kx * ts.ky).abs() as f64;
        det.sqrt().max(1e-6)
    };
    let (fill, evenodd) = match (p.fill(), ov) {
        (Some(f), Some(c)) => (
            Some(Value::String(
                c.with_alpha(c.a * f.opacity().get() as f64).hex(),
            )),
            f.rule() == usvg::FillRule::EvenOdd,
        ),
        (Some(f), None) => (
            paint(f.paint(), f.opacity().get() as f64, ts, bbox, out),
            f.rule() == usvg::FillRule::EvenOdd,
        ),
        (None, _) => (None, false),
    };
    let (stroke, stroke_width, linecap, linejoin) = match p.stroke() {
        Some(s) => {
            let paint = match ov {
                Some(c) => Some(Value::String(c.hex())),
                None => paint(s.paint(), s.opacity().get() as f64, ts, bbox, out),
            };
            if s.dasharray().is_some() {
                out.complex = true;
            }
            (
                paint,
                s.width().get() as f64 * scale,
                match s.linecap() {
                    usvg::LineCap::Butt => "butt",
                    usvg::LineCap::Round => "round",
                    usvg::LineCap::Square => "square",
                },
                match s.linejoin() {
                    usvg::LineJoin::Round => "round",
                    usvg::LineJoin::Bevel => "bevel",
                    _ => "miter",
                },
            )
        }
        None => (None, 0.0, "butt", "miter"),
    };
    if fill.is_none() && stroke.is_none() {
        return None;
    }
    Some(FlatPath {
        d,
        bbox,
        fill,
        evenodd,
        stroke,
        stroke_width,
        linecap,
        linejoin,
        opacity,
    })
}

fn rgb(c: usvg::Color, a: f64) -> Color {
    Color {
        r: c.red as f64 / 255.0,
        g: c.green as f64 / 255.0,
        b: c.blue as f64 / 255.0,
        a,
    }
}

fn stops(stops: &[usvg::Stop], opacity: f64) -> Vec<Value> {
    stops
        .iter()
        .map(|s| {
            json!({
                "color": rgb(s.color(), s.opacity().get() as f64 * opacity).hex(),
                "position": round4(s.offset().get() as f64),
            })
        })
        .collect()
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn paint(
    p: &Paint,
    opacity: f64,
    ts: Transform,
    bbox: [f64; 4],
    out: &mut Flattened,
) -> Option<Value> {
    match p {
        Paint::Color(c) => Some(Value::String(rgb(*c, opacity).hex())),
        Paint::LinearGradient(g) => {
            let t = ts.pre_concat(g.transform());
            let mut a = tiny_skia::Point::from_xy(g.x1(), g.y1());
            let mut b = tiny_skia::Point::from_xy(g.x2(), g.y2());
            t.map_point(&mut a);
            t.map_point(&mut b);
            Some(crate::convert::linear_paint(
                (a.x as f64 - bbox[0], a.y as f64 - bbox[1]),
                (b.x as f64 - bbox[0], b.y as f64 - bbox[1]),
                bbox[2],
                bbox[3],
                stops(g.stops(), opacity),
            ))
        }
        Paint::RadialGradient(g) => {
            let t = ts.pre_concat(g.transform());
            let mut c = tiny_skia::Point::from_xy(g.cx(), g.cy());
            t.map_point(&mut c);
            let s = ((t.sx * t.sy - t.kx * t.ky).abs() as f64).sqrt();
            let r = g.r().get() as f64 * s;
            Some(json!({
                "type": "gradient",
                "gradientType": "radial",
                "center": {
                    "x": round4((c.x as f64 - bbox[0]) / bbox[2]),
                    "y": round4((c.y as f64 - bbox[1]) / bbox[3]),
                },
                "size": {
                    "width": round4(2.0 * r / bbox[2]),
                    "height": round4(2.0 * r / bbox[3]),
                },
                "colors": stops(g.stops(), opacity),
            }))
        }
        Paint::Pattern(_) => {
            out.complex = true;
            None
        }
    }
}

/// Renders `tree` to PNG at `scale` (for SVGs too complex to flatten).
pub fn rasterize(tree: &usvg::Tree, scale: f32) -> Option<Vec<u8>> {
    let size = tree.size();
    let w = (size.width() * scale).ceil().max(1.0) as u32;
    let h = (size.height() * scale).ceil().max(1.0) as u32;
    if w > 8192 || h > 8192 {
        return None;
    }
    let mut pm = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(tree, Transform::from_scale(scale, scale), &mut pm.as_mut());
    pm.encode_png().ok()
}

/// Renders standalone SVG bytes (an `<img src=*.svg>`) to PNG, at least `min` px wide.
pub fn svg_bytes_to_png(bytes: &[u8], min_w: f64) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let tree = parse(text)?;
    let w = tree.size().width() as f64;
    let scale = if w > 0.0 {
        (min_w * 2.0 / w).clamp(1.0, 8.0)
    } else {
        2.0
    };
    rasterize(&tree, scale as f32)
}
