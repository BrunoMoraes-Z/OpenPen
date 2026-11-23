//! `shader` fills and effects: the contract with the shader engine.
//!
//! The display list has no shader primitive: a shader is evaluated (by a
//! [`ShaderRenderer`], implemented by `pen-shader`) into an image over the node's local
//! space, and the builder draws that image like an image fill (fills) or in place of the
//! node's rendering (effects). Every backend (GPU canvas, CPU raster, SVG/PDF export)
//! then draws shaders without knowing about them.
//!
//! Like images, rendering may be deferred: [`ShaderLookup::Pending`] makes the builder
//! skip the shader and remember the chunk; [`ShaderRenderer::take_ready`] reports that
//! results arrived, and [`crate::Pipeline::refresh_assets`] rebuilds those chunks.

use pen_doc::ParamValue;
use pen_render::{DisplayList, ImageData, Rect, Shape, Vec2};
use std::path::PathBuf;
use std::sync::Arc;

/// What a shader samples besides its own uniforms (answered from its source).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShaderNeeds {
    /// Some sampler is bound to `pencil:object`.
    pub object: bool,
    /// Some sampler is bound to `pencil:background`.
    pub backdrop: bool,
}

/// One shader evaluation, in the node's local space (y down, origin at the node's
/// top-left corner).
#[derive(Debug, Clone)]
pub struct ShaderRequest {
    /// `url` of the fill/effect (relative to `base_dir`).
    pub url: Arc<str>,
    pub base_dir: Option<PathBuf>,
    pub uniforms: Arc<[(Arc<str>, ParamValue)]>,
    /// Node box size: `@resolution`.
    pub size: Vec2,
    /// Area to render (fills: the node box; effects: the node's visual bounds).
    pub region: Rect,
    /// Node shape (for `@sdf` of fills).
    pub shape: Option<Shape>,
    /// `pencil:object` content, local space.
    pub object: Option<Arc<DisplayList>>,
    /// `pencil:background` content, local space.
    pub backdrop: Option<Arc<DisplayList>>,
    /// Identity of everything above (computed by the builder), for caching.
    pub key: u64,
}

/// Result of [`ShaderRenderer::render`].
#[derive(Debug, Clone)]
pub enum ShaderLookup {
    /// Image covering `region` (premultiplied RGBA8).
    Ready(Arc<ImageData>),
    /// Being rendered in the background.
    Pending,
    /// Compile or runtime error.
    Failed(Arc<str>),
}

/// Evaluates shaders (see the module docs).
pub trait ShaderRenderer: Send + Sync {
    /// Which special textures the shader at `url` samples (parses it if needed).
    fn needs(&self, url: &str, base_dir: Option<&std::path::Path>) -> ShaderNeeds;
    fn render(&self, req: &ShaderRequest) -> ShaderLookup;
    /// True (once) when deferred renders finished since the last call.
    fn take_ready(&self) -> bool {
        false
    }
}
