//! Engine 1: the installed Claude Code CLI.
//!
//! Runs `claude -p --input-format stream-json --output-format stream-json
//! --verbose --include-partial-messages` with the OpenPen MCP server as its
//! only MCP server, writes user messages to its stdin and maps the
//! stream-json lines it prints into [`AgentEvent`]s ([`CliMapper`]).
//!
//! The process stays alive between turns (its stdin stays open). `stop()`
//! kills it; the next message starts a new one with `--resume <session_id>`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use crate::session::{Command, Emitter};
use crate::tools::normalize_tool_name;
use crate::{AgentError, AgentEvent, Attachment, Effort, Usage, UserMessage};

/// Built-in tools the chat may use besides the OpenPen MCP tools.
pub const WEB_TOOLS: [&str; 2] = ["WebSearch", "WebFetch"];

/// Claude Code's per-call MCP tool timeout (ms), set for the chat's process
/// so a slow `import_website` (browser + downloads) is not cut short.
const MCP_TOOL_TIMEOUT_ENV: &str = "MCP_TOOL_TIMEOUT";
const MCP_TOOL_TIMEOUT_MS: &str = "600000";
/// Built-in tools that are always denied (belt and braces: `--tools` already
/// removes them).
pub const DENIED_TOOLS: [&str; 6] = [
    "Bash",
    "PowerShell",
    "Edit",
    "Write",
    "NotebookEdit",
    "Agent",
];

/// Settings of the Claude Code engine.
#[derive(Debug, Clone)]
pub struct CliConfig {
    /// `claude` executable. `None`: look it up on PATH (and in the usual
    /// install folders).
    pub claude_path: Option<PathBuf>,
    /// `openpen-cli` executable (the MCP shim).
    pub mcp_command: PathBuf,
    /// `--socket` of the shim (`None`: the default socket).
    pub mcp_socket: Option<String>,
    /// More `openpen-cli mcp` arguments (e.g. `--headless <file.pen>` to
    /// serve one document without the app).
    pub mcp_extra_args: Vec<String>,
    /// Display name of the chat agent; the session marker is appended
    /// (`OpenPen Chat#1a2b3c4d`).
    pub agent_name: String,
    /// Working folder of the process. Claude Code stores sessions per
    /// folder, so resuming needs the same one. Default: a fixed
    /// `openpen-chat` folder in the temp directory (no project `CLAUDE.md`).
    pub cwd: Option<PathBuf>,
    /// Allow WebSearch and WebFetch.
    pub allow_web: bool,
    /// More arguments, appended last.
    pub extra_args: Vec<String>,
    /// Extra environment variables for the process.
    pub env: Vec<(String, String)>,
    marker: String,
}

impl CliConfig {
    pub fn new(mcp_command: impl Into<PathBuf>) -> Self {
        Self {
            claude_path: None,
            mcp_command: mcp_command.into(),
            mcp_socket: None,
            mcp_extra_args: Vec::new(),
            agent_name: "OpenPen Chat".into(),
            cwd: None,
            allow_web: true,
            extra_args: Vec::new(),
            env: Vec::new(),
            marker: crate::random_hex(4),
        }
    }

    /// The marker that tells this session's MCP calls apart.
    pub fn marker(&self) -> &str {
        &self.marker
    }

    pub(crate) fn agent_name_with_marker(&self) -> String {
        format!("{}#{}", self.agent_name, self.marker)
    }

    fn working_dir(&self) -> PathBuf {
        self.cwd
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("openpen-chat"))
    }
}

