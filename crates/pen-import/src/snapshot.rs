//! The DOM snapshot produced by `crawl.js` (lenient: every field is optional).

use std::collections::HashMap;

use serde::Deserialize;

/// `[x, y, width, height]` in page CSS pixels (untransformed layout box).
pub type Rect4 = [f64; 4];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub url: String,
    pub title: String,
    pub viewport: [f64; 2],
    #[serde(rename = "docHeight")]
    pub doc_height: f64,
    pub region: Rect4,
    pub background: String,
    #[serde(rename = "backgroundImage")]
    pub background_image: Option<String>,
    pub root: Option<DomNode>,
    /// Font stacks used by text, indexed by `DomNode::f`.
    pub fonts: Vec<Option<FontUse>>,
    #[serde(rename = "fontFaces")]
    pub font_faces: Vec<FontFace>,
    pub shots: usize,
    pub nodes: usize,
    pub truncated: bool,
    pub warnings: Vec<String>,
    pub ms: f64,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct FontUse {
    pub family: String,
    pub weight: String,
    pub style: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct FontFace {
    pub family: String,
    pub weight: String,
    pub style: String,
    pub url: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct TextLine {
    pub t: String,
    pub r: Rect4,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Transform {
    /// `[a, b, c, d, e, f]` (CSS matrix, applied around the origin).
    pub m: [f64; 6],
    /// Transform origin relative to the border box.
    pub o: [f64; 2],
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Marker {
    pub t: String,
    pub w: f64,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct DomNode {
    /// `el`, `text`, `img`, `svg`, `shot`, `input`.
    pub k: String,
    /// Lower-case tag name.
    pub t: String,
    pub id: Option<String>,
    pub cls: Option<String>,
    pub role: Option<String>,
    pub label: Option<String>,
    pub href: Option<String>,
    pub comp: Option<String>,
    pub r: Rect4,
    /// Computed styles (only non-default values).
    pub s: HashMap<String, String>,
    pub c: Vec<DomNode>,
    pub hidden: bool,
    pub clip: bool,
    pub tf: Option<Transform>,
    pub deco: Vec<String>,
    /// Font use index (see [`Snapshot::fonts`]).
    pub f: Option<usize>,
    // text
    pub lines: Vec<TextLine>,
    pub approx: bool,
    // img / video poster
    pub src: Option<String>,
    pub nat: Option<[f64; 2]>,
    // svg
    pub svg: Option<String>,
    /// SVG used as `mask-image` (painted with the background color).
    pub mask: Option<String>,
    // shot
    pub shot: Option<usize>,
    // input
    pub value: Option<String>,
    pub placeholder: bool,
    /// `<input type=submit|button|reset>`: the value is a centered label.
    pub button: bool,
    #[serde(rename = "phColor")]
    pub ph_color: Option<String>,
    // backgrounds: natural sizes of url() layers
    #[serde(rename = "bgNat")]
    pub bg_nat: HashMap<String, [f64; 2]>,
    pub marker: Option<Marker>,
}

impl DomNode {
    pub fn style(&self, key: &str) -> &str {
        self.s.get(key).map(String::as_str).unwrap_or("")
    }

    /// A pixel length style (`"12px"`), 0 when absent or not in px.
    pub fn px(&self, key: &str) -> f64 {
        crate::css::px(self.style(key)).unwrap_or(0.0)
    }

    pub fn walk(&self, f: &mut dyn FnMut(&DomNode)) {
        f(self);
        for c in &self.c {
            c.walk(f);
        }
    }
}
