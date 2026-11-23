//! Parsers for the computed CSS values found in a snapshot.
//!
//! Computed values are already normalized by the browser (absolute lengths in
//! `px`, colors as `rgb()`/`rgba()` once the crawler converted the modern color
//! spaces), so these parsers only cover that canonical form.

/// An sRGB color with straight alpha (0..1 channels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const TRANSPARENT: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    pub const BLACK: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    pub fn is_visible(&self) -> bool {
        self.a > 0.003
    }

    pub fn with_alpha(self, a: f64) -> Color {
        Color { a, ..self }
    }

    /// `#RRGGBB`, or `#RRGGBBAA` when not opaque.
    pub fn hex(&self) -> String {
        let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let a = c(self.a);
        if a == 255 {
            format!("#{:02X}{:02X}{:02X}", c(self.r), c(self.g), c(self.b))
        } else {
            format!(
                "#{:02X}{:02X}{:02X}{:02X}",
                c(self.r),
                c(self.g),
                c(self.b),
                a
            )
        }
    }
}

/// Parses `rgb()`, `rgba()`, `transparent`, `#hex` and a few named colors.
pub fn color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "transparent" => return Some(Color::TRANSPARENT),
        "black" => return Some(Color::BLACK),
        "white" => {
            return Some(Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            });
        }
        "currentcolor" | "none" => return None,
        _ => {}
    }
    if let Some(h) = lower.strip_prefix('#') {
        let v = |i: usize, n: usize| u8::from_str_radix(h.get(i..i + n)?, 16).ok();
        let (r, g, b, a) = match h.len() {
            3 | 4 => {
                let d = |i| v(i, 1).map(|x| x * 17);
                (d(0)?, d(1)?, d(2)?, if h.len() == 4 { d(3)? } else { 255 })
            }
            6 | 8 => (
                v(0, 2)?,
                v(2, 2)?,
                v(4, 2)?,
                if h.len() == 8 { v(6, 2)? } else { 255 },
            ),
            _ => return None,
        };
        return Some(Color {
            r: r as f64 / 255.0,
            g: g as f64 / 255.0,
            b: b as f64 / 255.0,
            a: a as f64 / 255.0,
        });
    }
    let open = lower.find('(')?;
    let name = &lower[..open];
    if name != "rgb" && name != "rgba" {
        return None;
    }
    let inner = lower[open + 1..].trim_end_matches(')');
    let parts: Vec<&str> = inner
        .split([',', ' ', '/'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    let chan = |p: &str| -> Option<f64> {
        if let Some(pc) = p.strip_suffix('%') {
            Some(pc.parse::<f64>().ok()? / 100.0)
        } else {
            Some(p.parse::<f64>().ok()? / 255.0)
        }
    };
    let alpha = match parts.get(3) {
        Some(p) => match p.strip_suffix('%') {
            Some(pc) => pc.parse::<f64>().ok()? / 100.0,
            None => p.parse::<f64>().ok()?,
        },
        None => 1.0,
    };
    Some(Color {
        r: chan(parts[0])?,
        g: chan(parts[1])?,
        b: chan(parts[2])?,
        a: alpha.clamp(0.0, 1.0),
    })
}

/// `12px` / `0` -> pixels.
pub fn px(s: &str) -> Option<f64> {
    let s = s.trim();
    if s == "0" {
        return Some(0.0);
    }
    s.strip_suffix("px")?.trim().parse().ok()
}

/// A length that may be relative to some reference size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Len {
    Px(f64),
    Pct(f64),
}

impl Len {
    pub fn parse(s: &str) -> Option<Len> {
        let s = s.trim();
        if let Some(p) = s.strip_suffix('%') {
            return p.parse().ok().map(Len::Pct);
        }
        px(s).map(Len::Px)
    }

    pub fn resolve(self, reference: f64) -> f64 {
        match self {
            Len::Px(v) => v,
            Len::Pct(p) => p / 100.0 * reference,
        }
    }
}

