//! Text as vector outlines: the exact glyph geometry the rasterizer fills, for vector backends
//! (SVG/PDF export with "text to paths").

use crate::RasterContext;
use cosmic_text::{CacheKey, CacheKeyFlags, Command};
use pen_render::{Path, PathEl, TextAlign, TextBlock, Vec2};

/// Filled geometry of consecutive glyphs (or a decoration line) sharing one run style.
#[derive(Debug, Clone, PartialEq)]
pub struct TextOutline {
    /// Index into [`TextBlock::runs`] (its style's paint fills this outline).
    pub run: usize,
    /// Glyph outlines in the block's local space (non-zero fill rule).
    pub path: Path,
}

impl RasterContext {
    /// Shapes `block` like [`RasterContext::render`] does and returns its glyph and decoration
    /// outlines, grouped by run, in paint order. Glyphs without outlines (color bitmaps) are
    /// skipped.
    pub fn text_outlines(&mut self, block: &TextBlock) -> Vec<TextOutline> {
        let mut out: Vec<TextOutline> = Vec::new();
        if block.lines.is_empty() || block.runs.is_empty() {
            return out;
        }
        let n_runs = block.runs.len();
        let n_lines = block.lines.len();
        for (li, line) in block.lines.iter().enumerate() {
            let engine = self.res.text_engine();
            let Some(shaped) = engine.shape_line(block, line) else {
                continue;
            };
            let spaces = shaped.glyphs.iter().filter(|g| g.is_space).count();
            let extra = if block.align == TextAlign::Justify
                && li + 1 < n_lines
                && spaces > 0
                && line.width > shaped.width
            {
                (line.width - shaped.width) / spaces as f32
            } else {
                0.0
            };
            let mut shift = 0.0;
            for g in &shaped.glyphs {
                let run = (g.run as usize).min(n_runs - 1);
                let x = line.x + g.x + shift;
                let y = line.baseline + g.y;
                if g.is_space {
                    shift += extra;
                    continue;
                }
                let (key, _, _) = CacheKey::new(
                    g.font_id,
                    g.glyph_id,
                    g.size,
                    (0.0, 0.0),
                    g.weight,
                    g.flags | CacheKeyFlags::DISABLE_HINTING,
                );
                let Some(cmds) = engine.swash.get_outline_commands(&mut engine.fs, key) else {
                    continue;
                };
                if cmds.is_empty() {
                    continue;
                }
                if out.last().is_none_or(|o| o.run != run) {
                    out.push(TextOutline {
                        run,
                        path: Path::default(),
                    });
                }
                let els = &mut out.last_mut().expect("just pushed").path.els;
                append_commands(els, cmds, x, y);
            }
            for d in &shaped.decos {
                let run = (d.run as usize).min(n_runs - 1);
                let (x0, x1) = (line.x + d.x0, line.x + d.x1);
                let y0 = line.baseline + d.y - d.thickness * 0.5;
                let y1 = y0 + d.thickness;
                if !(x1 > x0 && y1 > y0) {
                    continue;
                }
                out.push(TextOutline {
                    run,
                    path: Path {
                        els: vec![
                            PathEl::MoveTo(Vec2::new(x0, y0)),
                            PathEl::LineTo(Vec2::new(x1, y0)),
                            PathEl::LineTo(Vec2::new(x1, y1)),
                            PathEl::LineTo(Vec2::new(x0, y1)),
                            PathEl::Close,
                        ],
                        fill_rule: Default::default(),
                    },
                });
            }
        }
        out
    }
}

fn append_commands(els: &mut Vec<PathEl>, cmds: &[Command], x: f32, y: f32) {
    for c in cmds {
        els.push(match *c {
            Command::MoveTo(p) => PathEl::MoveTo(Vec2::new(x + p.x, y - p.y)),
            Command::LineTo(p) => PathEl::LineTo(Vec2::new(x + p.x, y - p.y)),
            Command::QuadTo(c1, p) => {
                PathEl::QuadTo(Vec2::new(x + c1.x, y - c1.y), Vec2::new(x + p.x, y - p.y))
            }
            Command::CurveTo(c1, c2, p) => PathEl::CubicTo(
                Vec2::new(x + c1.x, y - c1.y),
                Vec2::new(x + c2.x, y - c2.y),
                Vec2::new(x + p.x, y - p.y),
            ),
            Command::Close => PathEl::Close,
        });
    }
}
