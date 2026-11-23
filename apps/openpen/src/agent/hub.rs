//! Foreground side of the MCP integration: owns the socket server, the list
//! of connected agents and the script engine, and runs each tool call on the
//! GPUI main thread against the document it targets.
//!
//! Calls are handled one at a time, in arrival order, by a task spawned on
//! the foreground executor that drains the [`GuiBackend`] channel.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui_kit::*;
use pen_mcp::socket::{BackgroundSocketServer, DEFAULT_SOCKET_NAME};
use pen_mcp::{CallCtx, McpOptions, ToolError, ToolOutput, ToolResult};
use pen_script::{Edit, ExecuteRequest, ScriptEngine};

use super::backend::{AgentRequest, GuiBackend, RequestRx};
use super::route::{Route, route};
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
    let opts = McpOptions::with_default_skills();
    match &opts.skills_dir {
        Some(d) => log(&format!("MCP: skills from {}", d.display())),
        None => log("MCP: skills folder not found (set OPENPEN_SKILLS_DIR)"),
    }
    let name = socket_name();
    let (server, socket_name, server_error) =
        match BackgroundSocketServer::start(&name, Arc::new(backend), opts) {
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
    let running = server.is_some();
    cx.set_global(AgentHub {
        agents: Vec::new(),
        colors_used: 0,
        server,
        socket_name,
        server_error,
    });
    cx.on_app_quit(|cx| {
        if let Some(s) = cx.global_mut::<AgentHub>().server.take() {
            s.shutdown();
        }
        std::future::ready(())
    })
    .detach();
    if running {
        cx.spawn(async move |cx| dispatch(rx, window, ws, cx).await)
            .detach();
    }
}

async fn dispatch(
    mut rx: RequestRx,
    window: AnyWindowHandle,
    ws: WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) {
    let mut engine = ScriptEngine::new();
    while let Some(req) = rx.next().await {
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
                engine.forget_agent(&ctx.client_id);
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
                let t0 = Instant::now();
                let r = execute(&mut engine, &ctx, args, window, &ws, cx).await;
                log(&format!(
                    "MCP: execute from '{}' in {:.1?}: {}",
                    ctx.agent_name,
                    t0.elapsed(),
                    match &r {
                        Ok(o) if !o.is_error => "ok",
                        _ => "failed",
                    }
                ));
                let _ = reply.send(r);
            }
            AgentRequest::Save { ctx, uri, reply } => {
                let r = save(&ctx, uri.as_deref(), window, &ws, cx).await;
                let _ = reply.send(r);
            }
        }
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

async fn execute(
    engine: &mut ScriptEngine,
    ctx: &CallCtx,
    args: pen_mcp::ExecuteArgs,
    window: AnyWindowHandle,
    ws: &WeakEntity<Workspace>,
    cx: &mut AsyncApp,
) -> ToolResult {
    let agent = cx.update(|cx| cx.global_mut::<AgentHub>().agent(ctx));
    let state = target(args.uri.as_deref(), window, ws, cx).await?;
    let view = view_of(&state, ws, cx).unwrap_or(pen_render::Rect::new(0.0, 0.0, 1.0, 1.0));
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
    let resp = state.update(cx, |s, cx| {
        s.run_agent_script(engine, &agent, req, view, cx)
    });
    Ok(ToolOutput {
        text: resp.text,
        images: resp.images,
        is_error: !resp.ok,
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
