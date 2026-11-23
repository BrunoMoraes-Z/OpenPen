//! Export de código: a node subtree of a [`SceneDoc`] -> HTML + CSS or HTML + Tailwind.
//!
//! The walk ([`build`]) turns resolved scene nodes plus their computed layout
//! ([`LayoutEngine`]) into a neutral element tree (tag, attributes, CSS
//! declarations, children); two emitters print it:
//! - [`CodeFormat::HtmlCss`]: `index.html` + `styles.css` with readable, deterministic
//!   class names derived from the layer names;
//! - [`CodeFormat::HtmlTailwind`]: one `index.html` with Tailwind v4 utility classes
//!   (arbitrary values where needed) and the browser build of Tailwind.
//!
//! Variables become CSS custom properties (`:root` plus one `[data-<axis>="<value>"]`
//! block per theme value) and properties bound to them use `var(--x)`. Image fills are
//! copied next to the output (`assets/`) and referenced by relative URLs.
//!
//! ```ignore
//! let out = pen_codegen::generate(&scene, &pipe.layout, &[key], &CodegenOptions::default());
//! out.write_to(Path::new("out"))?; // index.html, styles.css, assets/*
//! ```

mod build;
mod emit;
mod model;
mod paint;
mod svg;
mod tailwind;
mod vars;

use std::path::{Path, PathBuf};

use pen_doc::{NodeKey, SceneDoc};
use pen_layout::layout::LayoutEngine;

/// Output flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodeFormat {
    /// `index.html` + `styles.css` (classes named after the layers).
    #[default]
    HtmlCss,
    /// `index.html` with Tailwind v4 utility classes.
    HtmlTailwind,
}

impl CodeFormat {
    /// `html-css` / `html-tailwind` (the names used by `Export`).
    pub fn parse(s: &str) -> Option<CodeFormat> {
        match s {
            "html-css" => Some(CodeFormat::HtmlCss),
            "html-tailwind" => Some(CodeFormat::HtmlTailwind),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CodeFormat::HtmlCss => "html-css",
            CodeFormat::HtmlTailwind => "html-tailwind",
        }
    }
}

/// Code generation options.
#[derive(Debug, Clone, PartialEq)]
pub struct CodegenOptions {
    pub format: CodeFormat,
    /// Full HTML document (`<!doctype html>`, `<head>`, `<body>`). Off: just the markup.
    pub scaffold: bool,
    /// `<!-- component: Name -->` before each component instance.
    pub component_comments: bool,
    /// `data-name` attribute with the layer name.
    pub layer_names: bool,
    /// `data-id` attribute with the node id path.
    pub layer_ids: bool,
    /// `<link>` to Google Fonts for the (non-system) font families used.
    pub google_fonts: bool,
    /// `<title>` of the document (default: the name of the first exported node).
    pub title: Option<String>,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            format: CodeFormat::HtmlCss,
            scaffold: true,
            component_comments: true,
            layer_names: false,
            layer_ids: false,
            google_fonts: true,
            title: None,
        }
    }
}

impl CodegenOptions {
    pub fn with_format(format: CodeFormat) -> Self {
        Self {
            format,
            ..Self::default()
        }
    }
}

/// A file to copy next to the generated code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFile {
    /// Absolute (or document-relative, as resolved) source path.
    pub source: PathBuf,
    /// Destination relative to the output directory (`assets/photo.png`).
    pub path: String,
}

/// Generated code.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodeOutput {
    /// `index.html`.
    pub html: String,
    /// `styles.css` (html-css only).
    pub css: Option<String>,
    /// Images referenced by relative URLs.
    pub assets: Vec<AssetFile>,
    /// Things that could not be mapped exactly (deduplicated, sorted).
    pub warnings: Vec<String>,
}

/// File name of the HTML page written by [`CodeOutput::write_to`].
pub const HTML_FILE: &str = "index.html";
/// File name of the stylesheet written by [`CodeOutput::write_to`].
pub const CSS_FILE: &str = "styles.css";

impl CodeOutput {
    /// Writes `index.html`, `styles.css` (html-css) and the assets into `dir`
    /// (created if needed). Returns the written paths.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.write_html_as(&dir.join(HTML_FILE))
    }

    /// Like [`Self::write_to`] with an explicit HTML path; the stylesheet and the assets go
    /// next to it.
    pub fn write_html_as(&self, html_path: &Path) -> std::io::Result<Vec<PathBuf>> {
        let dir = html_path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let mut out = Vec::new();
        std::fs::write(html_path, &self.html)?;
        out.push(html_path.to_path_buf());
        if let Some(css) = &self.css {
            let p = dir.join(CSS_FILE);
            std::fs::write(&p, css)?;
            out.push(p);
        }
        for a in &self.assets {
            let dest = dir.join(&a.path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&a.source, &dest)?;
            out.push(dest);
        }
        Ok(out)
    }
}

/// Generates code for `roots` (empty = every top-level node of the document).
///
/// `layout` must be up to date for `scene` (e.g. `Pipeline::layout` after `rebuild`).
pub fn generate(
    scene: &SceneDoc,
    layout: &LayoutEngine,
    roots: &[NodeKey],
    opts: &CodegenOptions,
) -> CodeOutput {
    let all: Vec<NodeKey>;
    let roots = if roots.is_empty() {
        all = scene.roots().to_vec();
        &all[..]
    } else {
        roots
    };
    let mut b = build::Builder::new(scene, layout, opts);
    let tree = b.build_roots(roots);
    emit::emit(b, tree, roots)
}
