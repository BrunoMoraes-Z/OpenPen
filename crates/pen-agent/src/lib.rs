//! Agent core of OpenPen's embedded chat.
//!
//! One [`Session`] talks to one agent through one of two engines, chosen by
//! the user:
//!
//! - [`EngineConfig::ClaudeCli`]: the installed Claude Code CLI (`claude -p`
//!   with stream-json input and output). It reaches the document through the
//!   OpenPen MCP server (`openpen-cli mcp --agent "OpenPen Chat#<marker>"`).
//! - [`EngineConfig::Api`]: the Anthropic Messages API over raw HTTP (SSE),
//!   with our own agent loop. Tools run in-process through a [`ToolExecutor`].
//!
//! Both produce the same [`AgentEvent`] stream. The crate has no GPUI and no
//! document engine; the app plugs those in through [`ToolExecutor`] (or
//! [`BackendExecutor`] over a `pen_mcp::ToolBackend`).

pub mod api;
pub mod cli;
pub mod key;
mod partial;
pub mod prompt;
mod session;
pub mod tools;

pub use partial::execute_input_prefix;
pub use session::{EventReceiver, Session, start_session};
pub use tools::{BackendExecutor, ToolDef, ToolExecutor, pen_tool_defs};

pub use pen_mcp::{BoxFuture, ToolOutput};

/// One normalized event of a chat session.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// The agent started working on a user message.
    TurnStart,
    /// Known once the engine has created (or resumed) the conversation. Pass
    /// it as [`SessionConfig::resume`] to continue later.
    SessionId(String),
    /// A piece of the visible answer.
    TextDelta(String),
    /// A piece of the (summarized) reasoning.
    ThinkingDelta(String),
    /// The model started a tool call. `name` has no MCP prefix (`execute`,
    /// not `mcp__openpen__execute`).
    ToolUseStart { id: String, name: String },
    /// The tool input grew. `partial_json` is everything received so far
    /// (feed it to [`execute_input_prefix`] for `execute`).
    ToolInputDelta { id: String, partial_json: String },
    /// The tool input is complete.
    ToolUseEnd {
        id: String,
        input: serde_json::Value,
    },
    /// The tool ran. `images` counts the images attached to the result.
    ToolResult {
        id: String,
        ok: bool,
        text: String,
        images: usize,
    },
    /// Token usage of the whole turn (all model requests in it).
    Usage(Usage),
    /// The turn is over: `end_turn`, `interrupted`, `refusal`, `max_tokens`,
    /// `error`, ...
    TurnEnd { stop_reason: String },
    /// Something failed; a `TurnEnd` follows when a turn was running.
    Error(String),
}

/// Token counters of one turn.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Reported by the Claude Code CLI; the API engine leaves it empty.
    pub cost_usd: Option<f64>,
}

impl Usage {
    pub(crate) fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        if let Some(c) = o.cost_usd {
            *self.cost_usd.get_or_insert(0.0) += c;
        }
    }
}

/// Something the user attached to a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// `media_type` is `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    Image { media_type: String, data: Vec<u8> },
    /// A text file, sent as a text block with its name.
    Text { name: String, text: String },
}

/// A message from the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserMessage {
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// Volatile context sent before the text in the same message (e.g. the
    /// prefetched `get_app_state`, see [`prompt::app_state_context`]).
    pub context: Option<String>,
}

impl UserMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// How hard the model thinks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Effort {
    #[default]
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// Which engine runs the session.
pub enum EngineConfig {
    ClaudeCli(cli::CliConfig),
    Api(api::ApiConfig),
}

/// Settings of one chat session.
pub struct SessionConfig {
    pub engine: EngineConfig,
    /// Appended to Claude Code's prompt (CLI) or used as the whole system
    /// prompt (API). See [`prompt::chat_system_prompt`].
    pub system_prompt: String,
    /// Model ID or alias. `None`: the CLI's default, or
    /// [`api::DEFAULT_MODEL`] for the API.
    pub model: Option<String>,
    pub effort: Effort,
    /// Conversation to continue: a Claude Code session ID (CLI) or the ID
    /// reported by a previous API session (history is not persisted by this
    /// crate, so the API engine only keeps it as a label).
    pub resume: Option<String>,
}

/// Failures to start or drive a session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    #[error(
        "Claude Code is not installed (the `claude` command was not found on PATH). Install it from https://claude.com/claude-code or pick the API engine."
    )]
    ClaudeNotFound,
    #[error(
        "Claude Code is not logged in. Run `claude` in a terminal and use /login, then try again."
    )]
    ClaudeNotLoggedIn,
    #[error(
        "No Anthropic API key: save one in the settings or set the ANTHROPIC_API_KEY environment variable."
    )]
    NoApiKey,
    #[error("the session has ended")]
    Closed,
    #[error("{0}")]
    Other(String),
}

/// Random lowercase hex string of `bytes * 2` characters.
pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> ((i % 16) * 8)) as u8 ^ (i as u8).wrapping_mul(31);
        }
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
