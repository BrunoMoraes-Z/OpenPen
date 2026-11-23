//! Engine 2: the Anthropic Messages API over raw HTTP, with our own agent
//! loop.
//!
//! Every request streams (SSE). Tools are the OpenPen tools with eager input
//! streaming, so `execute` scripts arrive while the model writes them
//! ([`ToolExecutor::on_partial_input`]); the input is then validated here,
//! because the API no longer does. The stable prefix (tools + system) is
//! cached with a breakpoint on the system block, and the conversation with
//! one on the last block of the last message.

pub mod assemble;
pub mod http;
pub mod sse;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::key::{ApiKey, resolve_api_key};
use crate::session::{Command, Emitter};
use crate::tools::{ToolDef, ToolExecutor, normalize_tool_name, validate_input};
use crate::{AgentError, AgentEvent, Effort, ToolOutput, Usage, UserMessage};
use assemble::{Assembler, Sink, ToolCall, sanitize_fallback};
pub use http::{ApiError, RetryPolicy};

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Beta that enables `fallbacks: "default"` (server-side refusal fallback).
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

/// Settings of the API engine.
pub struct ApiConfig {
    /// `None`: the stored key, else `ANTHROPIC_API_KEY` (see [`crate::key`]).
    pub api_key: Option<ApiKey>,
    pub base_url: String,
    pub max_tokens: u32,
    /// Runs the tools in-process.
    pub executor: Arc<dyn ToolExecutor>,
    /// Default: [`crate::pen_tool_defs`]`(true)`.
    pub tools: Vec<ToolDef>,
    pub retry: RetryPolicy,
    /// `fallbacks: "default"` with the [`FALLBACK_BETA`] header.
    pub refusal_fallback: bool,
    /// `thinking.display` (`"summarized"` shows the reasoning; `None` omits it).
    pub thinking_display: Option<String>,
    /// On the first turn without [`UserMessage::context`], call
    /// `get_app_state` through the executor and send it with the message.
    pub prefetch_app_state: bool,
    /// Model requests per turn before giving up.
    pub max_requests_per_turn: usize,
}

impl ApiConfig {
    pub fn new(executor: Arc<dyn ToolExecutor>) -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_BASE_URL.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            executor,
            tools: crate::pen_tool_defs(true),
            retry: RetryPolicy::default(),
            refusal_fallback: true,
            thinking_display: Some("summarized".into()),
            prefetch_app_state: true,
            max_requests_per_turn: 200,
        }
    }
}

/// Text of a tool result for a call that was not run.
const NOT_RUN_TRUNCATED: &str = "Not run: your response hit the output token limit before this tool call was complete. \
     Send a smaller call.";
const NOT_RUN_INTERRUPTED: &str = "Not run: the user stopped this turn.";

struct Conv {
    http: http::HttpClient,
    executor: Arc<dyn ToolExecutor>,
    emitter: Emitter,
    model: String,
    effort: Effort,
    max_tokens: u32,
    system_prompt: String,
    tools: Vec<ToolDef>,
    tools_json: Vec<Value>,
    refusal_fallback: bool,
    thinking_display: Option<String>,
    prefetch_app_state: bool,
    max_requests: usize,
    messages: Vec<Value>,
    /// Blocks for the next user message (results of calls that never ran).
    pending_user: Vec<Value>,
    turn_usage: Usage,
    /// Calls that got partial input and have not been resolved yet.
    streaming: Vec<(String, String)>,
    /// Assistant `tool_use` IDs still waiting for a result.
    unanswered: Vec<String>,
    first_turn: bool,
    steer: Arc<Mutex<Vec<String>>>,
}

/// Forwards assembler output to the app and the executor.
struct ConvSink<'a> {
    emitter: &'a Emitter,
    executor: &'a dyn ToolExecutor,
    streaming: &'a mut Vec<(String, String)>,
}

impl Sink for ConvSink<'_> {
    fn event(&mut self, e: AgentEvent) {
        self.emitter.emit(e);
    }

    fn partial_input(&mut self, id: &str, name: &str, json: &str) {
        if !self.streaming.iter().any(|(i, _)| i == id) {
            self.streaming.push((id.to_string(), name.to_string()));
        }
        self.executor
            .on_partial_input(id, normalize_tool_name(name), json);
    }
}

fn text_block(t: impl Into<String>) -> Value {
    json!({"type": "text", "text": t.into()})
}

fn sniff_image(data: &[u8]) -> &'static str {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/png"
    }
}

