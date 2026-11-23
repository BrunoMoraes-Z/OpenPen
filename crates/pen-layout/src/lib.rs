//! Layout flex dos frames .pen (taffy), texto compartilhado (cosmic-text) e
//! construção da `DisplayList`.
//!
//! Typical use:
//! ```ignore
//! let mut scene = pen_doc::SceneDoc::load(&bytes, Some(dir), &pen_doc::FsLibraries::default())?.0;
//! let mut pipe = Pipeline::new(TextEngine::new(), Box::new(FsAssets::new(Some(dir.into()))));
//! pipe.rebuild(&scene);                 // full layout + display list -> pipe.list
//! scene.apply(op)?;                     // edit (or UndoStack::apply)
//! pipe.update(&mut scene);              // incremental: only dirty roots
//! ```

pub mod assets;
pub mod display;
pub mod icons;
pub mod layout;
pub mod path;
pub mod pipeline;
pub mod script;
pub mod shader;
pub mod text;
pub mod vector;

pub use assets::{AssetResolver, BundledIcons, FsAssets, IconGlyph, ImageLookup, NoAssets};
pub use icons::{IconError, IconRef, bundled_icon, resolve_icon};

/// Text fonts every text backend must register so measuring (here) and drawing
/// agree: `(file name, TTF bytes)` of the bundled Inter faces (family "Inter",
/// OFL-1.1). pen-layout's and pen-raster's engines load them automatically; the
/// GPUI app passes the bytes to `cx.text_system().add_fonts(..)`. Icon fonts are
/// not included: icons are emitted as vector paths.
pub fn bundled_fonts() -> Vec<(&'static str, &'static [u8])> {
    pen_fonts::text_fonts()
        .iter()
        .map(|f| (f.file, f.data))
        .collect()
}

/// Starts scanning the installed system fonts on a background thread. Optional: without
/// it, the first text that names a non-bundled family scans them on demand (~20 ms,
/// once per process). See [`pen_fonts::system`].
pub fn prewarm_system_fonts() {
    pen_fonts::system::prewarm();
}

/// True if `data` is a bundled font file (Inter or an icon font): no need to load it into a
/// [`TextEngine`].
pub fn is_bundled_font(data: &[u8]) -> bool {
    pen_fonts::is_bundled(data)
}

/// Family name of the bundled default sans (used when a family is missing).
pub const DEFAULT_SANS_FAMILY: &str = pen_fonts::DEFAULT_SANS;
pub use display::{DisplayListBuilder, local_transform, node_world_rect, parent_world};
pub use layout::{LayoutEngine, NodeLayout};
pub use pipeline::{Pipeline, UpdateStats};
pub use script::{ScriptError, ScriptExpander, ScriptRequest, ScriptRunner, ScriptStats};
pub use shader::{ShaderLookup, ShaderNeeds, ShaderRenderer, ShaderRequest};
pub use text::{TextEngine, TextLayout, text_block};
