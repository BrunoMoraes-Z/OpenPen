//! Shared text engine (cosmic-text): measures text for layout AND produces the
//! broken [`TextLine`]s stored in each `TextBlock`, so every backend wraps
//! identically. Font family resolution mirrors pen-raster's (same fallback
//! list, case-insensitive names, unknown families -> sans-serif) so that the
//! widths measured here match what the rasterizer shapes.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap};
use pen_doc::TextSpec;
use pen_fonts::system::{FamilyMatch, LazyFonts, needs_glyph};
use pen_render::{Paint, TextAlign, TextBlock, TextLine, TextRun, TextStyle, Vec2};
use smallvec::smallvec;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Families tried (in order) for the generic sans-serif fallback (same list as pen-raster).
const SANS_FALLBACKS: &[&str] = &[
    "Inter",
    "Segoe UI",
    "Arial",
    "Helvetica Neue",
    "Helvetica",
    "Roboto",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Open Sans",
];

/// Line height factor used when `lineHeight` is 0 (automatic).
pub const DEFAULT_LINE_HEIGHT: f32 = 1.2;

const MAX_CACHE: usize = 32_768;

/// Result of laying out one text: lines with `x = 0` (alignment is applied
/// when building the [`TextBlock`]) and baselines relative to the text top.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    pub lines: Vec<TextLine>,
    /// Widest line (trailing whitespace excluded).
    pub width: f32,
    /// Total height (lines x line height).
    pub height: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone)]
enum FamilyRes {
    Named(Arc<str>),
    SansSerif,
    Serif,
    Monospace,
}

impl FamilyRes {
    fn family(&self) -> Family<'_> {
        match self {
            FamilyRes::Named(n) => Family::Name(n),
            FamilyRes::SansSerif => Family::SansSerif,
            FamilyRes::Serif => Family::Serif,
            FamilyRes::Monospace => Family::Monospace,
        }
    }
}

pub struct TextEngine {
    fs: FontSystem,
    buffer: Buffer,
    families: HashMap<Box<str>, FamilyRes>,
    cache: HashMap<u64, Arc<TextLayout>>,
    para_starts: Vec<usize>,
    /// Families missing from `fs` are loaded on demand from the system catalog.
    lazy: LazyFonts,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    /// Engine with the bundled fonts (Inter). System families are loaded lazily, the first time
    /// a text uses one (see [`pen_fonts::system`]): creating it costs microseconds and no
    /// system font scan happens for documents that only use bundled fonts.
    pub fn new() -> Self {
        let fs =
            FontSystem::new_with_locale_and_db("en-US".into(), pen_fonts::system::bundled_db());
        let mut e = Self::with_font_system(fs);
        e.lazy = LazyFonts::system();
        e
    }