/// The `tool_result` block for one call.
pub(crate) fn tool_result_block(id: &str, out: &ToolOutput) -> Value {
    let mut content = Vec::new();
    if !out.text.trim().is_empty() {
        content.push(text_block(out.text.clone()));
    }
    for img in &out.images {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": sniff_image(img),
                "data": base64::engine::general_purpose::STANDARD.encode(img),
            }
        }));
    }
    if content.is_empty() {
        content.push(text_block(if out.is_error {
            "Error"
        } else {
            "(no output)"
        }));
    }
    let mut b = json!({"type": "tool_result", "tool_use_id": id, "content": content});
    if out.is_error {
        b["is_error"] = Value::Bool(true);
    }
    b
}

enum Outcome {
    Done(String),
    Failed(String),
}

impl Conv {
    /// Appends user content, merging into a trailing user message (roles
    /// must alternate).
    fn push_user(&mut self, mut blocks: Vec<Value>) {
        if blocks.is_empty() {
            return;
        }
        if let Some(last) = self.messages.last_mut()
            && last["role"] == "user"
            && let Some(c) = last["content"].as_array_mut()
        {
            c.append(&mut blocks);
            return;
        }
        self.messages
            .push(json!({"role": "user", "content": blocks}));
    }

    fn push_assistant(&mut self, content: Vec<Value>) {
        if !content.is_empty() {
            self.messages
                .push(json!({"role": "assistant", "content": content}));
        }
    }

