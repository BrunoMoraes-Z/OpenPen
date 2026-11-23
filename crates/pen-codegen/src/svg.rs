//! Inline SVG for vector layers (paths, polygons, lines, partial ellipses).

use pen_doc::{GradientKind, ImageMode, PaintSpec, SceneNode};
use pen_render::{FillRule, LineCap, LineJoin, Path, PathEl, StrokeAlign, Vec2};

use crate::build::{Builder, blend_css};
use crate::paint::{gradient_color, hex_opaque, linear_points, num, path_data, radial_axes};
use crate::vars::Binds;

fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// `fill="..."` / `stroke="..."` attributes of a paint; gradient and pattern
/// definitions go to `defs`.
fn paint_attrs(
    b: &mut Builder<'_>,
    n: &SceneNode,
    attr: &str,
    paint: &PaintSpec,
    var: Option<&str>,
    size: Vec2,
    defs: &mut Vec<String>,
) -> Option<String> {
    let solid = |c: pen_render::Color| {
        let (h, a) = hex_opaque(c);
        if a < 1.0 {
            format!("{attr}=\"{h}\" {attr}-opacity=\"{}\"", num(a, 3))
        } else {
            format!("{attr}=\"{h}\"")
        }
    };
    Some(match paint {
        PaintSpec::Solid(c) => match b.vars.color(var) {
            Some(v) => format!("style=\"{attr}: {v}\""),
            None => solid(*c),
        },
        PaintSpec::Gradient(g) => {
            let stops: Vec<String> = g
                .stops
                .iter()
                .map(|(o, c)| {
                    let (h, a) = hex_opaque(*c);
                    let op = if a < 1.0 {
                        format!(" stop-opacity=\"{}\"", num(a, 3))
                    } else {
                        String::new()
                    };
                    format!("  <stop offset=\"{}\" stop-color=\"{h}\"{op}/>", num(*o, 4))
                })
                .collect();
            let id = match g.kind {
                GradientKind::Linear => {
                    let id = b.new_id("pen-g");
                    let (s, e) = linear_points(g, size);
                    defs.push(format!(
                        "<linearGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\">",
                        num(s.x, 2),
                        num(s.y, 2),
                        num(e.x, 2),
                        num(e.y, 2)
                    ));
                    defs.extend(stops);
                    defs.push("</linearGradient>".into());
                    id
                }
                GradientKind::Radial => {
                    let id = b.new_id("pen-g");
                    let (c, ax, ay) = radial_axes(g, size);
                    defs.push(format!(
                        "<radialGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" cx=\"0\" cy=\"0\" r=\"1\" gradientTransform=\"matrix({} {} {} {} {} {})\">",
                        num(ax.x, 3),
                        num(ax.y, 3),
                        num(ay.x, 3),
                        num(ay.y, 3),
                        num(c.x, 2),
                        num(c.y, 2)
                    ));
                    defs.extend(stops);
                    defs.push("</radialGradient>".into());
                    id
                }
                GradientKind::Angular => {
                    let msg = format!(
                        "{}: angular gradient on a vector exported as a solid color",
                        n.id
                    );
                    b.warn(msg);
                    return Some(solid(gradient_color(g)));
                }
            };
            format!("{attr}=\"url(#{id})\"")
        }
        PaintSpec::Image(img) => {
            let url = b.asset_url(&img.url)?;
            let id = b.new_id("pen-p");
            let (w, h) = (num(size.x, 2), num(size.y, 2));
            let aspect = match img.mode {
                ImageMode::Cover => "xMidYMid slice",
                ImageMode::Contain => "xMidYMid meet",
                ImageMode::Stretch => "none",
            };
            let op = if img.opacity < 1.0 {
                format!(" opacity=\"{}\"", num(img.opacity, 3))
            } else {
                String::new()
            };
            defs.push(format!(
                "<pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" width=\"{w}\" height=\"{h}\">"
            ));
            defs.push(format!(
                "  <image href=\"{}\" width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"{aspect}\"{op}/>",
                attr_escape(&url)
            ));
            defs.push("</pattern>".into());
            format!("{attr}=\"url(#{id})\"")
        }
        PaintSpec::Shader(_) => {
            let msg = format!("{}: shader fill is not exported", n.id);
            b.warn(msg);
            return None;
        }
    })
}

