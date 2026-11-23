//! View model of the chat panel: a pure reducer from the agent's
//! [`AgentEvent`] stream to the items the panel shows (user messages,
//! streamed assistant text, thinking, tool-call rows, per-turn usage and
//! error banners). No GPUI: tested without a window.
//!
//! The reducer also returns [`Effect`]s for the app: the growing source of a
//! streamed `execute` (Claude Code engine: the app feeds it to
//! `GuiBackend::stream_partial`), stream cancellations and "persist now".

use std::collections::{HashMap, HashSet};

use pen_agent::{AgentEvent, Usage, execute_input_prefix};
use serde::{Deserialize, Serialize};

/// Name of the streaming tool (without the MCP prefix).
pub const EXECUTE: &str = "execute";

/// Where a tool call stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    /// The model is still writing the input.
    Streaming,
    /// The input is complete; waiting for the result.
    Running,
    Done,
    Failed,
    /// The turn ended (stop, error) before the tool finished.
    Cancelled,
}

impl ToolState {
    pub fn busy(self) -> bool {
        matches!(self, ToolState::Streaming | ToolState::Running)
    }
}

/// One tool call of the assistant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// `description` of an `execute` (the row label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The JS source of an `execute`, or the input JSON of other tools.
    #[serde(default)]
    pub code: String,
    pub state: ToolState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub images: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl ToolCall {
    /// Label of the row: the `execute` description, else the tool name.
    pub fn label(&self) -> &str {
        match &self.description {
            Some(d) if self.name == EXECUTE && !d.trim().is_empty() => d.trim(),
            _ => &self.name,
        }
    }
}

/// Token usage of one turn (serializable copy of [`pen_agent::Usage`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl From<Usage> for TurnUsage {
    fn from(u: Usage) -> Self {
        TurnUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read: u.cache_read,
            cache_write: u.cache_write,
            cost_usd: u.cost_usd,
        }
    }
}

impl TurnUsage {
    /// All input tokens, cached or not.
    pub fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_read + self.cache_write
    }

    /// `12.4k in · 310 out · $0.0042` (the cost only when known).
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} in · {} out",
            compact(self.total_input()),
            compact(self.output_tokens)
        );
        if let Some(c) = self.cost_usd {
            s.push_str(&format!(" · {}", money(c)));
        }
        s
    }
}

/// `950`, `12.4k`, `1.2M`.
pub fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..999_950 => trim_zero(format!("{:.1}", n as f64 / 1e3)) + "k",
        _ => trim_zero(format!("{:.1}", n as f64 / 1e6)) + "M",
    }
}

fn trim_zero(s: String) -> String {
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// `$0.0042`, `$0.13`, `$2.50`.
pub fn money(c: f64) -> String {
    if c < 0.01 {
        format!("${c:.4}")
    } else if c < 1.0 {
        format!("${c:.3}")
    } else {
        format!("${c:.2}")
    }
}

/// What went wrong, for the banner's wording and actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    ClaudeNotFound,
    NotLoggedIn,
    NoApiKey,
    Network,
    Other,
}

/// Classifies an error message of `pen-agent` (its `AgentError` texts and
/// the API engine's HTTP errors).
pub fn classify_error(msg: &str) -> ErrorKind {
    let l = msg.to_ascii_lowercase();
    if l.contains("claude code is not installed") || l.contains("`claude` command was not found") {
        ErrorKind::ClaudeNotFound
    } else if l.contains("not logged in")
        || l.contains("/login")
        || l.contains("invalid api key")
        || l.contains("authentication_error")
        || l.contains("oauth token has expired")
    {
        ErrorKind::NotLoggedIn
    } else if l.contains("no anthropic api key") {
        ErrorKind::NoApiKey
    } else if l.contains("could not reach")
        || l.contains("response stream broke")
        || l.contains("error sending request")
        || l.contains("connection")
        || l.contains("dns")
        || l.contains("timed out")
        || l.contains("network")
    {
        ErrorKind::Network
    } else {
        ErrorKind::Other
    }
}