    /// The request body. The last block of the last message carries a cache
    /// breakpoint (added for this request only).
    fn body(&mut self) -> bytes::Bytes {
        let mut thinking = json!({"type": "adaptive"});
        if let Some(d) = &self.thinking_display {
            thinking["display"] = Value::String(d.clone());
        }
        let marked = self
            .messages
            .last_mut()
            .and_then(|m| m["content"].as_array_mut())
            .and_then(|c| c.last_mut())
            .and_then(Value::as_object_mut)
            .map(|b| b.insert("cache_control".into(), json!({"type": "ephemeral"})))
            .is_some();
        let mut req = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "stream": true,
            "system": [{"type": "text", "text": self.system_prompt, "cache_control": {"type": "ephemeral"}}],
            "tools": self.tools_json,
            "tool_choice": {"type": "auto"},
            "thinking": thinking,
            "output_config": {"effort": self.effort.as_str()},
            "messages": self.messages,
        });
        if self.refusal_fallback {
            req["fallbacks"] = Value::String("default".into());
        }
        if marked
            && let Some(b) = self
                .messages
                .last_mut()
                .and_then(|m| m["content"].as_array_mut())
                .and_then(|c| c.last_mut())
                .and_then(Value::as_object_mut)
        {
            b.remove("cache_control");
        }
        bytes::Bytes::from(serde_json::to_vec(&req).unwrap_or_default())
    }

    fn abandon_streaming(&mut self, keep: &[String]) {
        let executor = self.executor.clone();
        self.streaming.retain(|(id, name)| {
            if keep.contains(id) {
                true
            } else {
                executor.on_input_abandoned(id, normalize_tool_name(name));
                false
            }
        });
    }

    /// After the turn future was dropped (stop/shutdown): answer pending
    /// calls so the history stays valid, and release streamed inputs.
    fn repair_after_interrupt(&mut self) {
        self.abandon_streaming(&[]);
        let ids = std::mem::take(&mut self.unanswered);
        for id in ids {
            self.pending_user.push(tool_result_block(
                &id,
                &ToolOutput::error(NOT_RUN_INTERRUPTED),
            ));
        }
        self.steer.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    fn user_blocks(&self, msg: &UserMessage) -> Vec<Value> {
        let mut blocks = Vec::new();
        if let Some(c) = msg.context.as_deref().filter(|c| !c.trim().is_empty()) {
            blocks.push(text_block(c));
        }
        for a in &msg.attachments {
            blocks.push(crate::cli::attachment_block(a));
        }
        if !msg.text.trim().is_empty() || blocks.is_empty() {
            blocks.push(text_block(if msg.text.is_empty() {
                " "
            } else {
                &msg.text
            }));
        }
        blocks
    }

    /// One request with retries. Returns the assembled message.
    async fn request(&mut self) -> Result<Assembler, String> {
        let body = self.body();
        let mut attempt = 0u32;
        loop {
            let err = match self.http.post(body.clone()).await {
                Ok(resp) => match self.consume(resp).await {
                    Ok(a) => return Ok(a),
                    Err((e, emitted)) => {
                        if emitted || !e.retryable {
                            return Err(e.to_string());
                        }
                        e
                    }
                },
                Err(e) => e,
            };
            if !err.retryable || attempt >= self.http.retry.max_retries {
                return Err(err.to_string());
            }
            tokio::time::sleep(self.http.retry.delay(attempt, err.retry_after)).await;
            attempt += 1;
        }
    }

    /// Reads the SSE stream into an [`Assembler`]. On failure, tells whether
    /// anything reached the app already (then a retry would duplicate it).
    async fn consume(&mut self, resp: reqwest::Response) -> Result<Assembler, (ApiError, bool)> {
        let mut parser = sse::SseParser::new();
        let mut asm = Assembler::new();
        let mut stream = resp.bytes_stream();
        let mut sink = ConvSink {
            emitter: &self.emitter,
            executor: self.executor.as_ref(),
            streaming: &mut self.streaming,
        };
        let handle = |events: Vec<sse::SseEvent>, asm: &mut Assembler, sink: &mut ConvSink<'_>| {
            for e in events {
                if e.data.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
                    asm.on_event(&v, sink);
                }
            }
        };
        loop {
            match stream.next().await {
                Some(Ok(chunk)) => {
                    let evs = parser.push(&chunk);
                    handle(evs, &mut asm, &mut sink);
                }
                Some(Err(e)) => {
                    let emitted = asm.emitted;
                    return Err((
                        ApiError {
                            status: 0,
                            kind: "connection_error".into(),
                            message: format!("the response stream broke: {e}"),
                            retry_after: None,
                            retryable: true,
                        },
                        emitted,
                    ));
                }
                None => break,
            }
            if asm.error.is_some() || asm.message_stopped {
                break;
            }
        }
        if let Some(e) = parser.finish() {
            handle(vec![e], &mut asm, &mut sink);
        }
        if let Some(se) = asm.error.take() {
            let retryable = matches!(
                se.kind.as_str(),
                "overloaded_error" | "api_error" | "rate_limit_error"
            );
            let emitted = asm.emitted;
            return Err((
                ApiError {
                    status: 0,
                    kind: se.kind,
                    message: se.message,
                    retry_after: None,
                    retryable,
                },
                emitted,
            ));
        }
        if !asm.message_stopped && asm.stop_reason.is_none() {
            let emitted = asm.emitted;
            return Err((
                ApiError {
                    status: 0,
                    kind: "connection_error".into(),
                    message: "the response stream ended early".into(),
                    retry_after: None,
                    retryable: true,
                },
                emitted,
            ));
        }
        Ok(asm)
    }

    async fn prefetch_context(&self) -> Option<String> {
        if !self
            .tools
            .iter()
            .any(|t| t.name == pen_mcp::tools::GET_APP_STATE)
        {
            return None;
        }
        let out = self
            .executor
            .call(pen_mcp::tools::GET_APP_STATE, json!({}), "prefetch")
            .await;
        (!out.is_error && !out.text.trim().is_empty())
            .then(|| crate::prompt::app_state_context(&out.text))
    }

    fn take_steer(&self) -> Vec<Value> {
        self.steer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .map(text_block)
            .collect()
    }

    /// Runs the tools of one assistant message; returns the result blocks.
    async fn run_tools(&mut self, calls: Vec<ToolCall>) -> Vec<Value> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            let name = normalize_tool_name(&call.name).to_string();
            let checked = call.input.clone().and_then(|input| {
                match self.tools.iter().find(|t| t.name == name) {
                    None => Err(format!("unknown tool `{name}`")),
                    Some(def) => validate_input(&def.input_schema, &input).map(|_| input),
                }
            });
            let out = match checked {
                Ok(input) => {
                    // Running it resolves any streamed input.
                    self.streaming.retain(|(id, _)| *id != call.id);
                    self.executor.call(&name, input, &call.id).await
                }
                Err(why) => {
                    self.abandon_streaming_one(&call.id, &name);
                    let raw = json!({ "INVALID_JSON": call.raw }).to_string();
                    let text = if call.input.is_err() {
                        raw
                    } else {
                        format!("Invalid tool input: {why}")
                    };
                    ToolOutput::error(text)
                }
            };
            let (text, images) = (out.text.clone(), out.images.len());
            self.emitter.emit(AgentEvent::ToolResult {
                id: call.id.clone(),
                ok: !out.is_error,
                text,
                images,
            });
            self.unanswered.retain(|i| *i != call.id);
            results.push(tool_result_block(&call.id, &out));
        }
        results
    }

    fn abandon_streaming_one(&mut self, id: &str, name: &str) {
        if let Some(pos) = self.streaming.iter().position(|(i, _)| i == id) {
            self.streaming.remove(pos);
            self.executor.on_input_abandoned(id, name);
        }
    }

    async fn run_turn(&mut self, msg: UserMessage) -> Outcome {
        self.turn_usage = Usage::default();
        let mut msg = msg;
        if self.first_turn {
            self.first_turn = false;
            if msg.context.is_none() && self.prefetch_app_state {
                msg.context = self.prefetch_context().await;
            }
        }
        let mut blocks = std::mem::take(&mut self.pending_user);
        blocks.extend(self.user_blocks(&msg));
        self.push_user(blocks);

        for _ in 0..self.max_requests {
            let asm = match self.request().await {
                Ok(a) => a,
                Err(e) => {
                    self.abandon_streaming(&[]);
                    return Outcome::Failed(e);
                }
            };
            self.turn_usage.add(&asm.usage);
            let stop = asm.stop_reason.clone().unwrap_or_else(|| "end_turn".into());
            let (mut content, mut calls) = asm.finish();
            let dropped = sanitize_fallback(&mut content);
            if !dropped.is_empty() {
                for id in &dropped {
                    self.emitter.emit(AgentEvent::ToolResult {
                        id: id.clone(),
                        ok: false,
                        text: "Not run: the model was switched (refusal fallback).".into(),
                        images: 0,
                    });
                }
                calls.retain(|c| !dropped.contains(&c.id));
            }
            match stop.as_str() {
                "tool_use" if !calls.is_empty() => {
                    self.unanswered = calls.iter().map(|c| c.id.clone()).collect();
                    self.push_assistant(content);
                    let keep: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
                    self.abandon_streaming(&keep);
                    let mut results = self.run_tools(calls).await;
                    // Steering goes after the results, in the same message.
                    results.extend(self.take_steer());
                    self.push_user(results);
                }
                "max_tokens" if !calls.is_empty() => {
                    // A cut-off input often still parses: never run it.
                    self.push_assistant(content);
                    self.abandon_streaming(&[]);
                    for c in &calls {
                        self.emitter.emit(AgentEvent::ToolResult {
                            id: c.id.clone(),
                            ok: false,
                            text: NOT_RUN_TRUNCATED.into(),
                            images: 0,
                        });
                        self.pending_user.push(tool_result_block(
                            &c.id,
                            &ToolOutput::error(NOT_RUN_TRUNCATED),
                        ));
                    }
                    self.emitter.emit(AgentEvent::Error(
                        "The response hit the output token limit in the middle of a tool call; \
                         the call was not run."
                            .into(),
                    ));
                    return Outcome::Done(stop);
                }
                "refusal" => {
                    // Never run tools of a refused turn; echo only its text.
                    self.abandon_streaming(&[]);
                    for c in &calls {
                        self.emitter.emit(AgentEvent::ToolResult {
                            id: c.id.clone(),
                            ok: false,
                            text: "Not run: the request was declined.".into(),
                            images: 0,
                        });
                    }
                    let mut text: Vec<Value> = content
                        .into_iter()
                        .filter(|b| {
                            b["type"] == "text"
                                && b["text"].as_str().is_some_and(|t| !t.trim().is_empty())
                        })
                        .map(|b| text_block(b["text"].as_str().unwrap_or_default()))
                        .collect();
                    if text.is_empty() {
                        text.push(text_block("(The request was declined.)"));
                    }
                    self.push_assistant(text);
                    return Outcome::Done(stop);
                }
                "pause_turn" => {
                    self.push_assistant(content);
                }
                _ => {
                    self.abandon_streaming(&[]);
                    self.push_assistant(content);
                    let steer = self.take_steer();
                    if steer.is_empty() {
                        return Outcome::Done(stop);
                    }
                    self.push_user(steer);
                }
            }
        }
        Outcome::Failed(format!(
            "stopped after {} model requests in one turn",
            self.max_requests
        ))
    }
}

