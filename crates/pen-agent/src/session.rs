//! The engine-independent session handle: a worker thread with its own Tokio
//! runtime, a command channel in and an event channel out.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread::JoinHandle;

use tokio::sync::mpsc;

use crate::{AgentError, AgentEvent, EngineConfig, SessionConfig, UserMessage};

pub(crate) enum Command {
    Send(UserMessage),
    Steer(String),
    Stop,
    Shutdown,
}

pub(crate) struct Shared {
    session_id: Mutex<Option<String>>,
    agent_name: Option<String>,
}

/// Sends events to the app and remembers the session ID.
#[derive(Clone)]
pub(crate) struct Emitter {
    tx: mpsc::UnboundedSender<AgentEvent>,
    shared: Arc<Shared>,
}

impl Emitter {
    pub fn emit(&self, e: AgentEvent) {
        if let AgentEvent::SessionId(id) = &e {
            let mut slot = self
                .shared
                .session_id
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if slot.as_deref() == Some(id.as_str()) {
                return;
            }
            *slot = Some(id.clone());
        }
        let _ = self.tx.send(e);
    }

    pub fn session_id(&self) -> Option<String> {
        self.shared
            .session_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// Receives the events of a session. Works from any thread: `blocking_recv`
/// on a plain thread, `recv().await` (or the `Stream` impl) in any async
/// executor. Ends (`None`) after the session is dropped.
pub struct EventReceiver(mpsc::UnboundedReceiver<AgentEvent>);

impl EventReceiver {
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.0.recv().await
    }

    /// Must not be called from inside an async context.
    pub fn blocking_recv(&mut self) -> Option<AgentEvent> {
        self.0.blocking_recv()
    }

    pub fn try_recv(&mut self) -> Option<AgentEvent> {
        self.0.try_recv().ok()
    }
}

impl futures::Stream for EventReceiver {
    type Item = AgentEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<AgentEvent>> {
        self.0.poll_recv(cx)
    }
}

/// A running chat session. Dropping it stops the agent (the Claude Code
/// process is killed) and ends the event stream.
pub struct Session {
    cmd: mpsc::UnboundedSender<Command>,
    events: Option<EventReceiver>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// Signals the end of the worker thread.
    finished: Option<std::sync::mpsc::Receiver<()>>,
}

impl Session {
    /// Sends a user message. While a turn is running, the message is queued
    /// and runs after it (use [`Session::steer`] to talk to the running turn).
    pub fn send(&self, msg: UserMessage) -> Result<(), AgentError> {
        self.cmd
            .send(Command::Send(msg))
            .map_err(|_| AgentError::Closed)
    }

    /// Shorthand for [`Session::send`] with text and attachments.
    pub fn send_text(
        &self,
        text: impl Into<String>,
        attachments: Vec<crate::Attachment>,
    ) -> Result<(), AgentError> {
        self.send(UserMessage {
            text: text.into(),
            attachments,
            context: None,
        })
    }

    /// Adds a user message to the running turn. API engine: it is appended
    /// after the next tool results (or starts a follow-up request when the
    /// model finishes first). CLI engine: it is written to Claude Code's
    /// input, which delivers it during or right after the current turn.
    /// Without a running turn it behaves like `send`.
    pub fn steer(&self, text: impl Into<String>) -> Result<(), AgentError> {
        self.cmd
            .send(Command::Steer(text.into()))
            .map_err(|_| AgentError::Closed)
    }

    /// Stops the running turn (a `TurnEnd { stop_reason: "interrupted" }`
    /// follows) and drops queued messages. The session stays usable.
    pub fn stop(&self) {
        let _ = self.cmd.send(Command::Stop);
    }

    /// The event stream. Can be taken once.
    pub fn take_events(&mut self) -> Option<EventReceiver> {
        self.events.take()
    }

    /// ID to resume this conversation later (known after the first turn
    /// starts; also announced as [`AgentEvent::SessionId`]).
    pub fn session_id(&self) -> Option<String> {
        self.shared
            .session_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// CLI engine: the agent name given to the MCP server
    /// (`OpenPen Chat#<marker>`), which the app sees as `CallCtx::agent_name`
    /// for this session's tool calls. `None` for the API engine.
    pub fn agent_name(&self) -> Option<&str> {
        self.shared.agent_name.as_deref()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.cmd.send(Command::Shutdown);
        // The worker drops the running turn and kills the CLI process right
        // away; wait for that, but never block the caller (often the UI
        // thread) for long.
        let done = self
            .finished
            .take()
            .is_some_and(|rx| rx.recv_timeout(std::time::Duration::from_secs(3)).is_ok());
        if let Some(t) = self.thread.take()
            && done
        {
            let _ = t.join();
        }
    }
}

/// Starts a session. Fails early when the engine cannot run (Claude Code not
/// installed, no API key); everything else is reported as events.
pub fn start_session(config: SessionConfig) -> Result<Session, AgentError> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let agent_name = match &config.engine {
        EngineConfig::ClaudeCli(c) => Some(c.agent_name_with_marker()),
        EngineConfig::Api(_) => None,
    };
    let shared = Arc::new(Shared {
        session_id: Mutex::new(config.resume.clone()),
        agent_name: agent_name.clone(),
    });
    let emitter = Emitter {
        tx: ev_tx,
        shared: shared.clone(),
    };
    let SessionConfig {
        engine,
        system_prompt,
        model,
        effort,
        resume,
    } = config;
    let worker: Pin<Box<dyn std::future::Future<Output = ()> + Send>> = match engine {
        EngineConfig::ClaudeCli(c) => {
            let w = crate::cli::CliWorker::new(
                c,
                agent_name.unwrap_or_default(),
                system_prompt,
                model,
                effort,
                resume,
                emitter,
            )?;
            Box::pin(w.run(cmd_rx))
        }
        EngineConfig::Api(a) => {
            let w = crate::api::ApiWorker::new(a, system_prompt, model, effort, emitter)?;
            Box::pin(w.run(cmd_rx))
        }
    };
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("pen-agent".into())
        .spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(worker);
                // Dropping the runtime here also kills a leftover child
                // (`kill_on_drop`).
                drop(rt);
            }
            let _ = done_tx.send(());
        })
        .map_err(|e| AgentError::Other(format!("could not start the agent thread: {e}")))?;
    Ok(Session {
        cmd: cmd_tx,
        events: Some(EventReceiver(ev_rx)),
        shared,
        thread: Some(thread),
        finished: Some(done_rx),
    })
}