/// Finds the `claude` executable: PATH (with `PATHEXT` on Windows, so
/// `claude.exe` or a `claude.cmd` shim), then the usual install folders.
pub fn find_claude() -> Option<PathBuf> {
    if let Ok(p) = which::which("claude") {
        return Some(p);
    }
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let mut candidates = Vec::new();
    if let Some(h) = &home {
        if cfg!(windows) {
            candidates.push(h.join(".local/bin/claude.exe"));
            candidates.push(h.join(".claude/local/claude.exe"));
        } else {
            candidates.push(h.join(".local/bin/claude"));
            candidates.push(h.join(".claude/local/claude"));
        }
    }
    if cfg!(windows)
        && let Some(appdata) = std::env::var_os("APPDATA")
    {
        candidates.push(PathBuf::from(appdata).join("npm/claude.cmd"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Command-line arguments of one `claude` process.
pub(crate) struct ArgsSpec<'a> {
    pub mcp_config: &'a Path,
    pub system_prompt_file: &'a Path,
    pub model: Option<&'a str>,
    pub effort: Effort,
    pub resume: Option<&'a str>,
    pub allow_web: bool,
    pub extra: &'a [String],
}

pub(crate) fn build_args(s: &ArgsSpec<'_>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--strict-mcp-config",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push("--mcp-config".into());
    a.push(s.mcp_config.display().to_string());
    a.push("--append-system-prompt-file".into());
    a.push(s.system_prompt_file.display().to_string());
    // Built-in tools: only the web ones (or none).
    a.push("--tools".into());
    a.push(if s.allow_web {
        WEB_TOOLS.join(",")
    } else {
        String::new()
    });
    let mut allowed = vec!["mcp__openpen__*".to_string()];
    if s.allow_web {
        allowed.extend(WEB_TOOLS.iter().map(|t| t.to_string()));
    }
    a.push("--allowedTools".into());
    a.push(allowed.join(","));
    a.push("--disallowedTools".into());
    a.push(DENIED_TOOLS.join(","));
    // Anything not allowed above is denied instead of prompting.
    a.push("--permission-mode".into());
    a.push("dontAsk".into());
    if let Some(m) = s.model {
        a.push("--model".into());
        a.push(m.to_string());
    }
    a.push("--effort".into());
    a.push(s.effort.as_str().into());
    if let Some(r) = s.resume {
        a.push("--resume".into());
        a.push(r.to_string());
    }
    a.extend(s.extra.iter().cloned());
    a
}

/// The `--mcp-config` JSON: our server only, through the stdio shim.
pub(crate) fn mcp_config_json(cfg: &CliConfig, agent_name: &str) -> Value {
    let mut args = pen_mcp::config::shim_args(cfg.mcp_socket.as_deref(), Some(agent_name));
    args.extend(cfg.mcp_extra_args.iter().cloned());
    json!({
        "mcpServers": {
            "openpen": {
                "type": "stdio",
                "command": cfg.mcp_command.display().to_string(),
                "args": args,
            }
        }
    })
}

/// The stream-json input line for one user message.
pub(crate) fn user_line(msg: &UserMessage) -> String {
    let mut content = Vec::new();
    if let Some(ctx) = msg.context.as_deref().filter(|c| !c.trim().is_empty()) {
        content.push(json!({"type": "text", "text": ctx}));
    }
    for a in &msg.attachments {
        content.push(attachment_block(a));
    }
    if !msg.text.is_empty() || content.is_empty() {
        content.push(json!({"type": "text", "text": msg.text}));
    }
    let v = json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
    });
    let mut s = v.to_string();
    s.push('\n');
    s
}

pub(crate) fn attachment_block(a: &Attachment) -> Value {
    match a {
        Attachment::Image { media_type, data } => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": base64::engine::general_purpose::STANDARD.encode(data),
            }
        }),
        Attachment::Text { name, text } => json!({
            "type": "text",
            "text": format!("<attachment name=\"{}\">\n{}\n</attachment>", name.replace('"', "'"), text),
        }),
    }
}

/// Text of the result that means "not logged in".
fn looks_like_auth_error(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("/login")
        || l.contains("not logged in")
        || l.contains("invalid api key")
        || l.contains("authentication_error")
        || l.contains("oauth token has expired")
}

#[derive(Debug)]
enum Block {
    Text,
    Thinking,
    Tool { id: String, json: String },
    Other,
}

/// Maps Claude Code stream-json values to [`AgentEvent`]s. Pure: one value
/// in, events out.
#[derive(Debug, Default)]
pub struct CliMapper {
    blocks: HashMap<u64, Block>,
    /// Messages that came with `stream_event`s (their `assistant` copy is
    /// not re-emitted).
    streamed: HashSet<String>,
    tool_ended: HashSet<String>,
    session_id: Option<String>,
}

