//! Number / color / gradient formatting.

use pen_doc::{GradientKind, GradientSpec};
use pen_render::{Color, Path, PathEl, Vec2};
use std::fmt::Write;

/// `v` rounded to `decimals`, without trailing zeros (`1.5`, `2`, `-0.25`).
pub(crate) fn num(v: f32, decimals: u32) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let p = 10f64.powi(decimals as i32);
    let r = (v as f64 * p).round() / p;
    let mut s = format!("{:.*}", decimals as usize, r);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// CSS length in px (`0` without unit).
pub(crate) fn px(v: f32) -> String {
    let s = num(v, 2);
    if s == "0" { s } else { s + "px" }
}

/// `#rrggbb` or `#rrggbbaa`.
pub(crate) fn hex(c: Color) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let (r, g, bl, a) = (b(c.r), b(c.g), b(c.b), b(c.a));
    if a == 255 {
        format!("#{r:02x}{g:02x}{bl:02x}")
    } else {
        format!("#{r:02x}{g:02x}{bl:02x}{a:02x}")
    }
}

/// `#rrggbb` (alpha dropped) and the alpha, for SVG `fill` + `fill-opacity`.
pub(crate) fn hex_opaque(c: Color) -> (String, f32) {
    (hex(Color { a: 1.0, ..c }), c.a.clamp(0.0, 1.0))
}

fn stops_css(stops: &[(f32, Color)], map: impl Fn(f32) -> f32, unit: &str) -> String {
    let mut s = String::new();
    for (i, (o, c)) in stops.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        let _ = write!(s, "{} {}{unit}", hex(*c), num(map(*o) * 100.0, 2));
    }
    s
}

/// Pen linear gradient endpoints in local px (as in the display-list builder).
pub(crate) fn linear_points(g: &GradientSpec, size: Vec2) -> (Vec2, Vec2) {
    let rot = g.rotation.to_radians();
    let d = Vec2::new(-rot.sin(), -rot.cos()) * (g.size[1] * 0.5);
    let cn = Vec2::new(g.center[0], g.center[1]);
    ((cn - d) * size, (cn + d) * size)
}

/// Pen radial gradient axes in local px (center, axis x, axis y).
pub(crate) fn radial_axes(g: &GradientSpec, size: Vec2) -> (Vec2, Vec2, Vec2) {
    let rot = g.rotation.to_radians();
    let (sin, cos) = rot.sin_cos();
    let half = |v: f32| if v == 0.0 { 1e-6 } else { v * 0.5 };
    let (rx, ry) = (half(g.size[0]), half(g.size[1]));
    let c = Vec2::new(g.center[0] * size.x, g.center[1] * size.y);
    (
        c,
        Vec2::new(cos, -sin) * rx * size,
        Vec2::new(-sin, -cos) * ry * size,
    )
}

fn norm_deg(d: f32) -> f32 {
    let d = d.rem_euclid(360.0);
    if (d - 360.0).abs() < 1e-3 { 0.0 } else { d }
}

/// CSS `*-gradient(...)` image equivalent to a Pen gradient on a `w x h` box.
pub(crate) fn css_gradient(g: &GradientSpec, size: Vec2) -> String {
    let last = g.stops.last().map_or(Color::TRANSPARENT, |s| s.1);
    match g.kind {
        GradientKind::Linear => {
            let (s, e) = linear_points(g, size);
            let v = e - s;
            if v.length() < 1e-4 {
                let c = hex(last);
                return format!("linear-gradient({c}, {c})");
            }
            // CSS angle: clockwise from "to top"; the gradient line goes through the box
            // center and its length makes the corners hit 0% / 100%.
            let theta = v.x.atan2(-v.y);
            let u = Vec2::new(theta.sin(), -theta.cos());
            let len = (size.x * theta.sin()).abs() + (size.y * theta.cos()).abs();
            let len = if len < 1e-4 { 1.0 } else { len };
            let c = size * 0.5;
            let t = |p: Vec2| (p - c).dot(u) / len + 0.5;
            let (ts, te) = (t(s), t(e));
            format!(
                "linear-gradient({}deg, {})",
                num(norm_deg(theta.to_degrees()), 2),
                stops_css(&g.stops, |o| ts + o * (te - ts), "%")
            )
        }
        GradientKind::Radial => {
            let (c, ax, ay) = radial_axes(g, size);
            // CSS ellipses are axis-aligned: use the axis lengths, swapped when the
            // gradient is turned by ~90 degrees.
            let (rx, ry) = if ax.x.abs() >= ax.y.abs() {
                (ax.length(), ay.length())
            } else {
                (ay.length(), ax.length())
            };
            format!(
                "radial-gradient({} {} at {} {}, {})",
                px(rx.max(0.01)),
                px(ry.max(0.01)),
                px(c.x),
                px(c.y),
                stops_css(&g.stops, |o| o, "%")
            )
        }
        GradientKind::Angular => {
            let c = Vec2::new(g.center[0] * size.x, g.center[1] * size.y);
            format!(
                "conic-gradient(from {}deg at {} {}, {})",
                num(norm_deg(-g.rotation), 2),
                px(c.x),
                px(c.y),
                stops_css(&g.stops, |o| o, "%")
            )
        }
    }
}

