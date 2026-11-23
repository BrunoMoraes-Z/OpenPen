//! Icon nodes rendered as vector glyph outlines of the bundled icon fonts
//! (lucide, Material Symbols Outlined/Rounded/Sharp, Phosphor; see `pen-fonts`).
//!
//! The legacy editor shapes the icon's codepoint in its font (wght axis =
//! `iconFontWeight`, default 200) with line height = font size and centers the
//! glyph's advance x em box in the node. We extract the same outline with
//! ttf-parser and return it as a filled [`Path`], so every backend draws it the
//! same way (no icon font registration needed in GPUI).

use crate::assets::IconGlyph;
use pen_fonts::IconFont;
use pen_render::{FillRule, Path, PathEl, Vec2};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// Why an icon could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconError {
    /// `iconFontFamily` is not a bundled icon set.
    UnknownFamily,
    /// The set has no icon with this name.
    UnknownName,
    /// The codepoint has no outline in the font.
    MissingGlyph,
}

/// An icon resolved to its font and codepoint.
#[derive(Debug, Clone, Copy)]
pub struct IconRef {
    pub set: &'static IconFont,
    pub codepoint: u32,
}

struct Set {
    font: &'static IconFont,
    names: HashMap<&'static str, u32>,
}

fn sets() -> &'static [Set] {
    static SETS: OnceLock<Vec<Set>> = OnceLock::new();
    SETS.get_or_init(|| {
        pen_fonts::icon_fonts()
            .iter()
            .map(|font| Set {
                font,
                names: font
                    .codepoints
                    .lines()
                    .filter_map(|l| {
                        let (n, hex) = l.split_once(' ')?;
                        Some((n, u32::from_str_radix(hex.trim(), 16).ok()?))
                    })
                    .collect(),
            })
            .collect()
    })
}

fn find_set(family: &str) -> Option<&'static Set> {
    let f = family.trim().to_ascii_lowercase();
    // feather is the ancestor of lucide and shares (almost) all its names.
    let f = if f == "feather" { "lucide".into() } else { f };
    sets()
        .iter()
        .find(|s| s.font.families.contains(&f.as_str()))
}

/// True if `family` is a bundled icon set (case-insensitive).
pub fn is_known_family(family: &str) -> bool {
    find_set(family).is_some()
}

/// Resolves `name` in icon set `family` to a codepoint.
pub fn resolve_icon(family: &str, name: &str) -> Result<IconRef, IconError> {
    let set = find_set(family).ok_or(IconError::UnknownFamily)?;
    let name = name.trim();
    let cp = set
        .names
        .get(name)
        .or_else(|| set.names.get(name.to_ascii_lowercase().as_str()))
        .copied()
        .ok_or(IconError::UnknownName)?;
    Ok(IconRef {
        set: set.font,
        codepoint: cp,
    })
}

struct Outline {
    els: Vec<PathEl>,
    /// Font units -> path units: (x, y_up) -> (x + dx, by - y_up).
    dx: f32,
    by: f32,
}

impl Outline {
    fn p(&self, x: f32, y: f32) -> Vec2 {
        Vec2::new(x + self.dx, self.by - y)
    }
}

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.els.push(PathEl::MoveTo(p));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.els.push(PathEl::LineTo(p));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (a, b) = (self.p(x1, y1), self.p(x, y));
        self.els.push(PathEl::QuadTo(a, b));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b, c) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        self.els.push(PathEl::CubicTo(a, b, c));
    }
    fn close(&mut self) {
        self.els.push(PathEl::Close);
    }
}

/// Outline of an icon at `weight` (only variable sets use it; clamped to the
/// font's `wght` range).
pub fn icon_glyph(icon: IconRef, weight: f32) -> Result<IconGlyph, IconError> {
    let mut face =
        ttf_parser::Face::parse(icon.set.font.data, 0).map_err(|_| IconError::MissingGlyph)?;
    if icon.set.variable_weight {
        let tag = ttf_parser::Tag::from_bytes(b"wght");
        if let Some(axis) = face.variation_axes().into_iter().find(|a| a.tag == tag) {
            let w = if weight.is_finite() {
                weight
            } else {
                axis.def_value
            };
            face.set_variation(tag, w.clamp(axis.min_value, axis.max_value));
        }
    }
    let gid = char::from_u32(icon.codepoint)
        .and_then(|c| face.glyph_index(c))
        .ok_or(IconError::MissingGlyph)?;
    // Line box of the legacy paragraph: advance x em (line height = 1em,
    // half leading); the baseline sits in the middle of the ascent/descent.
    let em = face.units_per_em() as f32;
    let adv = face
        .glyph_hor_advance(gid)
        .map(f32::from)
        .filter(|a| *a > 0.0)
        .unwrap_or(em);
    let (asc, desc) = (face.ascender() as f32, face.descender() as f32);
    let baseline = (em - (asc - desc)) * 0.5 + asc;
    // Fit the shorter side: center a min(adv, em) square in the line box.
    let side = adv.min(em);
    let mut out = Outline {
        els: Vec::new(),
        dx: (side - adv) * 0.5,
        by: baseline + (side - em) * 0.5,
    };
    face.outline_glyph(gid, &mut out)
        .ok_or(IconError::MissingGlyph)?;
    Ok(IconGlyph {
        path: Arc::new(Path {
            els: out.els,
            fill_rule: FillRule::NonZero,
        }),
        view_box: side,
        stroked: false,
        stroke_width: 0.0,
    })
}

/// Bundled icon for (`family`, `name`, `weight`), or `None` (with a one-time
/// warning on stderr per unknown family/name) so the caller draws a placeholder.
pub fn bundled_icon(family: &str, name: &str, weight: f32) -> Option<IconGlyph> {
    match resolve_icon(family, name).and_then(|r| icon_glyph(r, weight)) {
        Ok(g) => Some(g),
        Err(e) => {
            warn_once(family, name, &e);
            None
        }
    }
}

fn warn_once(family: &str, name: &str, e: &IconError) {
    static SEEN: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let key = match e {
        IconError::UnknownFamily => (family.to_owned(), String::new()),
        _ => (family.to_owned(), name.to_owned()),
    };
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if seen.insert(key) {
        match e {
            IconError::UnknownFamily => {
                eprintln!(
                    "warning: icon font family {family:?} is not bundled; drawing placeholders"
                )
            }
            IconError::UnknownName => {
                eprintln!("warning: icon {name:?} not found in {family:?}; drawing a placeholder")
            }
            IconError::MissingGlyph => {
                eprintln!(
                    "warning: icon {name:?} of {family:?} has no outline; drawing a placeholder"
                )
            }
        }
    }
}
