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
//!
//! Animation: shaders with `@time` / `@mouse` uniforms read them from a per-node
//! [`ShaderAnim`] (set with [`crate::Pipeline::animate_shaders`]); without one they get
//! t = 0 and the node center, as in Pen's exports. While a new frame renders, the engine
//! answers [`ShaderLookup::Updating`] with the previous frame of the same slot (no flicker).

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
    /// Declares a `@time` uniform.
    pub time: bool,
    /// Declares a `@mouse` uniform.
    pub mouse: bool,
}

impl ShaderNeeds {
    /// Uses `@time` or `@mouse` (Pen's `isAnimated`).
    pub fn animated(&self) -> bool {
        self.time || self.mouse
    }
}

/// Animation inputs of one node's shaders (editor only; exports use the default).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ShaderAnim {
    /// `@time`, in seconds.
    pub time: f32,
    /// `@mouse` in node-local units (y down, origin at the top-left corner); `None` =
    /// the node center.
    pub mouse: Option<Vec2>,
}

/// A node whose shaders are animated (recorded by the display-list builder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimatedShader {
    /// Top-level root whose chunk draws the node (rebuild it to show a new frame).
    pub root: pen_render::NodeKey,
    /// Some shader of the node uses `@time`.
    pub time: bool,
    /// Some shader of the node uses `@mouse`.
    pub mouse: bool,
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
    /// `@time` (seconds; 0 outside the editor).
    pub time: f32,
    /// `@mouse`, node-local, y down (`None` = node center).
    pub mouse: Option<Vec2>,
    /// Stable identity of the fill/effect (node + position) across frames, 0 = none: an
    /// animated shader shows its last frame for this slot while the next one renders.
    pub slot: u64,
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
    /// A newer frame of an animated shader is being rendered; this is the previous one.
    Updating(Arc<ImageData>),
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
    /// True while deferred renders are queued or running (the app waits for them before
    /// advancing an animation).
    fn busy(&self) -> bool {
        false
    }
}
