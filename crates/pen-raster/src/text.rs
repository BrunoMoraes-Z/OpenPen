//! Text shaping (cosmic-text) with a per-line cache. Drawing lives in `renderer`.

use cosmic_text::{
    Attrs, Buffer, CacheKeyFlags, Family, FontSystem, Metrics, Shaping, Style, SwashCache,
    UnderlineStyle, Weight, Wrap, fontdb,
};
use pen_fonts::system::{FamilyMatch, LazyFonts, needs_glyph};
use pen_render::{TextBlock, TextLine, TextStyle};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Families tried (in order) for the generic sans-serif fallback.
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

const MAX_SHAPED_LINES: usize = 16_384;

#[derive(Debug, Clone, Copy)]
pub struct ShapedGlyph {
    pub font_id: fontdb::ID,
    pub glyph_id: u16,
    pub size: f32,
    pub weight: fontdb::Weight,
    pub flags: CacheKeyFlags,
    /// Pen position relative to the line origin (x) and baseline (y, down).
    pub x: f32,
    pub y: f32,
    /// Index into `TextBlock::runs`.
    pub run: u32,
    pub is_space: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ShapedDeco {
    pub x0: f32,
    pub x1: f32,
    /// Center of the line relative to the baseline (positive = below).
    pub y: f32,
    pub thickness: f32,
    pub run: u32,
}

#[derive(Debug, Default)]
pub struct ShapedLine {
    pub glyphs: Vec<ShapedGlyph>,
    pub decos: Vec<ShapedDeco>,
    pub width: f32,
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
    pub fs: FontSystem,
    pub swash: SwashCache,
    buffer: Buffer,
    families: HashMap<Box<str>, FamilyRes>,
    shaped: HashMap<u64, Arc<ShapedLine>>,
    lazy: LazyFonts,
}

impl TextEngine {
    /// Bundled fonts plus system families loaded on demand (see [`pen_fonts::system`]).
    pub fn system() -> Self {
        let fs =
            FontSystem::new_with_locale_and_db("en-US".into(), pen_fonts::system::bundled_db());
        let mut e = Self::new(fs);
        e.lazy = LazyFonts::system();
        e
    }