pub(crate) struct ApiWorker {
    conv: Conv,
    session_id: String,
}

impl ApiWorker {
    pub fn new(
        cfg: ApiConfig,
        system_prompt: String,
        model: Option<String>,
        effort: Effort,
        emitter: Emitter,
    ) -> Result<Self, AgentError> {
        let key = match cfg.api_key {
            Some(k) => k,
            None => resolve_api_key()
                .map(|(k, _)| ApiKey::new(k))
                .ok_or(AgentError::NoApiKey)?,
        };
        if key.expose().is_empty() {
            return Err(AgentError::NoApiKey);
        }
        let betas = if cfg.refusal_fallback {
            vec![FALLBACK_BETA.to_string()]
        } else {
            Vec::new()
        };
        let http = http::HttpClient::new(&cfg.base_url, key, betas, cfg.retry)
            .map_err(AgentError::Other)?;
        let tools_json = cfg.tools.iter().map(ToolDef::to_api_json).collect();
        let session_id = emitter
            .session_id()
            .unwrap_or_else(|| format!("api-{}", crate::random_hex(8)));
        Ok(Self {
            conv: Conv {
                http,
                executor: cfg.executor,
                emitter,
                model: model.unwrap_or_else(|| DEFAULT_MODEL.into()),
                effort,
                max_tokens: cfg.max_tokens,
                system_prompt,
                tools: cfg.tools,
                tools_json,
                refusal_fallback: cfg.refusal_fallback,
                thinking_display: cfg.thinking_display,
                prefetch_app_state: cfg.prefetch_app_state,
                max_requests: cfg.max_requests_per_turn.max(1),
                messages: Vec::new(),
                pending_user: Vec::new(),
                turn_usage: Usage::default(),
                streaming: Vec::new(),
                unanswered: Vec::new(),
                first_turn: true,
                steer: Arc::new(Mutex::new(Vec::new())),
            },
            session_id,
        })
    }

