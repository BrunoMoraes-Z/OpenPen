//! Allocation-free, deterministic formatting helpers for the SVG writer.

use pen_render::{Affine2, Color};
use std::fmt::Write;

/// Writes `v` with at most `prec` decimals, trailing zeros trimmed (`1.50` -> `1.5`,
/// `-0.000` -> `0`). Non-finite values become `0`.
pub fn num_p(out: &mut String, v: f32, prec: usize) {
    let v = if v.is_finite() { v } else { 0.0 };
    let start = out.len();
    let _ = write!(out, "{v:.prec$}");
    if out[start..].contains('.') {
        while out.ends_with('0') {
            out.pop();
        }
        if out.ends_with('.') {
            out.pop();
        }
    }
    if &out[start..] == "-0" {
        out.truncate(start);
        out.push('0');
    }
}

/// Coordinates: 3 decimals (1/1000 of a document unit).
pub fn num(out: &mut String, v: f32) {
    num_p(out, v, 3);
}

/// ` name="v"` with a coordinate value.
pub fn attr_num(out: &mut String, name: &str, v: f32) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    num(out, v);
    out.push('"');
}

/// ` name="value"` (value written verbatim; callers escape).
pub fn attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(value);
    out.push('"');
}

/// ` name="url(#<prefix><id>)"`.
pub fn attr_url(out: &mut String, name: &str, id: Id) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"url(#");
    id.write(out);
    out.push_str(")\"");
}

/// A generated element id: `<prefix><n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(pub char, pub u32);

impl Id {
    pub fn write(self, out: &mut String) {
        out.push(self.0);
        let _ = write!(out, "{}", self.1);
    }
    /// ` id="..."`.
    pub fn attr(self, out: &mut String) {
        out.push_str(" id=\"");
        self.write(out);
        out.push('"');
    }
}

fn channel(v: f32) -> u8 {
    if v.is_finite() {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        0
    }
}

/// `#rrggbb` (alpha ignored).
pub fn hex(out: &mut String, c: Color) {
    let _ = write!(
        out,
        "#{:02x}{:02x}{:02x}",
        channel(c.r),
        channel(c.g),
        channel(c.b)
    );
}

/// Clamped alpha in 0..=1 (non-finite = 0).
pub fn alpha(c: Color) -> f32 {
    if c.a.is_finite() {
        c.a.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// ` transform="..."` for `t` (nothing for the identity).
pub fn transform_attr(out: &mut String, t: &Affine2) {
    let m = t.matrix2;
    let tr = t.translation;
    let is_lin_identity =
        m.x_axis.x == 1.0 && m.x_axis.y == 0.0 && m.y_axis.x == 0.0 && m.y_axis.y == 1.0;
    if is_lin_identity {
        if tr.x == 0.0 && tr.y == 0.0 {
            return;
        }
        out.push_str(" transform=\"translate(");
        num(out, tr.x);
        out.push(' ');
        num(out, tr.y);
        out.push_str(")\"");
        return;
    }
    out.push_str(" transform=\"matrix(");
    for (i, v) in [m.x_axis.x, m.x_axis.y, m.y_axis.x, m.y_axis.y]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            out.push(' ');
        }
        num_p(out, v, 6);
    }
    out.push(' ');
    num(out, tr.x);
    out.push(' ');
    num(out, tr.y);
    out.push_str(")\"");
}

/// Escapes text content / attribute values; drops characters XML 1.0 forbids.
pub fn escape(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(ch),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_trimmed_and_stable() {
        let mut s = String::new();
        for v in [1.5f32, 2.0, -0.0001, 0.1234, -3.25, f32::NAN, 1e5] {
            num(&mut s, v);
            s.push(',');
        }
        assert_eq!(s, "1.5,2,0,0.123,-3.25,0,100000,");
    }

    #[test]
    fn transforms() {
        let mut s = String::new();
        transform_attr(&mut s, &Affine2::IDENTITY);
        assert_eq!(s, "");
        transform_attr(
            &mut s,
            &Affine2::from_translation(pen_render::Vec2::new(3.0, -4.5)),
        );
        assert_eq!(s, " transform=\"translate(3 -4.5)\"");
    }

    #[test]
    fn escaping() {
        let mut s = String::new();
        escape(&mut s, "a<b & \"c\"\u{1}");
        assert_eq!(s, "a&lt;b &amp; &quot;c&quot;");
    }
}
