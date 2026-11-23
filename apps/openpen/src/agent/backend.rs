//! The [`ToolBackend`] of the GUI: MCP calls arrive on the server's Tokio
//! threads, are sent over an unbounded channel to a task on the GPUI
//! foreground (see `hub`), and the answer comes back through a oneshot.
//!
//! The same channel carries the streaming `execute` updates
//! ([`GuiBackend::stream_partial`], [`GuiBackend::stream_cancel`], see
//! `agent::stream`), which any thread (the embedded chat) can send.

use futures::FutureExt as _;
use futures::channel::{mpsc, oneshot};
use gpui_kit::EntityId;
use std::path::PathBuf;

use pen_mcp::{
    BoxFuture, CallCtx, ExecuteArgs, ImportWebsiteArgs, ImportedPage, ToolBackend, ToolError,
    ToolOutput, ToolResult,
};

use crate::web_import::{self, ImportRequest};

/// Environment variable that enables the hidden MCP tool
/// [`DEBUG_STREAM_TOOL`] (`1`), to drive streaming from outside the app.
pub const DEBUG_STREAM_ENV: &str = "OPENPEN_DEBUG_STREAM";

/// Hidden debug tool: `{toolUseId, input, uri?, agentId?, cancel?}` feeds (or
/// cancels) a stream as the embedded chat would. Not listed in `tools/list`.
pub const DEBUG_STREAM_TOOL: &str = "debug_stream_partial";

/// A request for the foreground.
#[derive(Debug)]
pub enum AgentRequest {
    AppState {
        ctx: CallCtx,
        uri: Option<String>,
        reply: oneshot::Sender<ToolResult>,
    },
    Execute {
        ctx: CallCtx,
        args: ExecuteArgs,
        reply: oneshot::Sender<ToolResult>,
    },
    Save {
        ctx: CallCtx,
        uri: Option<String>,
        reply: oneshot::Sender<ToolResult>,
    },
    /// `import_website`, step 1: the saved document a page will go into.
    ImportTarget {
        ctx: CallCtx,
        uri: Option<String>,
        reply: oneshot::Sender<Result<PathBuf, ToolError>>,
    },
    /// `import_website`, step 2: insert the converted frame into the
    /// document at `path`; replies with the new frame id.
    ImportInsert {
        ctx: CallCtx,
        path: PathBuf,
        frame: serde_json::Value,
        reply: oneshot::Sender<Result<String, ToolError>>,
    },
    Connected(CallCtx),
    Disconnected(CallCtx),
    /// The growing source of an `execute` call (see `agent::stream`).
    StreamPartial {
        agent_id: String,
        tool_use_id: String,
        uri: Option<String>,
        prefix: String,
    },
    /// Roll back a stream (by tool use id, else the agent's).
    StreamCancel {
        agent_id: String,
        tool_use_id: Option<String>,
    },
    /// A document tab is closing: its streams are cancelled.
    DocClosed(EntityId),
}

pub type RequestRx = mpsc::UnboundedReceiver<AgentRequest>;

/// Sends every call to the foreground.
#[derive(Debug, Clone)]
pub struct GuiBackend {
    tx: mpsc::UnboundedSender<AgentRequest>,
    debug_stream: bool,
}

const GONE: &str = "OpenPen is shutting down";

impl GuiBackend {
    pub fn new() -> (Self, RequestRx) {
        let (tx, rx) = mpsc::unbounded();
        (
            Self {
                tx,
                debug_stream: false,
            },
            rx,
        )
    }

    /// Enables the hidden [`DEBUG_STREAM_TOOL`].
    pub fn with_debug_stream(mut self, on: bool) -> Self {
        self.debug_stream = on;
        self
    }

    /// Reports the longest decoded prefix of the JS source of a streaming
    /// `execute` call (`tool_use_id`) of `agent_id` (the `client_id` its
    /// final call will carry, unless matched by tool use id). `doc_uri` picks
    /// the document like the tool's `uri` (`None`: the focused one). Cheap
    /// and non-blocking, callable from any thread: the foreground coalesces
    /// the updates. False if the app is gone.
    pub fn stream_partial(
        &self,
        agent_id: impl Into<String>,
        tool_use_id: impl Into<String>,
        doc_uri: Option<String>,
        input_prefix: String,
    ) -> bool {
        self.tx
            .unbounded_send(AgentRequest::StreamPartial {
                agent_id: agent_id.into(),
                tool_use_id: tool_use_id.into(),
                uri: doc_uri,
                prefix: input_prefix,
            })
            .is_ok()
    }

