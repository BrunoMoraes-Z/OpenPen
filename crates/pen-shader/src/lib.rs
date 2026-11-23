//! Shaders GLSL (WebGL 1.0) dos fills/effects `.pen`: anotações, front-end naga e
//! avaliador em CPU.
//!
//! - [`glsl`]: dialect rewrite (`texture2D`, `gl_FragColor`, uniforms -> globals) and
//!   parsing with naga's GLSL frontend; uniform annotations.
//! - [`eval`]: interpreter of naga IR (one `main` per pixel).
//! - [`texture`]: sampled textures (images, `pencil:object` / `pencil:background` bakes,
//!   `@sdf`).
//! - [`engine`]: uniform binding, parallel pixel loop, caches; [`ShaderEngine`]
//!   implements [`pen_layout::ShaderRenderer`] (sync for CLI/tests, deferred for the app).
//!
//! See `docs/scripts-shaders.md`.

pub mod engine;
pub mod eval;
pub mod glsl;
pub mod gpu;
pub mod texture;

pub use engine::{
    Compiled, RenderInput, ShaderEngine, ShaderLimits, ShaderStats, pick_scale, render,
};