/// A representative solid color of a gradient (for places that only take colors).
pub(crate) fn gradient_color(g: &GradientSpec) -> Color {
    g.stops.first().map_or(Color::TRANSPARENT, |s| s.1)
}

/// SVG path data (absolute commands, 2 decimals).
pub(crate) fn path_data(p: &Path) -> String {
    let mut s = String::new();
    let pt = |s: &mut String, v: Vec2| {
        let _ = write!(s, "{} {}", num(v.x, 2), num(v.y, 2));
    };
    for e in &p.els {
        match *e {
            PathEl::MoveTo(a) => {
                s.push('M');
                pt(&mut s, a);
            }
            PathEl::LineTo(a) => {
                s.push('L');
                pt(&mut s, a);
            }
            PathEl::QuadTo(a, b) => {
                s.push('Q');
                pt(&mut s, a);
                s.push(' ');
                pt(&mut s, b);
            }
            PathEl::CubicTo(a, b, c) => {
                s.push('C');
                pt(&mut s, a);
                s.push(' ');
                pt(&mut s, b);
                s.push(' ');
                pt(&mut s, c);
            }
            PathEl::Close => s.push('Z'),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn grad(kind: GradientKind, rotation: f32) -> GradientSpec {
        GradientSpec {
            kind,
            center: [0.5, 0.5],
            size: [1.0, 1.0],
            rotation,
            stops: smallvec![
                (0.0, Color::rgba(1.0, 0.0, 0.0, 1.0)),
                (1.0, Color::rgba(0.0, 0.0, 1.0, 0.5))
            ],
        }
    }

    #[test]
    fn numbers_and_colors() {
        assert_eq!(num(1.0, 2), "1");
        assert_eq!(num(1.256, 2), "1.26");
        assert_eq!(num(-0.001, 2), "0");
        assert_eq!(px(0.0), "0");
        assert_eq!(px(12.5), "12.5px");
        assert_eq!(hex(Color::rgba(1.0, 0.5, 0.0, 1.0)), "#ff8000");
        assert_eq!(hex(Color::rgba(0.0, 0.0, 0.0, 0.5)), "#00000080");
    }

    #[test]
    fn linear_gradient_angles() {
        let s = Vec2::new(100.0, 50.0);
        // Rotation 0: from the bottom (first stop) to the top.
        assert_eq!(
            css_gradient(&grad(GradientKind::Linear, 0.0), s),
            "linear-gradient(0deg, #ff0000 0%, #0000ff80 100%)"
        );
        // 90 = counter-clockwise: the gradient points left.
        assert_eq!(
            css_gradient(&grad(GradientKind::Linear, 90.0), s),
            "linear-gradient(270deg, #ff0000 0%, #0000ff80 100%)"
        );
        // Shorter gradient line: stops move inwards.
        let mut g = grad(GradientKind::Linear, 180.0);
        g.size = [1.0, 0.5];
        assert_eq!(
            css_gradient(&g, s),
            "linear-gradient(180deg, #ff0000 25%, #0000ff80 75%)"
        );
    }

    #[test]
    fn radial_and_conic() {
        let s = Vec2::new(100.0, 50.0);
        assert_eq!(
            css_gradient(&grad(GradientKind::Radial, 0.0), s),
            "radial-gradient(50px 25px at 50px 25px, #ff0000 0%, #0000ff80 100%)"
        );
        assert_eq!(
            css_gradient(&grad(GradientKind::Angular, 90.0), s),
            "conic-gradient(from 270deg at 50px 25px, #ff0000 0%, #0000ff80 100%)"
        );
    }

    #[test]
    fn path_data_format() {
        let p = Path {
            els: vec![
                PathEl::MoveTo(Vec2::new(0.0, 0.0)),
                PathEl::LineTo(Vec2::new(10.5, 0.0)),
                PathEl::CubicTo(
                    Vec2::new(1.0, 2.0),
                    Vec2::new(3.0, 4.0),
                    Vec2::new(5.0, 6.333),
                ),
                PathEl::Close,
            ],
            fill_rule: Default::default(),
        };
        assert_eq!(path_data(&p), "M0 0L10.5 0C1 2 3 4 5 6.33Z");
    }
}
