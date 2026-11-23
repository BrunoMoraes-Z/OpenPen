//! Foreground side of the MCP integration: owns the socket server, the list
//! of connected agents and the script engine, and runs each tool call on the
//! GPUI main thread against the document it targets.
//!
//! Calls are handled one at a time, in arrival order, by a task spawned on
//! the foreground executor that drains the [`GuiBackend`] channel. The same
//! task drives the streaming `execute` coordinator (`agent::stream`): it
//! wakes up for due partial evaluations and timeouts, and parks an `execute`
//! whose document is held by another agent's stream until it ends.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::oneshot;
use futures::future::{Either, select};
use gpui_kit::*;
use pen_mcp::socket::{BackgroundSocketServer, DEFAULT_SOCKET_NAME};
use pen_mcp::{CallCtx, McpOptions, ToolError, ToolOutput, ToolResult};
use pen_script::{Edit, ExecuteRequest, ExecuteResponse, PartialStatus, ScriptEngine};

use super::backend::{AgentRequest, DEBUG_STREAM_ENV, GuiBackend, RequestRx};
use super::route::{Route, route};
use super::stream::{Outcome, StreamCoordinator, StreamHost};
use super::{AgentInfo, color_for};
use crate::editor_state::EditorState;
use crate::scene_document::key_of;
use crate::workspace::{Workspace, canonical, log};

/// Environment variable that overrides the socket name (default `desktop`),
/// e.g. to run a second instance: `openpen-cli mcp --socket <name>`.
pub const SOCKET_ENV: &str = "OPENPEN_MCP_SOCKET";

/// How long a call waits for a document to load or a drag to end.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);

/// App-wide agent state (GPUI global).
pub struct AgentHub {
    /// Connected agents, in connection order.
    pub agents: Vec<AgentInfo>,
    colors_used: usize,
    server: Option<BackgroundSocketServer>,
    /// Socket name the server listens on (`None` when it is not running).
    pub socket_name: Option<String>,
    /// Why the server is not running.
    pub server_error: Option<String>,
    /// The channel to the dispatcher, for in-process agents (the chat):
    /// tool calls through `ToolBackend` and the streaming updates.
    pub backend: GuiBackend,
}

impl Global for AgentHub {}

impl AgentHub {
    fn agent(&mut self, ctx: &CallCtx) -> AgentInfo {
        if let Some(a) = self.agents.iter().find(|a| a.client_id == ctx.client_id) {
            return a.clone();
        }
        let a = AgentInfo {
            client_id: ctx.client_id.clone(),
            name: ctx.agent_name.clone(),
            color: color_for(self.colors_used),
        };
        self.colors_used += 1;
        self.agents.push(a.clone());
        a
    }

    pub fn names(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.name.clone()).collect()
    }

    /// Info of an agent by id, registering it (named after the id) if it
    /// never connected (an in-process agent that only streamed).
    fn agent_by_id(&mut self, id: &str) -> AgentInfo {
        self.agent(&CallCtx {
            client_id: id.to_string(),
            agent_name: id.to_string(),
            ..Default::default()
        })
    }
}

/// Tells the dispatcher that a document tab is closing (its streams are
/// rolled back).
pub fn doc_closing(id: EntityId, cx: &App) {
    if let Some(h) = cx.try_global::<AgentHub>() {
        h.backend.doc_closed(id);
    }
}

/// Socket name from [`SOCKET_ENV`] or the default.
pub fn socket_name() -> String {
    std::env::var(SOCKET_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SOCKET_NAME.to_string())
}

