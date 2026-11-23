//! Export vetorial da `DisplayList`: SVG (string) e PDF (bytes).
//!
//! The input is the same backend-agnostic [`DisplayList`] the rasterizers draw (document
//! space). Entry points:
//! - [`svg::export`]: SVG 1.1 document (deterministic ids, embedded images as data URIs).
//! - [`pdf::export`] / [`pdf::export_pages`]: vector PDF built from that SVG with `svg2pdf`
//!   (fonts embedded and subset; filters such as shadows/blur rasterized per group).
//!
//! What gets exported is chosen by [`ExportOptions`]: all chunks or only some top-level roots,
//! cropped to a region (default: union of the selected chunks' bounds), at a scale, over an
//! optional background. To export a nested node, build its list with
//! `pen_layout::Pipeline::node_list` and pass `region = Pipeline::node_rect` (like
//! `openpen-cli render --node`).

mod fmt;
pub mod pdf;
pub mod svg;

pub use pen_render::{Color, DisplayList, NodeKey, Rect};

/// How text items are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextMode {
    /// Real text (`<text>`/`<tspan>` per pre-broken line; selectable, fonts embedded in PDF).
    #[default]
    Text,
    /// Glyph outlines as paths, shaped with the same fonts as `pen-raster` (exact fidelity,
    /// no font dependency for the consumer).
    Outlines,
}

/// What to export and how.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportOptions {
    /// Top-level nodes (chunk roots) to export; `None` = every chunk.
    pub roots: Option<Vec<NodeKey>>,
    /// Crop rectangle in document space; `None` = union of the selected chunks' bounds.
    pub region: Option<Rect>,
    /// Output units (SVG px / PDF pt) per document unit.
    pub scale: f32,
    /// Opaque or translucent background painted under the content; `None` = transparent.
    pub background: Option<Color>,
    pub text: TextMode,
    /// Device pixels per output unit for the parts that are rasterized (angular gradients,
    /// backdrop blur; in PDF also SVG filters: shadows and layer blur).
    pub raster_scale: f32,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            roots: None,
            region: None,
            scale: 1.0,
            background: None,
            text: TextMode::Text,
            raster_scale: 2.0,
        }
    }
}

impl ExportOptions {
    /// Options exporting only the chunk of `root`, cropped to its bounds.
    pub fn root(root: NodeKey) -> Self {
        Self {
            roots: Some(vec![root]),
            ..Self::default()
        }
    }

    /// Effective scale (invalid values become 1).
    pub fn effective_scale(&self) -> f32 {
        if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }

    fn selects(&self, root: NodeKey) -> bool {
        self.roots.as_ref().is_none_or(|r| r.contains(&root))
    }

    /// The document-space rectangle that will be exported (at least 1x1, like
    /// `openpen-cli render`).
    pub fn resolve_region(&self, list: &DisplayList) -> Rect {
        let r = self.region.or_else(|| {
            list.chunks
                .iter()
                .filter(|c| self.selects(c.root))
                .map(|c| c.bounds)
                .reduce(|a, b| a.union(&b))
        });
        let r = r
            .filter(|r| r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite())
            .unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
        Rect::new(r.x, r.y, r.w.max(1.0), r.h.max(1.0))
    }
}

/// Export failures (SVG export itself cannot fail).
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("generated SVG could not be parsed: {0}")]
    Svg(String),
    #[error("PDF conversion failed: {0}")]
    Pdf(String),
    #[error("no pages to export")]
    NoPages,
}