    pub async fn run(mut self, mut cmds: mpsc::UnboundedReceiver<Command>) {
        let mut queue: VecDeque<UserMessage> = VecDeque::new();
        loop {
            let msg = match queue.pop_front() {
                Some(m) => m,
                None => match cmds.recv().await {
                    None | Some(Command::Shutdown) => return,
                    Some(Command::Send(m)) => m,
                    Some(Command::Steer(t)) => UserMessage::text(t),
                    Some(Command::Stop) => continue,
                },
            };
            self.conv.emitter.emit(AgentEvent::TurnStart);
            self.conv
                .emitter
                .emit(AgentEvent::SessionId(self.session_id.clone()));
            let steer = self.conv.steer.clone();
            let mut shutdown = false;
            let outcome = {
                let turn = self.conv.run_turn(msg);
                tokio::pin!(turn);
                loop {
                    tokio::select! {
                        o = &mut turn => break Some(o),
                        c = cmds.recv() => match c {
                            Some(Command::Send(m)) => queue.push_back(m),
                            Some(Command::Steer(t)) => steer.lock().unwrap_or_else(|p| p.into_inner()).push(t),
                            Some(Command::Stop) => break None,
                            None | Some(Command::Shutdown) => {
                                shutdown = true;
                                break None;
                            }
                        },
                    }
                }
            };
            let usage = self.conv.turn_usage;
            let e = &self.conv.emitter;
            match outcome {
                Some(Outcome::Done(stop)) => {
                    e.emit(AgentEvent::Usage(usage));
                    e.emit(AgentEvent::TurnEnd { stop_reason: stop });
                }
                Some(Outcome::Failed(msg)) => {
                    e.emit(AgentEvent::Error(msg));
                    e.emit(AgentEvent::Usage(usage));
                    e.emit(AgentEvent::TurnEnd {
                        stop_reason: "error".into(),
                    });
                }
                None => {
                    self.conv.repair_after_interrupt();
                    queue.clear();
                    let e = &self.conv.emitter;
                    e.emit(AgentEvent::Usage(usage));
                    e.emit(AgentEvent::TurnEnd {
                        stop_reason: "interrupted".into(),
                    });
                }
            }
            if shutdown {
                return;
            }
            // Leftover steering (the turn ended first) becomes a new message.
            let left: Vec<String> = steer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .drain(..)
                .collect();
            if !left.is_empty() {
                queue.push_front(UserMessage::text(left.join("\n\n")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn tool_result_blocks() {
        let b = tool_result_block(
            "t1",
            &ToolOutput::text("hi").with_image(vec![0x89, b'P', b'N', b'G']),
        );
        assert_eq!(b["content"][0]["text"], "hi");
        assert_eq!(b["content"][1]["source"]["media_type"], "image/png");
        assert!(b.get("is_error").is_none());
        let b = tool_result_block("t1", &ToolOutput::error(""));
        assert_eq!(b["is_error"], true);
        assert_eq!(b["content"][0]["text"], "Error");
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0]), "image/jpeg");
    }

    #[test]
    fn delay_bounds() {
        let d = RetryPolicy::default().delay(0, None);
        assert!(d >= Duration::from_secs(1));
    }
}