/// One entry of the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    User {
        text: String,
        /// Sent while the agent was working (steering).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        steer: bool,
    },
    /// Assistant text (markdown).
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    Tool(ToolCall),
    /// End of a turn, with its usage.
    TurnEnd {
        stop_reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TurnUsage>,
    },
    Error {
        error: ErrorKind,
        message: String,
    },
}

/// Something the app must do after an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Feed the decoded source prefix of a streaming `execute` to the
    /// document (`GuiBackend::stream_partial`).
    StreamPartial { tool_use_id: String, prefix: String },
    /// Roll back a stream that will not be finalized.
    CancelStream { tool_use_id: String },
    /// The conversation changed in a way worth saving.
    Persist,
}

/// The conversation of one chat panel.
#[derive(Debug, Clone, Default)]
pub struct ChatModel {
    pub items: Vec<Item>,
    /// A turn is running.
    pub running: bool,
    /// Engine session to resume (Claude Code session id).
    pub session_id: Option<String>,
    /// Emit [`Effect::StreamPartial`] from `ToolInputDelta` (Claude Code
    /// engine; the API engine streams through the executor's hook).
    pub stream_partials: bool,
    /// Bumped on every change (render caches key on it).
    pub revision: u64,
    pending_usage: Option<TurnUsage>,
    /// Raw partial JSON of the tools being written.
    partial_json: HashMap<String, String>,
    /// Length of the last prefix sent per tool (no duplicate partials).
    sent: HashMap<String, usize>,
    /// Tools whose input is complete: late partials are dropped.
    closed: HashSet<String>,
}

impl ChatModel {
    pub fn new(stream_partials: bool) -> Self {
        ChatModel {
            stream_partials,
            ..Default::default()
        }
    }

    /// A model holding restored items (nothing is running).
    pub fn restored(items: Vec<Item>, session_id: Option<String>) -> Self {
        let mut m = ChatModel {
            items,
            session_id,
            ..Default::default()
        };
        // A conversation saved mid-turn: those tools never finished.
        for it in &mut m.items {
            if let Item::Tool(t) = it
                && t.state.busy()
            {
                t.state = ToolState::Cancelled;
            }
        }
        m
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds a message typed by the user.
    pub fn push_user(&mut self, text: impl Into<String>, steer: bool) {
        self.items.push(Item::User {
            text: text.into(),
            steer,
        });
        self.revision += 1;
    }

    /// Adds an error banner (also used for failures to start a session).
    pub fn push_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.items.push(Item::Error {
            error: classify_error(&message),
            message,
        });
        self.revision += 1;
    }

    /// The tool calls that are still being written or run.
    pub fn busy_tools(&self) -> impl Iterator<Item = &ToolCall> {
        self.items.iter().filter_map(|i| match i {
            Item::Tool(t) if t.state.busy() => Some(t),
            _ => None,
        })
    }

