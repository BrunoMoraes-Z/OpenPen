//! The seam between the MCP protocol layer and whatever owns the documents.
//!
//! `pen-mcp` knows nothing about the scene graph or the script engine: the GUI
//! app and the headless CLI each implement [`ToolBackend`] and hand it to one of
//! the transports in this crate.

use std::path::PathBuf;

use futures::future::BoxFuture;

use crate::tools::{ExecuteArgs, ImportWebsiteArgs};

/// Who is calling: one MCP session (one socket connection or one stdio server).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallCtx {
    /// Unique per connection for the lifetime of the server (`client-1`, `stdio`, ...).
    pub client_id: String,
    /// Display name of the agent: `--agent` of the shim, else the MCP `clientInfo.name`.
    pub agent_name: String,
    /// Id of the model's tool call, from the request `_meta`
    /// ([`TOOL_USE_ID_META`], sent by Claude Code). Only set on `tools/call`;
    /// lets the app match a final `execute` with the stream of the same call.
    pub tool_use_id: Option<String>,
}

/// `_meta` key with which Claude Code tags each MCP tool call with the id of
/// the model's `tool_use` block.
pub const TOOL_USE_ID_META: &str = "claudecode/toolUseId";

/// What a tool returns to the agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOutput {
    /// Markdown/plain text shown to the model.
    pub text: String,
    /// PNG images (screenshots), sent as MCP `image` content after the text.
    pub images: Vec<Vec<u8>>,
    /// The tool ran but failed in a way the model should read and fix.
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// A tool-level failure (MCP `isError: true`) with a message for the model.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            is_error: true,
        }
    }

    pub fn with_image(mut self, png: Vec<u8>) -> Self {
        self.images.push(png);
        self
    }
}

/// Errors a backend can report. How each maps onto MCP:
///
/// | variant | MCP result |
/// |---|---|
/// | `NoDocument`, `Failed` | tool result with `isError: true` (the model reads the text) |
/// | `InvalidParams` | JSON-RPC error `-32602` |
/// | `Unavailable` | JSON-RPC error `-32603` (internal) |
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// No document matches the `uri`, or nothing is focused.
    #[error("{0}")]
    NoDocument(String),
    /// The operation ran and failed (script error, I/O error, ...).
    #[error("{0}")]
    Failed(String),
    /// The arguments are malformed beyond what the schema can express.
    #[error("{0}")]
    InvalidParams(String),
    /// The backend cannot serve requests (app shutting down, channel closed, ...).
    #[error("{0}")]
    Unavailable(String),
}

pub type ToolResult = Result<ToolOutput, ToolError>;

/// Implemented by the GUI app (hops to the main thread and awaits a oneshot)
/// and by the headless backend. Futures must be `'static`: clone what you need
/// out of `&self` before the `async move` block.
pub trait ToolBackend: Send + Sync + 'static {
    /// Summary of the editor: focused document, selection, top-level nodes, ...
    fn get_app_state(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult>;

    /// Runs (or patches and re-runs) a script against a document, as one undo step.
    fn execute(&self, ctx: CallCtx, args: ExecuteArgs) -> BoxFuture<'static, ToolResult>;

    /// Writes the document to disk.
    fn save(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult>;

    /// Imports a web page as a new top-level frame (`import_website`).
    /// `url` is already normalized ([`crate::tools::normalize_web_url`]).
    /// The default answers that the backend cannot do it.
    fn import_website(
        &self,
        ctx: CallCtx,
        args: ImportWebsiteArgs,
        url: String,
    ) -> BoxFuture<'static, ToolResult> {
        let _ = (ctx, args, url);
        Box::pin(std::future::ready(Err(ToolError::Failed(
            "This OpenPen server cannot import web pages.".into(),
        ))))
    }

    /// A tool this crate does not know (not listed in `tools/list`), e.g. an
    /// app's debug hook. `None` (the default) answers "unknown tool".
    fn call_extension(
        &self,
        ctx: CallCtx,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<BoxFuture<'static, ToolResult>> {
        let _ = (ctx, name, args);
        None
    }

    /// A session made its first request (the agent name is known by then).
    fn agent_connected(&self, ctx: &CallCtx) {
        let _ = ctx;
    }

    /// A session that was reported by `agent_connected` has ended.
    fn agent_disconnected(&self, ctx: &CallCtx) {
        let _ = ctx;
    }
}

/// Converts a tool `uri` (plain path or `file://` URI) into a filesystem path.
///
/// Percent-escapes are decoded; `file:///C:/x` becomes `C:/x` on Windows.
/// Returns `None` for other URI schemes.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let uri = uri.trim();
    let Some(rest) = strip_prefix_ignore_case(uri, "file://") else {
        // Something like `http://...` is not a document we can open.
        if let Some((scheme, _)) = uri.split_once("://")
            && !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+')
        {
            return None;
        }
        return Some(PathBuf::from(uri));
    };
    // Drop an authority of `localhost` (file://localhost/path).
    let rest = strip_prefix_ignore_case(rest, "localhost").unwrap_or(rest);
    let decoded = percent_decode(rest)?;
    // `/C:/dir` -> `C:/dir`.
    let b = decoded.as_bytes();
    let path = if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        &decoded[1..]
    } else {
        &decoded[..]
    };
    Some(PathBuf::from(path))
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = (bytes.next()? as char).to_digit(16)?;
            let lo = (bytes.next()? as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_forms() {
        assert_eq!(uri_to_path("a/b.pen"), Some(PathBuf::from("a/b.pen")));
        assert_eq!(
            uri_to_path("C:\\x\\y.pen"),
            Some(PathBuf::from("C:\\x\\y.pen"))
        );
        assert_eq!(
            uri_to_path("file:///home/me/My%20Design.pen"),
            Some(PathBuf::from("/home/me/My Design.pen"))
        );
        assert_eq!(
            uri_to_path("FILE:///C:/Users/x.pen"),
            Some(PathBuf::from("C:/Users/x.pen"))
        );
        assert_eq!(
            uri_to_path("file://localhost/tmp/a.pen"),
            Some(PathBuf::from("/tmp/a.pen"))
        );
        assert_eq!(uri_to_path("https://example.com/a.pen"), None);
        assert_eq!(uri_to_path("file:///bad%zz"), None);
    }
}
