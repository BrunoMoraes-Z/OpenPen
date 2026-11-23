//! The rmcp `ServerHandler`: one instance per MCP session.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeRequestParams, InitializeResult, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;

use crate::McpOptions;
use crate::backend::{CallCtx, TOOL_USE_ID_META, ToolBackend, ToolError, ToolOutput, ToolResult};
use crate::skills::SkillStore;
use crate::tools::{
    self, ExecuteArgs, GetAppStateArgs, ImportWebsiteArgs, ReadSkillArgs, SaveArgs, StartTaskArgs,
};

/// Text sent as the MCP `instructions` of the server.
pub const INSTRUCTIONS: &str = "\
OpenPen is a native design editor for .pen files. These tools read and change the \
documents open in OpenPen (or loaded by the headless server).

- Begin a design task with `start_task` (or `read_skill`): load `SKILL.md`, `execute.md` \
and `pen-schema.md` to learn the scripting API and the document format.
- Call `get_app_state` to find the focused document, the selection and the top-level frames.
- Change .pen documents only through `execute`. Each call is one atomic transaction and \
one undo step, and the user watches it happen on the canvas. Editing the .pen file on \
disk instead bypasses undo and the live view, and the editor may overwrite it.
- `uri` picks the document (file path or file:// URI); leave it out to use the focused one.
- Call `save` when the changes should be written to disk.
- `import_website` turns a live web page into a new frame of editable layers (slow: up \
to a minute or more).";

const STYLE_UNAVAILABLE: &str = "Style presets are not available in OpenPen yet. Pick the \
typography, colors and spacing yourself, reusing the document's variables when it has them.";

/// State shared by every session of one server.
pub(crate) struct Shared {
    pub backend: Arc<dyn ToolBackend>,
    pub opts: McpOptions,
    pub skills: Option<SkillStore>,
    pub tools: Vec<Tool>,
}

impl Shared {
    pub fn new(backend: Arc<dyn ToolBackend>, opts: McpOptions) -> Arc<Self> {
        let skills = opts.skills_dir.clone().map(SkillStore::new);
        let tools = tools::tool_list(opts.enable_save);
        Arc::new(Self {
            backend,
            opts,
            skills,
            tools,
        })
    }
}

/// Per-connection identity. The agent is announced to the backend lazily, on the
/// first request, because its name comes from `initialize`.
pub(crate) struct Session {
    client_id: String,
    /// Agent name announced by the shim (`--agent`) before the MCP traffic.
    hello_agent: Option<String>,
    ctx: Mutex<Option<CallCtx>>,
}

impl Session {
    pub fn new(client_id: String, hello_agent: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            client_id,
            hello_agent,
            ctx: Mutex::new(None),
        })
    }

    fn ctx(&self, shared: &Shared, client: Option<&Implementation>) -> CallCtx {
        let created = {
            let mut slot = self.ctx.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ctx) = slot.as_ref() {
                return ctx.clone();
            }
            let agent_name = self
                .hello_agent
                .clone()
                .or_else(|| shared.opts.agent_name.clone())
                .or_else(|| client.map(|c| c.name.clone()))
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| "agent".to_string());
            let ctx = CallCtx {
                client_id: self.client_id.clone(),
                agent_name,
                tool_use_id: None,
            };
            *slot = Some(ctx.clone());
            ctx
        };
        shared.backend.agent_connected(&created);
        created
    }

    /// Reports the disconnect if the session was ever announced.
    pub fn finish(&self, shared: &Shared) {
        let ctx = self.ctx.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(ctx) = ctx {
            shared.backend.agent_disconnected(&ctx);
        }
    }
}

#[derive(Clone)]
pub(crate) struct OpenPenServer {
    shared: Arc<Shared>,
    session: Arc<Session>,
}

impl OpenPenServer {
    pub fn new(shared: Arc<Shared>, session: Arc<Session>) -> Self {
        Self { shared, session }
    }