    /// The last error banner of the conversation, if nothing came after it
    /// but its turn end.
    pub fn last_error(&self) -> Option<(ErrorKind, &str)> {
        for it in self.items.iter().rev() {
            match it {
                Item::TurnEnd { .. } => continue,
                Item::Error { error, message } => return Some((*error, message)),
                _ => return None,
            }
        }
        None
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolCall> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    fn append_text(&mut self, delta: &str, thinking: bool) {
        match self.items.last_mut() {
            Some(Item::Text { text }) if !thinking => text.push_str(delta),
            Some(Item::Thinking { text }) if thinking => text.push_str(delta),
            _ => self.items.push(if thinking {
                Item::Thinking {
                    text: delta.to_string(),
                }
            } else {
                Item::Text {
                    text: delta.to_string(),
                }
            }),
        }
    }

    /// Applies one event; returns what the app must do.
    pub fn apply(&mut self, ev: AgentEvent) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.revision += 1;
        match ev {
            AgentEvent::TurnStart => {
                self.running = true;
                self.pending_usage = None;
            }
            AgentEvent::SessionId(id) => {
                if self.session_id.as_deref() != Some(id.as_str()) {
                    self.session_id = Some(id);
                    fx.push(Effect::Persist);
                }
            }
            AgentEvent::TextDelta(t) => self.append_text(&t, false),
            AgentEvent::ThinkingDelta(t) => self.append_text(&t, true),
            AgentEvent::ToolUseStart { id, name } => {
                if self.tool_mut(&id).is_none() {
                    self.items.push(Item::Tool(ToolCall {
                        id,
                        name,
                        description: None,
                        code: String::new(),
                        state: ToolState::Streaming,
                        result: None,
                        images: 0,
                    }));
                }
            }
            AgentEvent::ToolInputDelta { id, partial_json } => {
                if self.closed.contains(&id) {
                    return fx;
                }
                let stream = self.stream_partials;
                let Some(t) = self.tool_mut(&id) else {
                    return fx;
                };
                if t.name == EXECUTE {
                    if let Some(d) = partial_string_field(&partial_json, "description") {
                        t.description = Some(d);
                    }
                    if let Some(prefix) = execute_input_prefix(&partial_json) {
                        t.code.clone_from(&prefix);
                        let sent = self.sent.get(&id).copied();
                        if stream && sent != Some(prefix.len()) {
                            self.sent.insert(id.clone(), prefix.len());
                            fx.push(Effect::StreamPartial {
                                tool_use_id: id.clone(),
                                prefix,
                            });
                        }
                    }
                } else {
                    t.code.clone_from(&partial_json);
                }
                self.partial_json.insert(id, partial_json);
            }
            AgentEvent::ToolUseEnd { id, input } => {
                self.closed.insert(id.clone());
                self.partial_json.remove(&id);
                if let Some(t) = self.tool_mut(&id) {
                    if t.name == EXECUTE {
                        if let Some(d) = input.get("description").and_then(|v| v.as_str()) {
                            t.description = Some(d.to_string());
                        }
                        if let Some(c) = input.get("input").and_then(|v| v.as_str()) {
                            t.code = c.to_string();
                        }
                    } else {
                        t.code = serde_json::to_string_pretty(&input).unwrap_or_default();
                    }
                    if t.state == ToolState::Streaming {
                        t.state = ToolState::Running;
                    }
                }
            }
            AgentEvent::ToolResult {
                id,
                ok,
                text,
                images,
            } => {
                self.closed.insert(id.clone());
                if let Some(t) = self.tool_mut(&id) {
                    t.state = if ok {
                        ToolState::Done
                    } else {
                        ToolState::Failed
                    };
                    t.result = Some(text);
                    t.images = images;
                }
            }
            AgentEvent::Usage(u) => self.pending_usage = Some(u.into()),
            AgentEvent::TurnEnd { stop_reason } => {
                for it in &mut self.items {
                    if let Item::Tool(t) = it
                        && t.state.busy()
                    {
                        if t.state == ToolState::Streaming && t.name == EXECUTE {
                            fx.push(Effect::CancelStream {
                                tool_use_id: t.id.clone(),
                            });
                        }
                        self.closed.insert(t.id.clone());
                        t.state = ToolState::Cancelled;
                    }
                }
                self.items.push(Item::TurnEnd {
                    stop_reason,
                    usage: self.pending_usage.take(),
                });
                self.running = false;
                self.partial_json.clear();
                self.sent.clear();
                fx.push(Effect::Persist);
            }
            AgentEvent::Error(msg) => {
                self.push_error(msg);
                fx.push(Effect::Persist);
            }
        }
        fx
    }