/// Splits on `sep` outside parentheses and quotes.
pub fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, ch) in s.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '(' => depth += 1,
                ')' => depth -= 1,
                c if c == sep && depth == 0 => {
                    out.push(s[start..i].trim());
                    start = i + ch.len_utf8();
                }
                _ => {}
            },
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() || out.is_empty() {
        out.push(last);
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Whitespace-separated tokens, keeping functions such as `rgb(1, 2, 3)` whole.
pub fn tokens(s: &str) -> Vec<&str> {
    split_top(s, ' ')
}

/// The name and arguments of `name(args)`.
pub fn function(s: &str) -> Option<(&str, &str)> {
    let s = s.trim();
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    (close > open).then(|| (s[..open].trim(), &s[open + 1..close]))
}

/// The URL of `url("...")`.
pub fn url(s: &str) -> Option<String> {
    let (name, args) = function(s)?;
    if !name.eq_ignore_ascii_case("url") {
        return None;
    }
    Some(args.trim().trim_matches(['"', '\'']).to_string())
}

/// One `box-shadow` / `text-shadow` / `drop-shadow()` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Shadow {
    pub color: Color,
    pub x: f64,
    pub y: f64,
    pub blur: f64,
    pub spread: f64,
    pub inset: bool,
}

pub fn shadows(s: &str) -> Vec<Shadow> {
    if s.trim() == "none" {
        return Vec::new();
    }
    split_top(s, ',').into_iter().filter_map(shadow).collect()
}

fn shadow(s: &str) -> Option<Shadow> {
    let mut color = Color::BLACK;
    let mut lens = Vec::new();
    let mut inset = false;
    for t in tokens(s) {
        if t.eq_ignore_ascii_case("inset") {
            inset = true;
        } else if let Some(v) = px(t) {
            lens.push(v);
        } else if let Some(c) = self::color(t) {
            color = c;
        }
    }
    if lens.len() < 2 {
        return None;
    }
    Some(Shadow {
        color,
        x: lens[0],
        y: lens[1],
        blur: lens.get(2).copied().unwrap_or(0.0),
        spread: lens.get(3).copied().unwrap_or(0.0),
        inset,
    })
}

/// A `filter` / `backdrop-filter` function we can map.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Blur(f64),
    DropShadow(Shadow),
    Other(String),
}

pub fn filters(s: &str) -> Vec<Filter> {
    if s.trim() == "none" {
        return Vec::new();
    }
    tokens(s)
        .into_iter()
        .filter_map(|t| {
            let (name, args) = function(t)?;
            Some(match name {
                "blur" => Filter::Blur(px(args).unwrap_or(0.0)),
                "drop-shadow" => match shadow(args) {
                    Some(sh) => Filter::DropShadow(sh),
                    None => Filter::Other(name.to_string()),
                },
                _ => Filter::Other(name.to_string()),
            })
        })
        .collect()
}