impl CliMapper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Maps one output line. Lines that are not JSON are ignored.
    pub fn map_line(&mut self, line: &str) -> Vec<AgentEvent> {
        match serde_json::from_str::<Value>(line) {
            Ok(v) => self.map_value(&v),
            Err(_) => Vec::new(),
        }
    }

    fn note_session(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        if let Some(id) = v.get("session_id").and_then(Value::as_str)
            && !id.is_empty()
            && self.session_id.as_deref() != Some(id)
        {
            self.session_id = Some(id.to_string());
            out.push(AgentEvent::SessionId(id.to_string()));
        }
    }

    pub fn map_value(&mut self, v: &Value) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        // Subagent traffic (we deny `Agent`, but stay safe).
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return out;
        }
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" => {
                if v.get("subtype").and_then(Value::as_str) == Some("init") {
                    self.note_session(v, &mut out);
                }
            }
            "stream_event" => {
                if let Some(e) = v.get("event") {
                    self.stream_event(e, &mut out);
                }
            }
            "assistant" => self.assistant(v, &mut out),
            "user" => self.user(v, &mut out),
            "result" => self.result(v, &mut out),
            _ => {}
        }
        out
    }

    fn stream_event(&mut self, e: &Value, out: &mut Vec<AgentEvent>) {
        let index = e.get("index").and_then(Value::as_u64).unwrap_or(0);
        match e.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                self.blocks.clear();
                if let Some(id) = e.pointer("/message/id").and_then(Value::as_str) {
                    self.streamed.insert(id.to_string());
                }
            }
            "content_block_start" => {
                let cb = &e["content_block"];
                let block = match cb.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        if let Some(t) = cb
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                        {
                            out.push(AgentEvent::TextDelta(t.to_string()));
                        }
                        Block::Text
                    }
                    "thinking" => Block::Thinking,
                    "tool_use" => {
                        let id = cb
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = cb.get("name").and_then(Value::as_str).unwrap_or("");
                        out.push(AgentEvent::ToolUseStart {
                            id: id.clone(),
                            name: normalize_tool_name(name).to_string(),
                        });
                        Block::Tool {
                            id,
                            json: String::new(),
                        }
                    }
                    _ => Block::Other,
                };
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let d = &e["delta"];
                let block = self.blocks.get_mut(&index);
                match (d.get("type").and_then(Value::as_str).unwrap_or(""), block) {
                    ("text_delta", _) => {
                        if let Some(t) = d
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                        {
                            out.push(AgentEvent::TextDelta(t.to_string()));
                        }
                    }
                    ("thinking_delta", _) => {
                        if let Some(t) = d
                            .get("thinking")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                        {
                            out.push(AgentEvent::ThinkingDelta(t.to_string()));
                        }
                    }
                    ("input_json_delta", Some(Block::Tool { id, json })) => {
                        let p = d.get("partial_json").and_then(Value::as_str).unwrap_or("");
                        if !p.is_empty() {
                            json.push_str(p);
                            out.push(AgentEvent::ToolInputDelta {
                                id: id.clone(),
                                partial_json: json.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(Block::Tool { id, json }) = self.blocks.remove(&index)
                    && self.tool_ended.insert(id.clone())
                {
                    let input = if json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&json).unwrap_or(Value::String(json))
                    };
                    out.push(AgentEvent::ToolUseEnd { id, input });
                }
            }
            _ => {}
        }
    }

    fn assistant(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        let msg = &v["message"];
        let streamed = msg
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| self.streamed.contains(id));
        let Some(content) = msg.get("content").and_then(Value::as_array) else {
            return;
        };
        for b in content {
            match b.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" if !streamed => {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        out.push(AgentEvent::TextDelta(t.to_string()));
                    }
                }
                "thinking" if !streamed => {
                    if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                        out.push(AgentEvent::ThinkingDelta(t.to_string()));
                    }
                }
                "tool_use" => {
                    let id = b
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if !streamed {
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                        out.push(AgentEvent::ToolUseStart {
                            id: id.clone(),
                            name: normalize_tool_name(name).to_string(),
                        });
                    }
                    // The complete input arrives here, often before the
                    // block's `content_block_stop`.
                    if self.tool_ended.insert(id.clone()) {
                        out.push(AgentEvent::ToolUseEnd {
                            id,
                            input: b.get("input").cloned().unwrap_or_else(|| json!({})),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn user(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else {
            return;
        };
        for b in content {
            if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let id = b
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let ok = !b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            let (text, images) = tool_result_content(b.get("content"));
            out.push(AgentEvent::ToolResult {
                id,
                ok,
                text,
                images,
            });
        }
    }

    fn result(&mut self, v: &Value, out: &mut Vec<AgentEvent>) {
        self.note_session(v, out);
        let u = &v["usage"];
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        out.push(AgentEvent::Usage(Usage {
            input_tokens: n("input_tokens"),
            output_tokens: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
        }));
        let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("");
        let mut stop = v
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or(if subtype == "success" {
                "end_turn"
            } else {
                subtype
            })
            .to_string();
        if is_error || subtype.starts_with("error") {
            let text = v
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| v.get("errors").map(|e| e.to_string()))
                .unwrap_or_else(|| format!("Claude Code failed ({subtype})"));
            if looks_like_auth_error(&text) {
                out.push(AgentEvent::Error(AgentError::ClaudeNotLoggedIn.to_string()));
            } else {
                out.push(AgentEvent::Error(text));
            }
            stop = "error".into();
        }
        out.push(AgentEvent::TurnEnd { stop_reason: stop });
    }
}

/// Text and image count of a tool result's `content` (string or blocks).
pub(crate) fn tool_result_content(c: Option<&Value>) -> (String, usize) {
    match c {
        Some(Value::String(s)) => (s.clone(), 0),
        Some(Value::Array(blocks)) => {
            let mut text = String::new();
            let mut images = 0;
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(b.get("text").and_then(Value::as_str).unwrap_or(""));
                    }
                    Some("image") => images += 1,
                    _ => {}
                }
            }
            (text, images)
        }
        _ => (String::new(), 0),
    }
}