    async fn dispatch(
        &self,
        ctx: CallCtx,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ToolResult, McpError> {
        let backend = &self.shared.backend;
        Ok(match name {
            tools::GET_APP_STATE => {
                let a: GetAppStateArgs = parse(args)?;
                backend.get_app_state(ctx, a.uri).await
            }
            tools::EXECUTE => {
                let a: ExecuteArgs = parse(args)?;
                a.validate()
                    .map_err(|m| McpError::invalid_params(m, None))?;
                backend.execute(ctx, a).await
            }
            tools::SAVE if self.shared.opts.enable_save => {
                let a: SaveArgs = parse(args)?;
                backend.save(ctx, a.uri).await
            }
            tools::READ_SKILL => {
                let a: ReadSkillArgs = parse(args)?;
                self.skills()
                    .and_then(|s| s.read(a.path.as_deref()).map(ToolOutput::text))
            }
            tools::START_TASK => {
                let a: StartTaskArgs = parse(args)?;
                self.skills().and_then(|s| {
                    s.bundle(&a.skills, a.title.as_deref())
                        .map(ToolOutput::text)
                })
            }
            tools::GET_STYLE => Ok(ToolOutput::text(STYLE_UNAVAILABLE)),
            tools::IMPORT_WEBSITE => {
                let a: ImportWebsiteArgs = parse(args)?;
                let url = a
                    .validate()
                    .map_err(|m| McpError::invalid_params(m, None))?;
                backend.import_website(ctx, a, url).await
            }
            other => match backend.call_extension(ctx, other, args) {
                Some(f) => f.await,
                None => {
                    return Err(McpError::invalid_params(
                        format!("unknown tool `{other}`"),
                        None,
                    ));
                }
            },
        })
    }

    fn skills(&self) -> Result<&SkillStore, ToolError> {
        self.shared.skills.as_ref().ok_or_else(|| {
            ToolError::Failed(
                "OpenPen skills are not installed (skills folder not found). \
                 Set OPENPEN_SKILLS_DIR to the `skills/openpen` folder."
                    .into(),
            )
        })
    }
}

/// The Claude Code tool use id of a `tools/call`: rmcp moves the params'
/// `_meta` into the request context; the params' copy is checked as well.
fn tool_use_id(
    context: &serde_json::Map<String, serde_json::Value>,
    params: Option<&rmcp::model::MetaObject>,
) -> Option<String> {
    std::iter::once(context)
        .chain(params.map(|m| &m.0))
        .find_map(|m| m.get(TOOL_USE_ID_META)?.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse<T: DeserializeOwned>(
    args: serde_json::Map<String, serde_json::Value>,
) -> Result<T, McpError> {
    serde_json::from_value(serde_json::Value::Object(args))
        .map_err(|e| McpError::invalid_params(format!("invalid arguments: {e}"), None))
}

/// Maps a backend result onto MCP (see the table on [`ToolError`]).
pub(crate) fn to_call_result(result: ToolResult) -> Result<CallToolResult, McpError> {
    match result {
        Ok(out) => {
            let mut content = Vec::with_capacity(1 + out.images.len());
            if !out.text.is_empty() || out.images.is_empty() {
                content.push(ContentBlock::text(out.text));
            }
            for png in out.images {
                let data = base64::engine::general_purpose::STANDARD.encode(png);
                content.push(ContentBlock::image(data, "image/png"));
            }
            Ok(if out.is_error {
                CallToolResult::error(content)
            } else {
                CallToolResult::success(content)
            })
        }
        Err(ToolError::NoDocument(m) | ToolError::Failed(m)) => {
            Ok(CallToolResult::error(vec![ContentBlock::text(m)]))
        }
        Err(ToolError::InvalidParams(m)) => Err(McpError::invalid_params(m, None)),
        Err(ToolError::Unavailable(m)) => Err(McpError::internal_error(m, None)),
    }
}

impl ServerHandler for OpenPenServer {
    fn get_info(&self) -> ServerConfig {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("openpen", env!("CARGO_PKG_VERSION")).with_title("OpenPen"),
            )
            .with_instructions(INSTRUCTIONS)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        context.peer.set_peer_info(request.clone());
        let result = self.negotiate_initialize(&request)?;
        self.session.ctx(&self.shared, Some(&request.client_info));
        Ok(result)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.session
            .ctx(&self.shared, context.client_info().as_ref());
        Ok(ListToolsResult::with_all_items(self.shared.tools.clone()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.shared.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let mut ctx = self
            .session
            .ctx(&self.shared, context.client_info().as_ref());
        ctx.tool_use_id = tool_use_id(&context.meta.0.0, request.meta.as_ref().map(|m| &m.0));
        let args = request.arguments.unwrap_or_default();
        let result = self.dispatch(ctx, &request.name, args).await?;
        to_call_result(result).map(Into::into)
    }
}