    /// Engine over `fs` only (no system fonts are added later); the bundled fonts
    /// ([`pen_fonts::text_fonts`], Inter) are added to it if missing.
    pub fn with_font_system(mut fs: FontSystem) -> Self {
        let has = |fs: &FontSystem, name: &str| {
            fs.db()
                .faces()
                .any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
        };
        if !has(&fs, pen_fonts::DEFAULT_SANS) {
            for f in pen_fonts::text_fonts() {
                fs.db_mut()
                    .load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(f.data)));
            }
        }
        if let Some(sans) = SANS_FALLBACKS.iter().find(|n| has(&fs, n)) {
            fs.db_mut().set_sans_serif_family(*sans);
        }
        let buffer = Buffer::new(&mut fs, Metrics::new(16.0, 20.0));
        Self {
            fs,
            buffer,
            families: HashMap::new(),
            cache: HashMap::new(),
            para_starts: Vec::new(),
            lazy: LazyFonts::none(),
        }
    }

    pub fn font_system(&mut self) -> &mut FontSystem {
        &mut self.fs
    }

    /// Registers a font from memory and drops cached layouts.
    pub fn load_font_data(&mut self, data: Vec<u8>) {
        self.fs.db_mut().load_font_data(data);
        self.clear_caches();
    }

    pub fn clear_caches(&mut self) {
        self.cache.clear();
        self.families.clear();
    }

    /// True if the family is loaded (or, for engines with system fonts, installed).
    pub fn has_family(&self, name: &str) -> bool {
        self.lazy.has_family(self.fs.db(), name)
    }

    fn resolve_family(&mut self, name: &str) -> FamilyRes {
        if let Some(f) = self.families.get(name) {
            return f.clone();
        }
        let res = match self.lazy.resolve(self.fs.db_mut(), name) {
            FamilyMatch::Named(n) => FamilyRes::Named(n.into()),
            FamilyMatch::SansSerif => FamilyRes::SansSerif,
            FamilyMatch::Serif => FamilyRes::Serif,
            FamilyMatch::Monospace => FamilyRes::Monospace,
        };
        self.families.insert(name.into(), res.clone());
        res
    }

    /// Line height in px for a text style.
    pub fn line_height(t: &TextSpec) -> f32 {
        let k = if t.line_height > 0.0 {
            t.line_height
        } else {
            DEFAULT_LINE_HEIGHT
        };
        (t.size * k).max(0.1)
    }

    /// Lays `t` out, wrapping at `wrap` px (`None` = no wrapping). Cached.
    pub fn layout(&mut self, t: &TextSpec, wrap: Option<f32>) -> Arc<TextLayout> {
        let wrap = wrap.filter(|w| w.is_finite()).map(|w| w.max(0.0));
        let key = cache_key(t, wrap);
        if let Some(l) = self.cache.get(&key) {
            return l.clone();
        }
        let out = Arc::new(self.compute(t, wrap));
        if self.cache.len() >= MAX_CACHE {
            self.cache.clear();
        }
        self.cache.insert(key, out.clone());
        out
    }

    fn compute(&mut self, t: &TextSpec, wrap: Option<f32>) -> TextLayout {
        loop {
            let (l, missing) = self.compute_once(t, wrap);
            if !missing || !self.lazy.load_fallback(self.fs.db_mut()) {
                return l;
            }
        }
    }

    /// Lays out once; also reports whether a visible character got no glyph.
    fn compute_once(&mut self, t: &TextSpec, wrap: Option<f32>) -> (TextLayout, bool) {
        let lh = Self::line_height(t);
        let text: &str = &t.content;
        if text.is_empty() {
            let l = TextLayout {
                lines: vec![TextLine {
                    range: 0..0,
                    x: 0.0,
                    baseline: lh * 0.5 + t.size * 0.35,
                    width: 0.0,
                }],
                width: 0.0,
                height: lh,
                line_height: lh,
            };
            return (l, false);
        }
        let fam = self.resolve_family(&t.family);
        let size = t.size.max(0.1);
        let mut attrs = Attrs::new()
            .family(fam.family())
            .weight(Weight(t.weight.clamp(1, 1000)))
            .style(if t.italic {
                Style::Italic
            } else {
                Style::Normal
            })
            .metrics(Metrics::new(size, lh));
        if t.letter_spacing != 0.0 {
            attrs = attrs.letter_spacing(t.letter_spacing / size);
        }
        self.para_starts.clear();
        self.para_starts.push(0);
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                self.para_starts.push(i + 1);
            }
        }
        let buffer = &mut self.buffer;
        buffer.set_metrics(Metrics::new(size, lh));
        match wrap {
            // Small tolerance so that a text laid out at its own measured
            // width never wraps because of float rounding.
            Some(w) => {
                buffer.set_wrap(Wrap::Word);
                buffer.set_size(Some(w + 0.5), None);
            }
            None => {
                buffer.set_wrap(Wrap::None);
                buffer.set_size(None, None);
            }
        }
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fs, false);

        let mut lines = Vec::new();
        let mut width: f32 = 0.0;
        let mut bottom: f32 = 0.0;
        let mut missing = false;
        let check = self.lazy.can_fallback();
        for run in buffer.layout_runs() {
            let pstart = self.para_starts.get(run.line_i).copied().unwrap_or(0);
            if check && !missing {
                missing = run.glyphs.iter().any(|g| {
                    g.glyph_id == 0 && run.text.get(g.start..g.end).is_some_and(needs_glyph)
                });
            }
            bottom = bottom.max(run.line_top + run.line_height);
            if run.glyphs.is_empty() {
                lines.push(TextLine {
                    range: pstart..pstart,
                    x: 0.0,
                    baseline: run.line_y,
                    width: 0.0,
                });
                continue;
            }
            let mut s = usize::MAX;
            let mut e = 0usize;
            for g in run.glyphs {
                s = s.min(g.start);
                e = e.max(g.end);
            }
            let ptext = run.text;
            let mut trimmed = e.min(ptext.len());
            while trimmed > s {
                let Some(c) = ptext[..trimmed].chars().next_back() else {
                    break;
                };
                if !c.is_whitespace() {
                    break;
                }
                trimmed -= c.len_utf8();
            }
            let (mut x0, mut x1) = (f32::INFINITY, f32::NEG_INFINITY);
            for g in run.glyphs {
                if g.start < trimmed {
                    x0 = x0.min(g.x);
                    x1 = x1.max(g.x + g.w);
                }
            }
            let w = if x1 > x0 { x1 - x0.min(0.0) } else { 0.0 };
            width = width.max(w);
            lines.push(TextLine {
                range: pstart + s.min(trimmed)..pstart + trimmed,
                x: 0.0,
                baseline: run.line_y,
                width: w,
            });
        }
        if lines.is_empty() {
            lines.push(TextLine {
                range: 0..0,
                x: 0.0,
                baseline: lh * 0.5 + size * 0.35,
                width: 0.0,
            });
            bottom = lh;
        }
        let l = TextLayout {
            height: bottom.max(lh),
            lines,
            width,
            line_height: lh,
        };
        (l, missing)
    }
}

