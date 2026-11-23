//! The [`ToolBackend`] of the GUI: MCP calls arrive on the server's Tokio
//! threads, are sent over an unbounded channel to a task on the GPUI
//! foreground (see `hub`), and the answer comes back through a oneshot.

use futures::FutureExt as _;
use futures::channel::{mpsc, oneshot};
use pen_mcp::{BoxFuture, CallCtx, ExecuteArgs, ToolBackend, ToolError, ToolResult};

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
    Connected(CallCtx),
    Disconnected(CallCtx),
}

pub type RequestRx = mpsc::UnboundedReceiver<AgentRequest>;

/// Sends every call to the foreground.
#[derive(Debug, Clone)]
pub struct GuiBackend {
    tx: mpsc::UnboundedSender<AgentRequest>,
}

const GONE: &str = "OpenPen is shutting down";

impl GuiBackend {
    pub fn new() -> (Self, RequestRx) {
        let (tx, rx) = mpsc::unbounded();
        (Self { tx }, rx)
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

    fn agent_connected(&self, ctx: &CallCtx) {
        let _ = self.tx.unbounded_send(AgentRequest::Connected(ctx.clone()));
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