/// Starts the MCP socket server and the foreground dispatcher. When the
/// socket is taken (another OpenPen instance) the app runs without it.
pub fn start(window: AnyWindowHandle, ws: WeakEntity<Workspace>, cx: &mut App) {
    let (backend, rx) = GuiBackend::new();
    let debug_stream = std::env::var(DEBUG_STREAM_ENV).is_ok_and(|v| v.trim() == "1");
    if debug_stream {
        log("MCP: debug streaming tool enabled (OPENPEN_DEBUG_STREAM=1)");
    }
    let backend = backend.with_debug_stream(debug_stream);
    let opts = McpOptions::with_default_skills();
    match &opts.skills_dir {
        Some(d) => log(&format!("MCP: skills from {}", d.display())),
        None => log("MCP: skills folder not found (set OPENPEN_SKILLS_DIR)"),
    }
    let name = socket_name();
    let (server, socket_name, server_error) =
        match BackgroundSocketServer::start(&name, Arc::new(backend.clone()), opts) {
            Ok(s) => {
                log(&format!("MCP: listening on {}", s.path().display()));
                (Some(s), Some(name), None)
            }
            Err(e) => {
                log(&format!(
                    "MCP: server not started on '{name}' ({e}); is another OpenPen running?"
                ));
                (None, None, Some(e.to_string()))
            }
        };
    cx.set_global(AgentHub {
        agents: Vec::new(),
        colors_used: 0,
        server,
        socket_name,
        server_error,
        backend,
    });
    cx.on_app_quit(|cx| {
        if let Some(s) = cx.global_mut::<AgentHub>().server.take() {
            s.shutdown();
        }
        std::future::ready(())
    })
    .detach();
    // Also without a socket: in-process agents use the same channel.
    cx.spawn(async move |cx| dispatch(rx, window, ws, cx).await)
        .detach();
}

type Doc = WeakEntity<EditorState>;

/// An `execute` waiting for another agent's stream to release its document.
struct Call {
    ctx: CallCtx,
    doc: Doc,
    req: ExecuteRequest,
    reply: oneshot::Sender<ToolResult>,
    since: Instant,
}

/// A parked call gives up after this long.
const PARK_TIMEOUT: Duration = Duration::from_secs(120);

/// The [`StreamHost`] of the app: documents are tabs (weak, a closed tab is
/// "gone"); partials and finals refresh the panels and flash like `execute`.
struct AppHost<'a> {
    cx: &'a mut App,
    ws: &'a WeakEntity<Workspace>,
}

impl AppHost<'_> {
    fn agent(&mut self, id: &str) -> AgentInfo {
        self.cx.global_mut::<AgentHub>().agent_by_id(id)
    }

    fn view(&mut self, s: &Entity<EditorState>) -> pen_render::Rect {
        self.ws
            .update(self.cx, |w, cx| w.visible_world(s, cx))
            .ok()
            .flatten()
            .unwrap_or(pen_render::Rect::new(0.0, 0.0, 1.0, 1.0))
    }
}

impl StreamHost for AppHost<'_> {
    type Doc = Doc;

    fn ready(&mut self, doc: &Doc) -> Option<bool> {
        let s = doc.upgrade()?;
        let s = s.read(self.cx);
        Some(!s.loading && s.load_error.is_none() && !s.doc.in_gesture())
    }

    fn partial(
        &mut self,
        doc: &Doc,
        engine: &mut ScriptEngine,
        agent: &str,
        prefix: &str,
    ) -> Option<PartialStatus> {
        let s = doc.upgrade()?;
        let info = self.agent(agent);
        let view = self.view(&s);
        Some(s.update(self.cx, |s, cx| {
            s.run_agent_partial(engine, &info, prefix, view, cx)
        }))
    }

    fn cancel(&mut self, doc: &Doc, engine: &mut ScriptEngine, agent: &str) -> bool {
        let Some(s) = doc.upgrade() else {
            return false;
        };
        s.update(self.cx, |s, cx| s.cancel_agent_stream(engine, agent, cx));
        true
    }

    fn execute(
        &mut self,
        doc: &Doc,
        engine: &mut ScriptEngine,
        req: ExecuteRequest,
        skip_touched: usize,
    ) -> Option<ExecuteResponse> {
        let s = doc.upgrade()?;
        let info = self.agent(&req.agent_id);
        let view = self.view(&s);
        Some(s.update(self.cx, |s, cx| {
            s.run_agent_script(engine, &info, req, view, skip_touched, cx)
        }))
    }
}