fn cache_key(t: &TextSpec, wrap: Option<f32>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.content.hash(&mut h);
    t.family.hash(&mut h);
    t.size.to_bits().hash(&mut h);
    t.weight.hash(&mut h);
    t.italic.hash(&mut h);
    t.letter_spacing.to_bits().hash(&mut h);
    t.line_height.to_bits().hash(&mut h);
    wrap.map(f32::to_bits).hash(&mut h);
    h.finish()
}

/// Builds the backend text block of a laid-out text in a box of `size`.
pub fn text_block(
    t: &TextSpec,
    layout: &TextLayout,
    paint: Paint,
    size: Vec2,
    clip: bool,
) -> TextBlock {
    let dy = match t.valign {
        pen_doc::VAlign::Top => 0.0,
        pen_doc::VAlign::Middle => (size.y - layout.height) * 0.5,
        pen_doc::VAlign::Bottom => size.y - layout.height,
    };
    let lines = layout
        .lines
        .iter()
        .map(|l| {
            let x = match t.align {
                TextAlign::Left | TextAlign::Justify => 0.0,
                TextAlign::Center => (size.x - l.width) * 0.5,
                TextAlign::Right => size.x - l.width,
            };
            TextLine {
                range: l.range.clone(),
                x,
                baseline: l.baseline + dy,
                width: l.width,
            }
        })
        .collect();
    TextBlock {
        text: t.content.clone(),
        runs: smallvec![TextRun {
            range: 0..t.content.len(),
            style: TextStyle {
                family: t.family.clone(),
                size: t.size,
                weight: t.weight,
                italic: t.italic,
                letter_spacing: t.letter_spacing,
                underline: t.underline,
                strikethrough: t.strikethrough,
                paint,
            },
        }],
        lines,
        align: t.align,
        size,
        clip,
    }
}