    /// Rolls back a stream (e.g. the user stopped the chat, or the tool call
    /// was abandoned). By tool use id, else the agent's stream.
    pub fn stream_cancel(&self, agent_id: impl Into<String>, tool_use_id: Option<String>) -> bool {
        self.tx
            .unbounded_send(AgentRequest::StreamCancel {
                agent_id: agent_id.into(),
                tool_use_id,
            })
            .is_ok()
    }

    /// A document tab is closing.
    pub fn doc_closed(&self, id: EntityId) {
        let _ = self.tx.unbounded_send(AgentRequest::DocClosed(id));
    }

    fn call(
        &self,
        make: impl FnOnce(oneshot::Sender<ToolResult>) -> AgentRequest,
    ) -> BoxFuture<'static, ToolResult> {
        let (reply, rx) = oneshot::channel();
        let sent = self.tx.unbounded_send(make(reply)).is_ok();
        async move {
            if !sent {
                return Err(ToolError::Unavailable(GONE.into()));
            }
            rx.await
                .unwrap_or_else(|_| Err(ToolError::Unavailable(GONE.into())))
        }
        .boxed()
    }
}

impl ToolBackend for GuiBackend {
    fn get_app_state(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        self.call(|reply| AgentRequest::AppState { ctx, uri, reply })
    }