/// Runs (or parks) an `execute` through the coordinator; replies unless parked.
fn run_call(
    coord: &mut StreamCoordinator<Doc>,
    call: Call,
    ws: &WeakEntity<Workspace>,
    cx: &mut App,
) -> Option<Call> {
    let Call {
        ctx,
        doc,
        req,
        reply,
        since,
    } = call;
    let t0 = Instant::now();
    let mut host = AppHost { cx, ws };
    let outcome = coord.execute(
        &mut host,
        &ctx.client_id,
        ctx.tool_use_id.as_deref(),
        &doc,
        req,
    );
    let r = match outcome {
        Outcome::Parked(req) => {
            if since.elapsed() > PARK_TIMEOUT {
                Err(ToolError::Failed(
                    "the document stayed busy (another agent kept writing to it); try again".into(),
                ))
            } else {
                return Some(Call {
                    ctx,
                    doc,
                    req,
                    reply,
                    since,
                });
            }
        }
        Outcome::Gone => Err(ToolError::Failed(
            "the document was closed before the script ran".into(),
        )),
        Outcome::Done { resp, streamed } => {
            log(&format!(
                "MCP: execute from '{}' in {:.1?}{}: {}",
                ctx.agent_name,
                t0.elapsed(),
                if streamed { " (finalized stream)" } else { "" },
                if resp.ok { "ok" } else { "failed" }
            ));
            Ok(ToolOutput {
                text: resp.text,
                images: resp.images,
                is_error: !resp.ok,
            })
        }
    };
    let _ = reply.send(r);
    None
}