    /// Usage of the whole conversation (sum of the turns).
    pub fn total_usage(&self) -> TurnUsage {
        let mut t = TurnUsage::default();
        for it in &self.items {
            if let Item::TurnEnd { usage: Some(u), .. } = it {
                t.input_tokens += u.input_tokens;
                t.output_tokens += u.output_tokens;
                t.cache_read += u.cache_read;
                t.cache_write += u.cache_write;
                if let Some(c) = u.cost_usd {
                    *t.cost_usd.get_or_insert(0.0) += c;
                }
            }
        }
        t
    }

    /// Title of the conversation: the start of the first user message.
    pub fn title(&self) -> String {
        let first = self.items.iter().find_map(|i| match i {
            Item::User { text, .. } => Some(text.as_str()),
            _ => None,
        });
        let t = first.unwrap_or("New chat").trim();
        let line = t.lines().next().unwrap_or("");
        if line.chars().count() > 60 {
            let mut s: String = line.chars().take(57).collect();
            s.push('…');
            s
        } else {
            line.to_string()
        }
    }
}

/// Best-effort decoded value of a top-level string field of a partial JSON
/// object (for labels while the model is still writing). `None` until the
/// value has started.
pub fn partial_string_field(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let mut from = 0;
    let start = loop {
        let at = from + json[from..].find(&pat)?;
        // Skip an escaped occurrence inside another string (`\"key\"`).
        if at > 0 && json.as_bytes()[at - 1] == b'\\' {
            from = at + pat.len();
            continue;
        }
        let rest = json[at + pat.len()..].trim_start();
        let rest = rest.strip_prefix(':')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        break json.len() - rest.len();
    };
    let mut out = String::new();
    let mut chars = json[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => {}
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_start(id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolUseStart {
            id: id.into(),
            name: name.into(),
        }
    }

    fn delta(id: &str, j: &str) -> AgentEvent {
        AgentEvent::ToolInputDelta {
            id: id.into(),
            partial_json: j.into(),
        }
    }

    fn tool(m: &ChatModel, id: &str) -> ToolCall {
        m.items
            .iter()
            .find_map(|i| match i {
                Item::Tool(t) if t.id == id => Some(t.clone()),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn text_and_thinking_merge_per_block() {
        let mut m = ChatModel::new(false);
        m.push_user("hi", false);
        m.apply(AgentEvent::TurnStart);
        assert!(m.running);
        m.apply(AgentEvent::ThinkingDelta("Let me ".into()));
        m.apply(AgentEvent::ThinkingDelta("think.".into()));
        m.apply(AgentEvent::TextDelta("**Hello**".into()));
        m.apply(AgentEvent::TextDelta(" world".into()));
        m.apply(tool_start("t1", "get_app_state"));
        m.apply(AgentEvent::TextDelta("After.".into()));
        assert_eq!(
            &m.items[1..3],
            &[
                Item::Thinking {
                    text: "Let me think.".into()
                },
                Item::Text {
                    text: "**Hello** world".into()
                }
            ]
        );
        assert_eq!(
            m.items.last(),
            Some(&Item::Text {
                text: "After.".into()
            })
        );
    }

    #[test]
    fn execute_row_streams_label_code_and_partials() {
        let mut m = ChatModel::new(true);
        m.apply(AgentEvent::TurnStart);
        m.apply(tool_start("t1", EXECUTE));
        assert_eq!(tool(&m, "t1").label(), EXECUTE);
        assert!(m.apply(delta("t1", "{\"descr")).is_empty());
        let fx = m.apply(delta("t1", "{\"description\": \"Build the ca"));
        assert!(fx.is_empty());
        assert_eq!(tool(&m, "t1").label(), "Build the ca");
        let fx = m.apply(delta(
            "t1",
            "{\"description\": \"Build the card\", \"input\": \"a = 1;\\nb",
        ));
        assert_eq!(
            fx,
            vec![Effect::StreamPartial {
                tool_use_id: "t1".into(),
                prefix: "a = 1;\nb".into()
            }]
        );
        assert_eq!(tool(&m, "t1").state, ToolState::Streaming);
        // Same prefix (only the closing quote arrived): no duplicate.
        let full = "{\"description\": \"Build the card\", \"input\": \"a = 1;\\nb\"}";
        assert!(m.apply(delta("t1", full)).is_empty());
        m.apply(AgentEvent::ToolUseEnd {
            id: "t1".into(),
            input: json!({"description": "Build the card", "input": "a = 1;\nb"}),
        });
        let t = tool(&m, "t1");
        assert_eq!(
            (t.state, t.code.as_str()),
            (ToolState::Running, "a = 1;\nb")
        );
        // A late partial after the input closed is dropped.
        assert!(
            m.apply(delta(
                "t1",
                "{\"description\": \"x\", \"input\": \"a = 1;\\nb = 2\""
            ))
            .is_empty()
        );
        m.apply(AgentEvent::ToolResult {
            id: "t1".into(),
            ok: true,
            text: "Done".into(),
            images: 1,
        });
        let t = tool(&m, "t1");
        assert_eq!(t.state, ToolState::Done);
        assert_eq!((t.result.as_deref(), t.images), (Some("Done"), 1));
    }

    #[test]
    fn api_engine_does_not_emit_partials() {
        let mut m = ChatModel::new(false);
        m.apply(tool_start("t1", EXECUTE));
        assert!(
            m.apply(delta("t1", "{\"input\": \"a = 1;\""))
                .iter()
                .all(|e| !matches!(e, Effect::StreamPartial { .. }))
        );
        assert_eq!(tool(&m, "t1").code, "a = 1;");
    }

    #[test]
    fn other_tools_keep_their_json_and_name() {
        let mut m = ChatModel::new(true);
        m.apply(tool_start("t2", "read_skill"));
        let fx = m.apply(delta("t2", "{\"path\": \"exe"));
        assert!(fx.is_empty());
        m.apply(AgentEvent::ToolUseEnd {
            id: "t2".into(),
            input: json!({"path": "execute.md"}),
        });
        m.apply(AgentEvent::ToolResult {
            id: "t2".into(),
            ok: false,
            text: "nope".into(),
            images: 0,
        });
        let t = tool(&m, "t2");
        assert_eq!(t.label(), "read_skill");
        assert!(t.code.contains("\"path\": \"execute.md\""));
        assert_eq!(t.state, ToolState::Failed);
    }

    #[test]
    fn interrupted_turn_cancels_busy_tools_and_records_usage() {
        let mut m = ChatModel::new(true);
        m.apply(AgentEvent::TurnStart);
        m.apply(tool_start("t1", EXECUTE));
        m.apply(delta("t1", "{\"input\": \"a = 1;\\n"));
        m.apply(tool_start("t2", "get_app_state"));
        m.apply(AgentEvent::ToolUseEnd {
            id: "t2".into(),
            input: json!({}),
        });
        m.apply(AgentEvent::Usage(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read: 2000,
            cache_write: 0,
            cost_usd: Some(0.0021),
        }));
        let fx = m.apply(AgentEvent::TurnEnd {
            stop_reason: "interrupted".into(),
        });
        assert_eq!(
            fx,
            vec![
                Effect::CancelStream {
                    tool_use_id: "t1".into()
                },
                Effect::Persist
            ]
        );
        assert!(!m.running);
        assert_eq!(tool(&m, "t1").state, ToolState::Cancelled);
        assert_eq!(tool(&m, "t2").state, ToolState::Cancelled);
        match m.items.last() {
            Some(Item::TurnEnd { stop_reason, usage }) => {
                assert_eq!(stop_reason, "interrupted");
                assert_eq!(usage.unwrap().summary(), "2k in · 5 out · $0.0021");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(m.total_usage().output_tokens, 5);
    }

    #[test]
    fn errors_are_classified() {
        let mut m = ChatModel::new(true);
        let fx = m.apply(AgentEvent::Error(
            pen_agent::AgentError::ClaudeNotLoggedIn.to_string(),
        ));
        assert_eq!(fx, vec![Effect::Persist]);
        assert_eq!(m.last_error().unwrap().0, ErrorKind::NotLoggedIn);
        let k = |e: pen_agent::AgentError| classify_error(&e.to_string());
        assert_eq!(
            k(pen_agent::AgentError::ClaudeNotFound),
            ErrorKind::ClaudeNotFound
        );
        assert_eq!(k(pen_agent::AgentError::NoApiKey), ErrorKind::NoApiKey);
        assert_eq!(
            classify_error("could not reach the Anthropic API: dns error"),
            ErrorKind::Network
        );
        assert_eq!(
            classify_error("Anthropic API error 400 (invalid_request_error): x"),
            ErrorKind::Other
        );
        m.apply(AgentEvent::TurnEnd {
            stop_reason: "error".into(),
        });
        assert!(m.last_error().is_some(), "the turn end does not hide it");
        m.push_user("again", false);
        assert!(m.last_error().is_none());
    }

    #[test]
    fn session_id_persists_once() {
        let mut m = ChatModel::new(true);
        assert_eq!(
            m.apply(AgentEvent::SessionId("s1".into())),
            vec![Effect::Persist]
        );
        assert!(m.apply(AgentEvent::SessionId("s1".into())).is_empty());
        assert_eq!(m.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn restored_conversation_has_no_busy_tools() {
        let m = ChatModel::restored(
            vec![Item::Tool(ToolCall {
                id: "t".into(),
                name: EXECUTE.into(),
                description: Some("x".into()),
                code: String::new(),
                state: ToolState::Running,
                result: None,
                images: 0,
            })],
            Some("s".into()),
        );
        assert_eq!(m.busy_tools().count(), 0);
        assert!(!m.running);
    }

    #[test]
    fn partial_field_decoding() {
        assert_eq!(partial_string_field("{\"a\": 1", "description"), None);
        assert_eq!(
            partial_string_field("{\"description\":", "description"),
            None
        );
        assert_eq!(
            partial_string_field("{\"description\": \"", "description").as_deref(),
            Some("")
        );
        assert_eq!(
            partial_string_field(
                "{\"input\": \"x = \\\"description\\\"\", \"description\": \"Say \\\"hi\\\"\\u00e9\"",
                "description"
            )
            .as_deref(),
            Some("Say \"hi\"é")
        );
    }

    #[test]
    fn formatting() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(12_400), "12.4k");
        assert_eq!(compact(2_000), "2k");
        assert_eq!(compact(1_260_000), "1.3M");
        assert_eq!(money(0.0042), "$0.0042");
        assert_eq!(money(0.134), "$0.134");
        assert_eq!(money(2.5), "$2.50");
        let mut m = ChatModel::new(false);
        assert_eq!(m.title(), "New chat");
        m.push_user("Build a login card\nwith two fields", false);
        assert_eq!(m.title(), "Build a login card");
    }

    #[test]
    fn items_round_trip_through_json() {
        let items = vec![
            Item::User {
                text: "hi".into(),
                steer: true,
            },
            Item::Text { text: "a".into() },
            Item::Thinking { text: "b".into() },
            Item::Tool(ToolCall {
                id: "t".into(),
                name: EXECUTE.into(),
                description: Some("d".into()),
                code: "c".into(),
                state: ToolState::Done,
                result: Some("r".into()),
                images: 2,
            }),
            Item::TurnEnd {
                stop_reason: "end_turn".into(),
                usage: Some(TurnUsage {
                    input_tokens: 1,
                    output_tokens: 2,
                    cache_read: 3,
                    cache_write: 4,
                    cost_usd: Some(0.5),
                }),
            },
            Item::Error {
                error: ErrorKind::Network,
                message: "x".into(),
            },
        ];
        let s = serde_json::to_string(&items).unwrap();
        let back: Vec<Item> = serde_json::from_str(&s).unwrap();
        assert_eq!(back, items);
    }
}
