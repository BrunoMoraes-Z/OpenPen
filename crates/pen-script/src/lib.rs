//! Motor do `execute` (MCP): JavaScript em QuickJS contra o documento, em uma transação.
//!
//! An AI agent sends a JavaScript snippet; [`ScriptEngine::execute`] runs it in
//! a fresh QuickJS context (via `rquickjs`) whose globals (`Insert`, `Copy`,
//! `Update`, `Replace`, `Move`, `Delete`, `SetVariables`, `GetVariables`,
//! `Get`, `FindEmptySpace`, `Print`, `TakeScreenshot`, `Export`) operate on the
//! open [`pen_doc::SceneDoc`]. The whole call is one transaction: on success
//! its document ops become **one** undo step; on any uncaught error every
//! change is reverted and the response carries an `editId` so the agent can
//! patch the snippet with find/replace edits instead of resending it.
//!
//! - Mutations apply immediately (later `Get`s and bounds see them; layout is
//!   recomputed lazily before reads that need it).
//! - Node data is converted with pen-schema; invalid/unknown properties become
//!   warnings. Ids are always generated.
//! - Limits: heap, native stack, CPU per evaluated chunk, wall-clock total.
//! - Globals assigned without `const`/`let` persist per agent (JSON values).
//! - Streaming: [`ScriptEngine::execute_partial`] evaluates the complete
//!   statements of a snippet that is still arriving ([`complete_prefix`]).
//!
//! `ScriptEngine` is `Send` and works synchronously on the caller's thread.

mod engine;
pub mod errors;
pub mod host;
mod nodes;
mod resolve;
pub mod stream;
mod tx;

pub use engine::{
    Edit, ExecuteRequest, ExecuteResponse, Limits, PartialStatus, ScriptEngine, ScriptError,
    apply_edits,
};
pub use host::{
    ExportFormat, ExportRequest, HeadlessHost, RenderTarget, SCREENSHOT_MAX_SIDE, ScriptHost,
    export_images, render_rgba, screenshot_png,
};
pub use nodes::ROOT_ALIASES;
pub use stream::complete_prefix;
pub use tx::Touch;