/// The document a streamed call targets: `uri` routed like the tools'
/// (opening a closed file, which the stream then waits for).
fn stream_doc(
    uri: Option<&str>,
    window: AnyWindowHandle,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> Option<Doc> {
    let r = ws
        .update(cx, |w, cx| {
            route(uri, &w.tab_infos(cx), w.focused_doc(), canonical)
        })
        .ok()?;
    match r {
        Route::Tab(s) => Some(s.downgrade()),
        Route::NoDocument(m) => {
            log(&format!("MCP: stream ignored: {m}"));
            None
        }
        Route::Open(path) => {
            let ws2 = ws.clone();
            let p = path.clone();
            window
                .update(cx, move |_, window, cx| {
                    let _ = ws2.update(cx, |w, cx| w.open_path(p, window, cx));
                })
                .ok()?;
            ws.update(cx, |w, cx| w.doc_for_path(&path, cx))
                .ok()
                .flatten()
                .map(|s| s.downgrade())
        }
    }
}

async fn dispatch(
    mut rx: RequestRx,
    window: AnyWindowHandle,
    ws: WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) {
    let mut coord: StreamCoordinator<Doc> = StreamCoordinator::new(ScriptEngine::new());
    let mut parked: Vec<Call> = Vec::new();
    // (agent, tool use id) -> (uri, document) of the open streams.
    let mut stream_docs: HashMap<(String, String), (Option<String>, Doc)> = HashMap::new();
    loop {
        let wake = coord
            .next_wake(Instant::now())
            .or_else(|| (!parked.is_empty()).then_some(Duration::from_secs(1)));
        let next = match wake {
            None => rx.next().await,
            Some(d) => {
                let timer = cx.background_executor().timer(d);
                match select(rx.next(), std::pin::pin!(timer)).await {
                    Either::Left((r, _)) => r,
                    Either::Right(_) => {
                        pump(&mut coord, &mut parked, &ws, cx);
                        continue;
                    }
                }
            }
        };
        let Some(req) = next else { break };
        match req {
            AgentRequest::Connected(ctx) => {
                log(&format!(
                    "MCP: agent '{}' connected ({})",
                    ctx.agent_name, ctx.client_id
                ));
                cx.update(|cx| {
                    cx.global_mut::<AgentHub>().agent(&ctx);
                });
                let _ = ws.update(cx, |_, cx| cx.notify());
            }
            AgentRequest::Disconnected(ctx) => {
                log(&format!("MCP: agent '{}' disconnected", ctx.agent_name));
                cx.update(|cx| {
                    coord.forget_agent(&mut AppHost { cx, ws: &ws }, &ctx.client_id);
                });
                stream_docs.retain(|(a, _), _| *a != ctx.client_id);
                cx.update(|cx| {
                    cx.global_mut::<AgentHub>()
                        .agents
                        .retain(|a| a.client_id != ctx.client_id);
                });
                let _ = ws.update(cx, |w, cx| {
                    for s in w.doc_states() {
                        s.update(cx, |s, cx| {
                            s.agent_fx.forget(&ctx.client_id);
                            cx.notify();
                        });
                    }
                    cx.notify();
                });
            }
            AgentRequest::AppState { ctx, uri, reply } => {
                let r = app_state(&ctx, uri.as_deref(), window, &ws, cx).await;
                let _ = reply.send(r);
            }
            AgentRequest::Execute { ctx, args, reply } => {
                if let Some(call) = execute(ctx, args, reply, window, &ws, cx).await
                    && let Some(p) = cx.update(|cx| run_call(&mut coord, call, &ws, cx))
                {
                    log(&format!(
                        "MCP: execute from '{}' waits: another agent is streaming into the document",
                        p.ctx.agent_name
                    ));
                    parked.push(p);
                }
            }
            AgentRequest::Save { ctx, uri, reply } => {
                let r = save(&ctx, uri.as_deref(), window, &ws, cx).await;
                let _ = reply.send(r);
            }
            AgentRequest::StreamPartial {
                agent_id,
                tool_use_id,
                uri,
                prefix,
            } => {
                let key = (agent_id, tool_use_id);
                let doc = match stream_docs.get(&key) {
                    Some((u, d)) if *u == uri => Some(d.clone()),
                    _ => stream_doc(uri.as_deref(), window, &ws, cx),
                };
                if let Some(doc) = doc {
                    stream_docs.insert(key.clone(), (uri, doc.clone()));
                    cx.update(|cx| {
                        coord.partial(
                            &mut AppHost { cx, ws: &ws },
                            &key.0,
                            &key.1,
                            doc,
                            prefix,
                            Instant::now(),
                        );
                    });
                }
            }
            AgentRequest::StreamCancel {
                agent_id,
                tool_use_id,
            } => {
                cx.update(|cx| {
                    coord.cancel(
                        &mut AppHost { cx, ws: &ws },
                        &agent_id,
                        tool_use_id.as_deref(),
                    );
                });
            }
            AgentRequest::DocClosed(id) => {
                cx.update(|cx| {
                    coord.doc_closed(&mut AppHost { cx, ws: &ws }, |d| d.entity_id() == id);
                });
            }
        }
        pump(&mut coord, &mut parked, &ws, cx);
        // Forget the routing of streams that ended.
        stream_docs.retain(|(a, t), _| {
            coord
                .streams()
                .iter()
                .any(|s| s.agent == *a && s.tool_use_id == *t)
        });
    }
}

/// Evaluates due partials, times streams out and retries parked calls.
fn pump(
    coord: &mut StreamCoordinator<Doc>,
    parked: &mut Vec<Call>,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) {
    cx.update(|cx| {
        coord.pump(&mut AppHost { cx, ws }, Instant::now());
        if !parked.is_empty() {
            let calls = std::mem::take(parked);
            for call in calls {
                if let Some(p) = run_call(coord, call, ws, cx) {
                    parked.push(p);
                }
            }
        }
    });
    for line in coord.take_log() {
        log(&format!("MCP: {line}"));
    }
}

fn gone() -> ToolError {
    ToolError::Unavailable("the OpenPen window is closed".into())
}

/// Finds (or opens) the document a call targets and waits until it can be
/// edited: loaded and with no drag in progress.
async fn target(
    uri: Option<&str>,
    window: AnyWindowHandle,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> Result<Entity<EditorState>, ToolError> {
    let r = ws
        .update(cx, |w, cx| {
            route(uri, &w.tab_infos(cx), w.focused_doc(), canonical)
        })
        .map_err(|_| gone())?;
    let state = match r {
        Route::Tab(s) => s,
        Route::NoDocument(m) => return Err(ToolError::NoDocument(m)),
        Route::Open(path) => open(path, window, ws, cx).await?,
    };
    let start = Instant::now();
    loop {
        let (loading, busy, error) = state.update(cx, |s, _| {
            (s.loading, s.doc.in_gesture(), s.load_error.clone())
        });
        if let Some(e) = error {
            return Err(ToolError::Failed(e));
        }
        if !loading && !busy {
            return Ok(state);
        }
        if start.elapsed() > READY_TIMEOUT {
            return Err(ToolError::Failed(
                "the document is busy (still loading or being edited by the user); try again"
                    .into(),
            ));
        }
        cx.background_executor().timer(POLL).await;
    }
}

/// Opens `path` in a new tab and returns it once it is loaded.
async fn open(
    path: PathBuf,
    window: AnyWindowHandle,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> Result<Entity<EditorState>, ToolError> {
    let ws2 = ws.clone();
    let p = path.clone();
    window
        .update(cx, move |_, window, cx| {
            let _ = ws2.update(cx, |w, cx| w.open_path(p, window, cx));
        })
        .map_err(|_| gone())?;
    let start = Instant::now();
    loop {
        let found = ws
            .update(cx, |w, cx| w.doc_for_path(&path, cx))
            .map_err(|_| gone())?;
        let Some(state) = found else {
            return Err(ToolError::Failed(format!(
                "could not open {}",
                path.display()
            )));
        };
        if !state.read_with(cx, |s, _| s.loading) {
            return Ok(state);
        }
        if start.elapsed() > READY_TIMEOUT {
            return Err(ToolError::Failed(format!(
                "timed out opening {}",
                path.display()
            )));
        }
        cx.background_executor().timer(POLL).await;
    }
}

/// Visible world rect of a document's canvas.
fn view_of(
    state: &Entity<EditorState>,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> Option<pen_render::Rect> {
    ws.update(cx, |w, cx| w.visible_world(state, cx))
        .ok()
        .flatten()
}

async fn app_state(
    ctx: &CallCtx,
    uri: Option<&str>,
    window: AnyWindowHandle,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> ToolResult {
    let agents = cx.update(|cx| {
        let hub = cx.global_mut::<AgentHub>();
        hub.agent(ctx);
        hub.names()
    });
    let state = match target(uri, window, ws, cx).await {
        Ok(s) => s,
        Err(ToolError::NoDocument(m)) => {
            return Ok(ToolOutput::text(format!("# OpenPen editor state\n\n{m}\n")));
        }
        Err(e) => return Err(e),
    };
    let viewport = view_of(&state, ws, cx);
    let others = ws
        .update(cx, |w, cx| w.open_documents(cx))
        .map_err(|_| gone())?;
    let mut text = state.update(cx, |s, _| {
        let uri = s.path.as_ref().map(|p| p.display().to_string());
        let selection: Vec<_> = s.selection.iter().map(|&id| key_of(id)).collect();
        s.doc.describe(&pen_script::AppStateInfo {
            uri: uri.as_deref(),
            selection: &selection,
            viewport,
            connected_agents: &agents,
        })
    });
    if others.len() > 1 {
        text.push_str("\n## Open documents\n\n");
        for (name, path, focused) in others {
            let p = path
                .map(|p| format!("`{}`", p.display()))
                .unwrap_or_else(|| "(unsaved, no uri)".into());
            let f = if focused { " (focused)" } else { "" };
            text.push_str(&format!("- {name}: {p}{f}\n"));
        }
    }
    Ok(ToolOutput::text(text))
}

/// Resolves the document of an `execute` and builds the call; replies with
/// the error (and returns `None`) when there is no document to run on.
async fn execute(
    ctx: CallCtx,
    args: pen_mcp::ExecuteArgs,
    reply: oneshot::Sender<ToolResult>,
    window: AnyWindowHandle,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> Option<Call> {
    cx.update(|cx| cx.global_mut::<AgentHub>().agent(&ctx));
    let since = Instant::now();
    let state = match target(args.uri.as_deref(), window, ws, cx).await {
        Ok(s) => s,
        Err(e) => {
            let _ = reply.send(Err(e));
            return None;
        }
    };
    let req = ExecuteRequest {
        agent_id: ctx.client_id.clone(),
        description: args.description,
        input: args.input,
        edit_id: args.edit_id,
        edits: args
            .edits
            .into_iter()
            .map(|e| Edit {
                find: e.find,
                replace: e.replace,
                all: e.all,
            })
            .collect(),
    };
    Some(Call {
        ctx,
        doc: state.downgrade(),
        req,
        reply,
        since,
    })
}

async fn save(
    ctx: &CallCtx,
    uri: Option<&str>,
    window: AnyWindowHandle,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> ToolResult {
    let state = target(uri, window, ws, cx).await?;
    if let Some(agent) = state.read_with(cx, |s, _| s.doc.stream_lock().map(str::to_string)) {
        return Err(ToolError::Failed(format!(
            "an agent ({agent}) is still streaming an execute into this document; save after that call finishes"
        )));
    }
    let task = ws
        .update(cx, |w, cx| w.save_for_agent(&state, cx))
        .map_err(|_| gone())?;
    match task.await {
        Ok(path) => {
            log(&format!(
                "MCP: '{}' saved {}",
                ctx.agent_name,
                path.display()
            ));
            Ok(ToolOutput::text(format!("Saved {}", path.display())))
        }
        Err(e) => Err(ToolError::Failed(e)),
    }
}