    fn execute(&self, ctx: CallCtx, args: ExecuteArgs) -> BoxFuture<'static, ToolResult> {
        self.call(|reply| AgentRequest::Execute { ctx, args, reply })
    }

    fn save(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        self.call(|reply| AgentRequest::Save { ctx, uri, reply })
    }

    /// Resolves the target document on the foreground, runs the browser
    /// import on a background thread (dropping this future, e.g. when the
    /// agent cancels, kills the browser), then inserts the frame on the
    /// foreground like the dialog does.
    fn import_website(
        &self,
        ctx: CallCtx,
        args: ImportWebsiteArgs,
        url: String,
    ) -> BoxFuture<'static, ToolResult> {
        let tx = self.tx.clone();
        async move {
            let gone = || ToolError::Unavailable(GONE.into());
            let (reply, rx) = oneshot::channel();
            tx.unbounded_send(AgentRequest::ImportTarget {
                ctx: ctx.clone(),
                uri: args.uri.clone(),
                reply,
            })
            .map_err(|_| gone())?;
            let path = rx.await.map_err(|_| gone())??;
            let mut req = ImportRequest::from_mcp(&args, url.clone());
            req.assets_dir = web_import::assets_dir_for(&path);
            let t0 = std::time::Instant::now();
            let mut job = web_import::spawn(req);
            let outcome = (&mut job.result)
                .await
                .unwrap_or(Err(web_import::Failure::Cancelled))
                .map_err(|f| ToolError::Failed(f.message()))?;
            let (reply, rx) = oneshot::channel();
            tx.unbounded_send(AgentRequest::ImportInsert {
                ctx,
                path,
                frame: outcome.frame(),
                reply,
            })
            .map_err(|_| gone())?;
            let frame_id = rx.await.map_err(|_| gone())??;
            let mut stats = outcome.stats.clone();
            stats.push((
                "time".into(),
                format!("{:.1} s", t0.elapsed().as_secs_f32()),
            ));
            Ok(ImportedPage {
                frame_id,
                title: outcome.title.clone(),
                url,
                size: outcome.size(),
                stats,
                warnings: outcome.warnings.clone(),
                assets: outcome.assets_written.len(),
                screenshot: outcome.screenshot_png.clone(),
            }
            .to_output())
        }
        .boxed()
    }

    fn agent_connected(&self, ctx: &CallCtx) {
        let _ = self.tx.unbounded_send(AgentRequest::Connected(ctx.clone()));
    }

    fn call_extension(
        &self,
        ctx: CallCtx,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Option<BoxFuture<'static, ToolResult>> {
        if !self.debug_stream || name != DEBUG_STREAM_TOOL {
            return None;
        }
        let str_arg = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let agent = str_arg("agentId").unwrap_or(ctx.client_id);
        let Some(tool) = str_arg("toolUseId") else {
            return Some(
                std::future::ready(Err(ToolError::InvalidParams(
                    "`toolUseId` is required".into(),
                )))
                .boxed(),
            );
        };
        let sent = if args.get("cancel").and_then(|v| v.as_bool()) == Some(true) {
            self.stream_cancel(agent, Some(tool))
        } else {
            let input = str_arg("input").unwrap_or_default();
            self.stream_partial(agent, tool, str_arg("uri"), input)
        };
        let r = if sent {
            Ok(ToolOutput::text("queued"))
        } else {
            Err(ToolError::Unavailable(GONE.into()))
        };
        Some(std::future::ready(r).boxed())
    }

    fn agent_disconnected(&self, ctx: &CallCtx) {
        let _ = self
            .tx
            .unbounded_send(AgentRequest::Disconnected(ctx.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;

    fn ctx() -> CallCtx {
        CallCtx {
            client_id: "client-1".into(),
            agent_name: "claude".into(),
            ..Default::default()
        }
    }

    #[test]
    fn calls_round_trip_through_the_channel() {
        let (backend, mut rx) = GuiBackend::new();
        let fut = backend.get_app_state(ctx(), Some("a.pen".into()));
        match futures::executor::block_on(rx.next()) {
            Some(AgentRequest::AppState { uri, reply, ctx }) => {
                assert_eq!(uri.as_deref(), Some("a.pen"));
                assert_eq!(ctx.agent_name, "claude");
                reply.send(Ok(pen_mcp::ToolOutput::text("ok"))).unwrap();
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(futures::executor::block_on(fut).unwrap().text, "ok");
        backend.agent_connected(&ctx());
        assert!(matches!(
            futures::executor::block_on(rx.next()),
            Some(AgentRequest::Connected(_))
        ));
    }

    #[test]
    fn stream_updates_and_debug_tool_go_through_the_channel() {
        let (backend, mut rx) = GuiBackend::new();
        assert!(backend.stream_partial("chat-1", "toolu_1", None, "a = 1\n".into()));
        match futures::executor::block_on(rx.next()) {
            Some(AgentRequest::StreamPartial {
                agent_id,
                tool_use_id,
                uri,
                prefix,
            }) => {
                assert_eq!(
                    (agent_id.as_str(), tool_use_id.as_str()),
                    ("chat-1", "toolu_1")
                );
                assert_eq!((uri, prefix.as_str()), (None, "a = 1\n"));
            }
            other => panic!("{other:?}"),
        }
        // The debug tool is off unless enabled.
        let args = serde_json::json!({"toolUseId": "t9", "input": "x"});
        let args = args.as_object().unwrap().clone();
        assert!(
            backend
                .call_extension(ctx(), DEBUG_STREAM_TOOL, args.clone())
                .is_none()
        );
        let backend = backend.with_debug_stream(true);
        let fut = backend
            .call_extension(ctx(), DEBUG_STREAM_TOOL, args)
            .unwrap();
        assert_eq!(futures::executor::block_on(fut).unwrap().text, "queued");
        assert!(matches!(
            futures::executor::block_on(rx.next()),
            Some(AgentRequest::StreamPartial { agent_id, .. }) if agent_id == "client-1"
        ));
        assert!(backend.stream_cancel("chat-1", None));
        assert!(matches!(
            futures::executor::block_on(rx.next()),
            Some(AgentRequest::StreamCancel {
                tool_use_id: None,
                ..
            })
        ));
        drop(rx);
        assert!(!backend.stream_partial("chat-1", "t", None, String::new()));
    }

    #[test]
    fn dropped_reply_or_receiver_is_unavailable() {
        let (backend, mut rx) = GuiBackend::new();
        let fut = backend.save(ctx(), None);
        drop(futures::executor::block_on(rx.next()));
        assert!(matches!(
            futures::executor::block_on(fut),
            Err(ToolError::Unavailable(_))
        ));
        drop(rx);
        let fut = backend.execute(ctx(), ExecuteArgs::default());
        assert!(matches!(
            futures::executor::block_on(fut),
            Err(ToolError::Unavailable(_))
        ));
    }
}