/// A color stop; the position is unresolved (absent = distribute evenly).
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub color: Color,
    pub pos: Option<Len>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GradientShape {
    /// Angle in degrees, clockwise from "to top".
    Linear {
        angle: f64,
    },
    Radial {
        circle: bool,
        /// Explicit radii, or an extent keyword (`farthest-corner` ...).
        size: RadialSize,
        at: (Len, Len),
    },
    Conic {
        from: f64,
        at: (Len, Len),
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RadialSize {
    Extent(String),
    Explicit(Len, Option<Len>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub shape: GradientShape,
    pub repeating: bool,
    pub stops: Vec<Stop>,
}

/// Background layer from `background-image`.
#[derive(Debug, Clone, PartialEq)]
pub enum Layer {
    Url(String),
    Gradient(Gradient),
}

pub fn background_layers(s: &str) -> Vec<Layer> {
    if s.trim() == "none" {
        return Vec::new();
    }
    split_top(s, ',')
        .into_iter()
        .filter_map(|l| {
            if let Some(u) = url(l) {
                return Some(Layer::Url(u));
            }
            gradient(l).map(Layer::Gradient)
        })
        .collect()
}

fn angle(s: &str) -> Option<f64> {
    let s = s.trim();
    let num = |suffix: &str| s.strip_suffix(suffix).and_then(|v| v.parse::<f64>().ok());
    if let Some(v) = num("deg") {
        return Some(v);
    }
    if let Some(v) = num("grad") {
        return Some(v * 0.9);
    }
    if let Some(v) = num("rad") {
        return Some(v.to_degrees());
    }
    if let Some(v) = num("turn") {
        return Some(v * 360.0);
    }
    (s == "0").then_some(0.0)
}

fn position(tokens: &[&str]) -> (Len, Len) {
    let kw = |t: &str| match t {
        "left" | "top" => Some(Len::Pct(0.0)),
        "center" => Some(Len::Pct(50.0)),
        "right" | "bottom" => Some(Len::Pct(100.0)),
        _ => Len::parse(t),
    };
    match tokens {
        [] => (Len::Pct(50.0), Len::Pct(50.0)),
        [a] => match *a {
            "top" | "bottom" => (Len::Pct(50.0), kw(a).unwrap_or(Len::Pct(50.0))),
            _ => (kw(a).unwrap_or(Len::Pct(50.0)), Len::Pct(50.0)),
        },
        [a, b, ..] => {
            if matches!(*a, "top" | "bottom") || matches!(*b, "left" | "right") {
                (
                    kw(b).unwrap_or(Len::Pct(50.0)),
                    kw(a).unwrap_or(Len::Pct(50.0)),
                )
            } else {
                (
                    kw(a).unwrap_or(Len::Pct(50.0)),
                    kw(b).unwrap_or(Len::Pct(50.0)),
                )
            }
        }
    }
}

/// Parses a background position (`50% 50%`, `10px 0px`).
pub fn bg_position(s: &str) -> (Len, Len) {
    let first = split_top(s, ',').into_iter().next().unwrap_or("");
    position(&tokens(first))
}

pub fn gradient(s: &str) -> Option<Gradient> {
    let (name, args) = function(s)?;
    let name = name.to_ascii_lowercase();
    let repeating = name.starts_with("repeating-");
    let base = name.trim_start_matches("repeating-");
    let parts = split_top(args, ',');
    let mut parts = parts.as_slice();
    let first = parts.first().copied().unwrap_or("");
    let first_is_stop = tokens(first).first().and_then(|t| color(t)).is_some();
    let shape = match base {
        "linear-gradient" | "-webkit-linear-gradient" => {
            let mut a = 180.0;
            if !first_is_stop {
                let t = first.trim();
                if let Some(v) = angle(t) {
                    a = v;
                } else if let Some(dir) = t.strip_prefix("to ") {
                    let d: Vec<&str> = dir.split_whitespace().collect();
                    let has = |w: &str| d.contains(&w);
                    a = match (has("top"), has("bottom"), has("left"), has("right")) {
                        (true, _, true, _) => 315.0,
                        (true, _, _, true) => 45.0,
                        (_, true, true, _) => 225.0,
                        (_, true, _, true) => 135.0,
                        (true, ..) => 0.0,
                        (_, true, ..) => 180.0,
                        (_, _, true, _) => 270.0,
                        _ => 90.0,
                    };
                }
                parts = &parts[1..];
            }
            GradientShape::Linear { angle: a }
        }
        "radial-gradient" | "-webkit-radial-gradient" => {
            let mut circle = false;
            let mut size = RadialSize::Extent("farthest-corner".into());
            let mut at = (Len::Pct(50.0), Len::Pct(50.0));
            if !first_is_stop {
                let toks = tokens(first);
                let (before, after) = match toks.iter().position(|t| *t == "at") {
                    Some(i) => (&toks[..i], &toks[i + 1..]),
                    None => (&toks[..], &[][..]),
                };
                at = position(after);
                let mut lens = Vec::new();
                for t in before {
                    match *t {
                        "circle" => circle = true,
                        "ellipse" => {}
                        "closest-side" | "closest-corner" | "farthest-side" | "farthest-corner" => {
                            size = RadialSize::Extent(t.to_string())
                        }
                        _ => {
                            if let Some(l) = Len::parse(t) {
                                lens.push(l)
                            }
                        }
                    }
                }
                if let Some(&a) = lens.first() {
                    size = RadialSize::Explicit(a, lens.get(1).copied());
                    if lens.len() == 1 {
                        circle = true;
                    }
                }
                parts = &parts[1..];
            }
            GradientShape::Radial { circle, size, at }
        }
        "conic-gradient" => {
            let mut from = 0.0;
            let mut at = (Len::Pct(50.0), Len::Pct(50.0));
            if !first_is_stop {
                let toks = tokens(first);
                let mut i = 0;
                while i < toks.len() {
                    if toks[i] == "from" {
                        from = toks.get(i + 1).and_then(|t| angle(t)).unwrap_or(0.0);
                        i += 2;
                    } else if toks[i] == "at" {
                        at = position(&toks[i + 1..]);
                        break;
                    } else {
                        i += 1;
                    }
                }
                parts = &parts[1..];
            }
            GradientShape::Conic { from, at }
        }
        _ => return None,
    };
    let mut stops = Vec::new();
    for p in parts {
        let t = tokens(p);
        let Some(c) = t.first().and_then(|t| color(t)) else {
            // A bare position is an interpolation hint: ignored.
            continue;
        };
        let pos: Vec<Len> = t[1..]
            .iter()
            .filter_map(|t| Len::parse(t).or_else(|| angle(t).map(|a| Len::Pct(a / 3.6))))
            .collect();
        stops.push(Stop {
            color: c,
            pos: pos.first().copied(),
        });
        if let Some(&p2) = pos.get(1) {
            stops.push(Stop {
                color: c,
                pos: Some(p2),
            });
        }
    }
    if stops.is_empty() {
        return None;
    }
    Some(Gradient {
        shape,
        repeating,
        stops,
    })
}

/// Resolves stop positions to fractions of `length` px (CSS fix-up rules).
pub fn resolve_stops(stops: &[Stop], length: f64) -> Vec<(f64, Color)> {
    let n = stops.len();
    let mut pos: Vec<Option<f64>> = stops
        .iter()
        .map(|s| {
            s.pos.map(|l| match l {
                Len::Pct(p) => p / 100.0,
                Len::Px(v) => {
                    if length > 0.0 {
                        v / length
                    } else {
                        0.0
                    }
                }
            })
        })
        .collect();
    if n == 0 {
        return Vec::new();
    }
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    // Monotonic.
    let mut max = f64::MIN;
    for p in pos.iter_mut().flatten() {
        if *p < max {
            *p = max;
        }
        max = *p;
    }
    // Distribute the missing ones.
    let mut i = 0;
    while i < n {
        if pos[i].is_none() {
            let start = i - 1;
            let mut end = i;
            while pos[end].is_none() {
                end += 1;
            }
            let (a, b) = (pos[start].unwrap_or(0.0), pos[end].unwrap_or(1.0));
            for (k, p) in pos.iter_mut().enumerate().take(end).skip(start + 1) {
                *p = Some(a + (b - a) * (k - start) as f64 / (end - start) as f64);
            }
            i = end;
        }
        i += 1;
    }
    stops
        .iter()
        .zip(pos)
        .map(|(s, p)| (p.unwrap_or(0.0), s.color))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(color("rgb(255, 0, 0)").unwrap().hex(), "#FF0000");
        assert_eq!(color("rgba(0, 0, 0, 0.5)").unwrap().hex(), "#00000080");
        assert_eq!(color("rgb(0 128 255 / 50%)").unwrap().hex(), "#0080FF80");
        assert_eq!(color("#abc").unwrap().hex(), "#AABBCC");
        assert!(!color("rgba(0, 0, 0, 0)").unwrap().is_visible());
    }

    #[test]
    fn shadow_lists() {
        let s = shadows("rgba(0, 0, 0, 0.1) 0px 4px 6px -1px, rgb(0, 0, 0) 1px 2px 3px 0px inset");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].y, 4.0);
        assert_eq!(s[0].spread, -1.0);
        assert!(s[1].inset);
    }

    #[test]
    fn gradients() {
        let g =
            gradient("linear-gradient(135deg, rgb(255, 0, 0) 0%, rgb(0, 0, 255) 100%)").unwrap();
        assert_eq!(g.shape, GradientShape::Linear { angle: 135.0 });
        assert_eq!(g.stops.len(), 2);
        let g = gradient("linear-gradient(to right, rgb(1, 1, 1), rgb(2, 2, 2), rgb(3, 3, 3))")
            .unwrap();
        assert_eq!(g.shape, GradientShape::Linear { angle: 90.0 });
        let r = resolve_stops(&g.stops, 100.0);
        assert_eq!(r[1].0, 0.5);
        let g = gradient("radial-gradient(circle at 20% 30%, rgb(1, 1, 1), rgba(0, 0, 0, 0) 60%)")
            .unwrap();
        match g.shape {
            GradientShape::Radial { circle, at, .. } => {
                assert!(circle);
                assert_eq!(at, (Len::Pct(20.0), Len::Pct(30.0)));
            }
            _ => panic!(),
        }
        let layers = background_layers(
            "url(\"https://x/a.png\"), linear-gradient(rgb(0, 0, 0), rgb(255, 255, 255))",
        );
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0], Layer::Url("https://x/a.png".into()));
    }
}