    /// Engine over `fs` only (plus the bundled fonts if missing).
    pub fn new(mut fs: FontSystem) -> Self {
        let has = |fs: &FontSystem, name: &str| {
            fs.db()
                .faces()
                .any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
        };
        // Bundled open-source fonts (Inter): the default sans, available everywhere.
        if !has(&fs, pen_fonts::DEFAULT_SANS) {
            for f in pen_fonts::text_fonts() {
                fs.db_mut()
                    .load_font_source(fontdb::Source::Binary(Arc::new(f.data)));
            }
        }
        let has = |fs: &FontSystem, name: &str| {
            fs.db()
                .faces()
                .any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)))
        };
        if let Some(sans) = SANS_FALLBACKS.iter().find(|n| has(&fs, n)) {
            fs.db_mut().set_sans_serif_family(*sans);
        }
        let buffer = Buffer::new(&mut fs, Metrics::new(16.0, 20.0));
        Self {
            fs,
            swash: SwashCache::new(),
            buffer,
            families: HashMap::new(),
            shaped: HashMap::new(),
            lazy: LazyFonts::none(),
        }
    }

    /// Drops cached shaping results (call after loading new fonts).
    pub fn clear_caches(&mut self) {
        self.shaped.clear();
        self.families.clear();
    }

    /// True if a face with this family name is loaded or installed.
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

    /// Shapes one pre-broken line of `block`, using the cache when possible.
    pub fn shape_line(&mut self, block: &TextBlock, line: &TextLine) -> Option<Arc<ShapedLine>> {
        let text = block.text.get(line.range.clone())?;
        if block.runs.is_empty() {
            return None;
        }
        // Spans: (byte range within `text`, run index). Gaps take the previous run's style.
        let mut spans: SmallVec<[(usize, usize, usize); 4]> = SmallVec::new();
        let mut cursor = line.range.start;
        let mut last_run = 0usize;
        for (i, run) in block.runs.iter().enumerate() {
            let s = run.range.start.max(line.range.start);
            let e = run.range.end.min(line.range.end);
            if s >= e {
                if run.range.end <= line.range.start {
                    last_run = i;
                }
                continue;
            }
            if s > cursor {
                spans.push((cursor, s, last_run));
            }
            if s >= cursor {
                spans.push((s, e, i));
                cursor = e;
            }
            last_run = i;
        }
        if cursor < line.range.end {
            spans.push((cursor, line.range.end, last_run));
        }
        let key = line_key(block, text, &spans);
        if let Some(s) = self.shaped.get(&key) {
            return Some(s.clone());
        }

        let fams: SmallVec<[FamilyRes; 4]> = block
            .runs
            .iter()
            .map(|r| r.style.family.clone())
            .map(|f| self.resolve_family(&f))
            .collect();
        let mut max_size: f32 = 1.0;
        let mut attrs_list: SmallVec<[(&str, Attrs<'_>); 4]> = SmallVec::new();
        for &(s, e, ri) in &spans {
            let Some(slice) = block.text.get(s..e) else {
                continue;
            };
            let style = &block.runs[ri].style;
            max_size = max_size.max(style.size);
            attrs_list.push((slice, attrs_for(style, &fams[ri], ri)));
        }
        let default_attrs = attrs_for(&block.runs[0].style, &fams[0], 0);

        let buffer = &mut self.buffer;
        buffer.set_metrics(Metrics::new(max_size.max(1.0), max_size.max(1.0) * 1.2));
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        loop {
            buffer.set_rich_text(
                attrs_list.iter().map(|(s, a)| (*s, a.clone())),
                &default_attrs,
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut self.fs, false);
            // A visible character without a glyph: load the next system fallback and retry.
            let missing = self.lazy.can_fallback()
                && buffer.layout_runs().any(|run| {
                    run.glyphs.iter().any(|g| {
                        g.glyph_id == 0 && run.text.get(g.start..g.end).is_some_and(needs_glyph)
                    })
                });
            if !missing || !self.lazy.load_fallback(self.fs.db_mut()) {
                break;
            }
        }

        let mut out = ShapedLine::default();
        for run in buffer.layout_runs() {
            out.width = out.width.max(run.line_w);
            for g in run.glyphs {
                let cluster = run.text.get(g.start..g.end).unwrap_or("");
                out.glyphs.push(ShapedGlyph {
                    font_id: g.font_id,
                    glyph_id: g.glyph_id,
                    size: g.font_size,
                    weight: g.font_weight,
                    flags: g.cache_key_flags,
                    x: g.x + g.x_offset * g.font_size,
                    y: g.y - g.y_offset * g.font_size,
                    run: g.metadata as u32,
                    is_space: cluster.chars().all(char::is_whitespace) && !cluster.is_empty(),
                });
            }
            for d in run.decorations {
                let glyphs = &run.glyphs[d.glyph_range.clone()];
                let Some(first) = glyphs.first() else {
                    continue;
                };
                let (mut x0, mut x1) = (f32::INFINITY, f32::NEG_INFINITY);
                for g in glyphs {
                    x0 = x0.min(g.x);
                    x1 = x1.max(g.x + g.w);
                }
                let fs = d.font_size;
                let td = &d.data.text_decoration;
                let run_i = first.metadata as u32;
                if td.underline != UnderlineStyle::None {
                    let m = d.data.underline_metrics;
                    let (off, th) = if m.thickness > 0.0 {
                        (m.offset, m.thickness)
                    } else {
                        (-0.12, 0.06)
                    };
                    let th = (th * fs).max(0.5);
                    out.decos.push(ShapedDeco {
                        x0,
                        x1,
                        y: -off * fs + th * 0.5,
                        thickness: th,
                        run: run_i,
                    });
                }
                if td.strikethrough {
                    let m = d.data.strikethrough_metrics;
                    let (off, th) = if m.thickness > 0.0 {
                        (m.offset, m.thickness)
                    } else {
                        (0.3, 0.06)
                    };
                    let th = (th * fs).max(0.5);
                    out.decos.push(ShapedDeco {
                        x0,
                        x1,
                        y: -off * fs + th * 0.5,
                        thickness: th,
                        run: run_i,
                    });
                }
            }
        }

        if self.shaped.len() >= MAX_SHAPED_LINES {
            self.shaped.clear();
        }
        let out = Arc::new(out);
        self.shaped.insert(key, out.clone());
        Some(out)
    }
}

fn attrs_for<'a>(style: &TextStyle, fam: &'a FamilyRes, run: usize) -> Attrs<'a> {
    let size = style.size.max(0.1);
    let mut a = Attrs::new()
        .family(fam.family())
        .weight(Weight(style.weight.clamp(1, 1000)))
        .style(if style.italic {
            Style::Italic
        } else {
            Style::Normal
        })
        .metrics(Metrics::new(size, size * 1.2))
        .metadata(run);
    if style.letter_spacing != 0.0 {
        a = a.letter_spacing(style.letter_spacing / size);
    }
    if style.underline {
        a = a.underline(UnderlineStyle::Single);
    }
    if style.strikethrough {
        a = a.strikethrough();
    }
    a
}

fn line_key(block: &TextBlock, text: &str, spans: &[(usize, usize, usize)]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    let base = spans.first().map_or(0, |s| s.0);
    for &(s, e, ri) in spans {
        (s - base, e - base, ri).hash(&mut h);
        let st = &block.runs[ri].style;
        st.family.hash(&mut h);
        st.size.to_bits().hash(&mut h);
        st.weight.hash(&mut h);
        st.italic.hash(&mut h);
        st.letter_spacing.to_bits().hash(&mut h);
        st.underline.hash(&mut h);
        st.strikethrough.hash(&mut h);
    }
    h.finish()
}