/// Reply to a `control_request` from the CLI. We never ask the CLI to route
/// permission prompts to us, so anything that shows up is denied.
fn control_reply(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("control_request") {
        return None;
    }
    let id = v.get("request_id").cloned().unwrap_or(Value::Null);
    let sub = v
        .pointer("/request/subtype")
        .and_then(Value::as_str)
        .unwrap_or("");
    let resp = if sub == "can_use_tool" {
        json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": id,
            "response": {"behavior": "deny", "message": "This tool is not available in the OpenPen chat."}
        }})
    } else {
        json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": id, "error": format!("unsupported request `{sub}`")
        }})
    };
    Some(format!("{resp}\n"))
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
}

/// Temp files of one session, removed on drop.
struct TempFiles(Vec<PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub(crate) struct CliWorker {
    cfg: CliConfig,
    claude: PathBuf,
    model: Option<String>,
    effort: Effort,
    emitter: Emitter,
    mapper: CliMapper,
    mcp_config: PathBuf,
    prompt_file: PathBuf,
    _temp: TempFiles,
    proc: Option<Proc>,
    turn_active: bool,
    /// `None` until the first process reported its session ID.
    session_id: Option<String>,
}

impl CliWorker {
    pub fn new(
        cfg: CliConfig,
        agent_name: String,
        system_prompt: String,
        model: Option<String>,
        effort: Effort,
        resume: Option<String>,
        emitter: Emitter,
    ) -> Result<Self, AgentError> {
        let claude = match &cfg.claude_path {
            Some(p) if p.is_file() => p.clone(),
            Some(_) => return Err(AgentError::ClaudeNotFound),
            None => find_claude().ok_or(AgentError::ClaudeNotFound)?,
        };
        let dir = std::env::temp_dir().join("openpen-agent");
        std::fs::create_dir_all(&dir)
            .map_err(|e| AgentError::Other(format!("could not create {}: {e}", dir.display())))?;
        let mcp_config = dir.join(format!("chat-{}-mcp.json", cfg.marker));
        let prompt_file = dir.join(format!("chat-{}-prompt.md", cfg.marker));
        let write = |p: &Path, s: &str| {
            std::fs::write(p, s)
                .map_err(|e| AgentError::Other(format!("could not write {}: {e}", p.display())))
        };
        write(&mcp_config, &mcp_config_json(&cfg, &agent_name).to_string())?;
        write(&prompt_file, &system_prompt)?;
        let temp = TempFiles(vec![mcp_config.clone(), prompt_file.clone()]);
        Ok(Self {
            cfg,
            claude,
            model,
            effort,
            emitter,
            mapper: CliMapper::new(),
            mcp_config,
            prompt_file,
            _temp: temp,
            proc: None,
            turn_active: false,
            session_id: resume,
        })
    }

    fn spawn(&mut self) -> Result<(), AgentError> {
        let resume = self.session_id.clone();
        let args = build_args(&ArgsSpec {
            mcp_config: &self.mcp_config,
            system_prompt_file: &self.prompt_file,
            model: self.model.as_deref(),
            effort: self.effort,
            resume: resume.as_deref(),
            allow_web: self.cfg.allow_web,
            extra: &self.cfg.extra_args,
        });
        let cwd = self.cfg.working_dir();
        let _ = std::fs::create_dir_all(&cwd);
        let mut cmd = tokio::process::Command::new(&self.claude);
        cmd.args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // `import_website` can run for a minute or more: make sure Claude
        // Code waits for slow tool calls (unless the user chose a timeout).
        if std::env::var_os(MCP_TOOL_TIMEOUT_ENV).is_none()
            && !self.cfg.env.iter().any(|(k, _)| k == MCP_TOOL_TIMEOUT_ENV)
        {
            cmd.env(MCP_TOOL_TIMEOUT_ENV, MCP_TOOL_TIMEOUT_MS);
        }
        for (k, v) in &self.cfg.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console window flashing up from a GUI app.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AgentError::ClaudeNotFound
            } else {
                AgentError::Other(format!("could not start Claude Code: {e}"))
            }
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::Other("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Other("no stdout".into()))?;
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        if let Some(mut err) = child.stderr.take() {
            let buf = stderr_buf.clone();
            tokio::spawn(async move {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = err.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    let mut b = buf.lock().unwrap_or_else(|p| p.into_inner());
                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    // Keep the tail only.
                    if b.len() > 16 * 1024 {
                        let cut = b.len() - 8 * 1024;
                        let cut = (cut..b.len()).find(|&i| b.is_char_boundary(i)).unwrap_or(0);
                        b.drain(..cut);
                    }
                }
            });
        }
        self.mapper = CliMapper::new();
        self.proc = Some(Proc {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr: stderr_buf,
        });
        Ok(())
    }

    async fn kill(&mut self) {
        if let Some(mut p) = self.proc.take() {
            let _ = p.child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), p.child.wait()).await;
        }
    }

    fn start_turn(&mut self) {
        if !self.turn_active {
            self.turn_active = true;
            self.emitter.emit(AgentEvent::TurnStart);
        }
    }

    fn end_turn(&mut self, reason: &str) {
        if self.turn_active {
            self.turn_active = false;
            self.emitter.emit(AgentEvent::TurnEnd {
                stop_reason: reason.into(),
            });
        }
    }

    async fn write_message(&mut self, msg: &UserMessage) {
        let line = user_line(msg);
        for attempt in 0..2 {
            if self.proc.is_none()
                && let Err(e) = self.spawn()
            {
                self.emitter.emit(AgentEvent::Error(e.to_string()));
                self.end_turn("error");
                return;
            }
            let Some(p) = self.proc.as_mut() else { return };
            let ok =
                p.stdin.write_all(line.as_bytes()).await.is_ok() && p.stdin.flush().await.is_ok();
            if ok {
                return;
            }
            // The process went away between turns: start a new one.
            self.kill().await;
            if attempt == 1 {
                self.emitter.emit(AgentEvent::Error(
                    "could not send the message to Claude Code".into(),
                ));
                self.end_turn("error");
            }
        }
    }

    async fn on_line(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if let Some(reply) = control_reply(&v) {
            if let Some(p) = self.proc.as_mut() {
                let _ = p.stdin.write_all(reply.as_bytes()).await;
                let _ = p.stdin.flush().await;
            }
            return;
        }
        let is_message_start = v.get("type").and_then(Value::as_str) == Some("stream_event")
            && v.pointer("/event/type").and_then(Value::as_str) == Some("message_start");
        if is_message_start {
            // A queued message started on its own after the previous result.
            self.start_turn();
        }
        for e in self.mapper.map_value(&v) {
            match e {
                AgentEvent::SessionId(ref id) => {
                    self.session_id = Some(id.clone());
                    self.emitter.emit(e);
                }
                AgentEvent::TurnEnd { stop_reason } => self.end_turn(&stop_reason),
                other => self.emitter.emit(other),
            }
        }
    }

    async fn on_exit(&mut self) {
        let Some(mut p) = self.proc.take() else {
            return;
        };
        let status = tokio::time::timeout(std::time::Duration::from_secs(2), p.child.wait())
            .await
            .ok()
            .and_then(Result::ok);
        // Let the stderr reader drain.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if self.turn_active {
            let stderr = p
                .stderr
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .trim()
                .to_string();
            let msg = if looks_like_auth_error(&stderr) {
                AgentError::ClaudeNotLoggedIn.to_string()
            } else {
                let code = status
                    .and_then(|s| s.code())
                    .map(|c| format!(" (exit code {c})"))
                    .unwrap_or_default();
                let tail: String = stderr
                    .lines()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                if tail.is_empty() {
                    format!("Claude Code stopped unexpectedly{code}")
                } else {
                    format!("Claude Code stopped unexpectedly{code}: {tail}")
                }
            };
            self.emitter.emit(AgentEvent::Error(msg));
            self.end_turn("error");
        }
    }

    pub async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Command>) {
        loop {
            let line = async {
                match self.proc.as_mut() {
                    Some(p) => p.lines.next_line().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    None | Some(Command::Shutdown) => break,
                    Some(Command::Send(m)) => {
                        self.start_turn();
                        self.write_message(&m).await;
                    }
                    Some(Command::Steer(t)) => {
                        self.start_turn();
                        self.write_message(&UserMessage::text(t)).await;
                    }
                    Some(Command::Stop) => {
                        if self.proc.is_some() {
                            self.kill().await;
                        }
                        self.end_turn("interrupted");
                    }
                },
                l = line => match l {
                    Ok(Some(l)) => self.on_line(&l).await,
                    Ok(None) | Err(_) => self.on_exit().await,
                },
            }
        }
        self.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const FIXTURE: &str = include_str!("../tests/fixtures/claude-cli-execute.jsonl");

    fn map_all(src: &str) -> Vec<AgentEvent> {
        let mut m = CliMapper::new();
        src.lines().flat_map(|l| m.map_line(l)).collect()
    }

    #[test]
    fn recorded_transcript_maps_to_events() {
        let ev = map_all(FIXTURE);
        let sid = "00000000-0000-4000-8000-000000000001";
        assert_eq!(ev[0], AgentEvent::SessionId(sid.into()));
        let id = "toolu_017A4dbdEQS7hMKg6e8QgCx2";
        assert_eq!(
            ev[1],
            AgentEvent::ToolUseStart {
                id: id.into(),
                name: "execute".into()
            }
        );
        let deltas: Vec<&String> = ev
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolInputDelta { partial_json, .. } => Some(partial_json),
                _ => None,
            })
            .collect();
        // The CLI streams MCP tool input in many chunks.
        assert!(deltas.len() >= 10, "{}", deltas.len());
        let scripts: Vec<String> = deltas
            .iter()
            .filter_map(|d| crate::execute_input_prefix(d))
            .collect();
        assert!(scripts.len() >= 10);
        assert!(scripts.windows(2).all(|w| w[1].starts_with(&w[0])));
        let ends: Vec<_> = ev
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolUseEnd { .. }))
            .collect();
        assert_eq!(ends.len(), 1, "one ToolUseEnd per call");
        let AgentEvent::ToolUseEnd { input, .. } = ends[0] else {
            unreachable!()
        };
        assert_eq!(input["description"], "Print a list");
        assert!(
            input["input"]
                .as_str()
                .unwrap()
                .ends_with("Print(\"line 12\");")
        );
        let result = ev.iter().find_map(|e| match e {
            AgentEvent::ToolResult {
                id: rid,
                ok,
                text,
                images,
            } => Some((rid, *ok, text, *images)),
            _ => None,
        });
        let (rid, ok, text, images) = result.unwrap();
        assert_eq!(rid, id);
        assert!(ok && images == 0 && text.contains("line 12"));
        let text: String = ev
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            text,
            "The script ran once and printed lines 1 through 12 with no document changes."
        );
        let n = ev.len();
        assert!(
            matches!(&ev[n - 2], AgentEvent::Usage(u) if u.output_tokens == 197 && u.cache_read == 9635 && u.cost_usd.is_some())
        );
        assert_eq!(
            ev[n - 1],
            AgentEvent::TurnEnd {
                stop_reason: "end_turn".into()
            }
        );
    }

    #[test]
    fn without_partial_messages_assistant_is_used() {
        let src = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Hi"},{"type":"tool_use","id":"t1","name":"mcp__openpen__get_app_state","input":{}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"boom"}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"a"},{"type":"image","source":{}},{"type":"text","text":"b"}]}]}}"#;
        assert_eq!(
            map_all(src),
            vec![
                AgentEvent::ThinkingDelta("hmm".into()),
                AgentEvent::TextDelta("Hi".into()),
                AgentEvent::ToolUseStart {
                    id: "t1".into(),
                    name: "get_app_state".into()
                },
                AgentEvent::ToolUseEnd {
                    id: "t1".into(),
                    input: json!({})
                },
                AgentEvent::ToolResult {
                    id: "t1".into(),
                    ok: false,
                    text: "boom".into(),
                    images: 0
                },
                AgentEvent::ToolResult {
                    id: "t2".into(),
                    ok: true,
                    text: "a\nb".into(),
                    images: 1
                },
            ]
        );
    }

    #[test]
    fn error_results() {
        let ev = map_all(
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login","session_id":"s"}"#,
        );
        assert!(ev.contains(&AgentEvent::Error(
            AgentError::ClaudeNotLoggedIn.to_string()
        )));
        assert_eq!(
            ev.last(),
            Some(&AgentEvent::TurnEnd {
                stop_reason: "error".into()
            })
        );
        let ev = map_all(r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#);
        assert!(matches!(&ev[1], AgentEvent::Error(m) if m.contains("error_max_turns")));
    }

    #[test]
    fn thinking_and_text_stream() {
        let src = r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}}}
{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"a"}}}
{"type":"stream_event","event":{"type":"content_block_stop","index":0}}
{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}
{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"b"}}}
{"type":"assistant","message":{"id":"m","content":[{"type":"thinking","thinking":"a"},{"type":"text","text":"b"}]}}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}},"parent_tool_use_id":"toolu_sub"}
not json"#;
        assert_eq!(
            map_all(src),
            vec![
                AgentEvent::ThinkingDelta("a".into()),
                AgentEvent::TextDelta("b".into())
            ]
        );
    }

    #[test]
    fn args_and_input_lines() {
        let extra = vec!["--foo".to_string()];
        let a = build_args(&ArgsSpec {
            mcp_config: Path::new("m.json"),
            system_prompt_file: Path::new("p.md"),
            model: Some("opus"),
            effort: Effort::Low,
            resume: Some("sid"),
            allow_web: true,
            extra: &extra,
        });
        let s = a.join(" ");
        for want in [
            "-p --input-format stream-json --output-format stream-json --verbose --include-partial-messages",
            "--strict-mcp-config",
            "--mcp-config m.json",
            "--append-system-prompt-file p.md",
            "--tools WebSearch,WebFetch",
            "--allowedTools mcp__openpen__*,WebSearch,WebFetch",
            "--disallowedTools Bash,PowerShell,Edit,Write,NotebookEdit,Agent",
            "--permission-mode dontAsk",
            "--model opus",
            "--effort low",
            "--resume sid",
        ] {
            assert!(s.contains(want), "missing {want} in {s}");
        }
        assert_eq!(a.last().unwrap(), "--foo");
        let a = build_args(&ArgsSpec {
            mcp_config: Path::new("m.json"),
            system_prompt_file: Path::new("p.md"),
            model: None,
            effort: Effort::High,
            resume: None,
            allow_web: false,
            extra: &[],
        });
        let i = a.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(a[i + 1], "");
        assert!(!a.contains(&"--resume".to_string()) && !a.contains(&"--model".to_string()));

        let mut cfg = CliConfig::new("C:/x/openpen-cli.exe");
        cfg.mcp_socket = Some("dev".into());
        let name = cfg.agent_name_with_marker();
        assert!(name.starts_with("OpenPen Chat#") && name.len() == "OpenPen Chat#".len() + 8);
        let m = mcp_config_json(&cfg, &name);
        assert_eq!(
            m["mcpServers"]["openpen"]["args"],
            json!(["mcp", "--socket", "dev", "--agent", name])
        );

        let line = user_line(&UserMessage {
            text: "hi".into(),
            attachments: vec![Attachment::Image {
                media_type: "image/png".into(),
                data: vec![1, 2, 3],
            }],
            context: Some("<editor_state>x</editor_state>".into()),
        });
        assert!(line.ends_with('\n'));
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user");
        let c = v["message"]["content"].as_array().unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[1]["source"]["data"], "AQID");
        assert_eq!(c[2]["text"], "hi");
    }

    #[test]
    fn control_requests_are_denied() {
        let r = control_reply(&json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash"}})).unwrap();
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["request_id"], "r1");
        assert!(control_reply(&json!({"type":"result"})).is_none());
    }
}