/// Inner markup of the `<svg>` of a vector layer (`path` in local px).
pub(crate) fn vector_markup(
    b: &mut Builder<'_>,
    n: &SceneNode,
    path: &Path,
    size: Vec2,
    binds: &Binds<'_>,
) -> Vec<String> {
    if path.els.is_empty() {
        return Vec::new();
    }
    let d = path_data(path);
    let rule = if path.fill_rule == FillRule::EvenOdd {
        " fill-rule=\"evenodd\""
    } else {
        ""
    };
    let closed = path.els.iter().any(|e| matches!(e, PathEl::Close));
    let mut defs: Vec<String> = Vec::new();
    let mut body: Vec<String> = Vec::new();
    // Fills (a line has none).
    if n.kind != pen_doc::Kind::Line {
        let vars = binds.fills(n.fills.len());
        for (i, layer) in n.fills.iter().enumerate() {
            let var = vars.get(i).copied().flatten();
            let Some(p) = paint_attrs(b, n, "fill", &layer.paint, var, size, &mut defs) else {
                continue;
            };
            let blend = if layer.blend == pen_render::BlendMode::Normal {
                String::new()
            } else if p.starts_with("style=") {
                b.warn(format!(
                    "{}: fill blend mode on a vector is not exported",
                    n.id
                ));
                String::new()
            } else {
                format!(" style=\"mix-blend-mode: {}\"", blend_css(layer.blend))
            };
            body.push(format!("<path d=\"{d}\"{rule} {p}{blend}/>"));
        }
    }
    // Stroke (top paint).
    if let Some(st) = n.stroke.as_deref()
        && st.max_width() > 0.0
        && let Some(top) = st.paints.last()
    {
        let vars = binds.stroke_paints(st.paints.len());
        let var = vars.last().copied().flatten();
        if let Some(p) = paint_attrs(b, n, "stroke", &top.paint, var, size, &mut defs) {
            let w = st.max_width();
            let align = if closed {
                st.align
            } else {
                StrokeAlign::Center
            };
            let (sw, extra) = match align {
                StrokeAlign::Center => (w, String::new()),
                StrokeAlign::Inside => {
                    let id = b.new_id("pen-c");
                    defs.push(format!("<clipPath id=\"{id}\">"));
                    defs.push(format!("  <path d=\"{d}\"{rule}/>"));
                    defs.push("</clipPath>".into());
                    (w * 2.0, format!(" clip-path=\"url(#{id})\""))
                }
                StrokeAlign::Outside => {
                    let id = b.new_id("pen-m");
                    let m = w * 2.0 + size.x.max(size.y);
                    let (x, y) = (num(-m, 2), num(-m, 2));
                    let (mw, mh) = (num(size.x + 2.0 * m, 2), num(size.y + 2.0 * m, 2));
                    defs.push(format!(
                        "<mask id=\"{id}\" maskUnits=\"userSpaceOnUse\" x=\"{x}\" y=\"{y}\" width=\"{mw}\" height=\"{mh}\">"
                    ));
                    defs.push(format!(
                        "  <rect x=\"{x}\" y=\"{y}\" width=\"{mw}\" height=\"{mh}\" fill=\"white\"/>"
                    ));
                    defs.push(format!("  <path d=\"{d}\"{rule} fill=\"black\"/>"));
                    defs.push("</mask>".into());
                    (w * 2.0, format!(" mask=\"url(#{id})\""))
                }
            };
            let cap = match st.cap {
                LineCap::Butt => "",
                LineCap::Round => " stroke-linecap=\"round\"",
                LineCap::Square => " stroke-linecap=\"square\"",
            };
            let join = match st.join {
                LineJoin::Miter => "",
                LineJoin::Round => " stroke-linejoin=\"round\"",
                LineJoin::Bevel => " stroke-linejoin=\"bevel\"",
            };
            body.push(format!(
                "<path d=\"{d}\" fill=\"none\" {p} stroke-width=\"{}\"{cap}{join}{extra}/>",
                num(sw, 2)
            ));
        }
    }
    let mut out = Vec::new();
    if !defs.is_empty() {
        out.push("<defs>".into());
        out.extend(defs.into_iter().map(|l| format!("  {l}")));
        out.push("</defs>".into());
    }
    out.extend(body);
    out
}
