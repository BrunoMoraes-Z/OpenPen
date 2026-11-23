//! Rebuilds one assistant message from Messages API stream events, emitting
//! [`AgentEvent`]s as the pieces arrive.

use serde_json::{Map, Value, json};

use crate::tools::normalize_tool_name;
use crate::{AgentEvent, Usage};

#[derive(Debug)]
struct Block {
    /// The `content_block` of `content_block_start`, completed at the end.
    start: Value,
    kind: String,
    text: String,
    thinking: String,
    signature: Option<String>,
    json: String,
    stopped: bool,
}

/// A client tool call of the finished message.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// The input as streamed (may be cut off or invalid with eager streaming).
    pub raw: String,
    pub input: Result<Value, String>,
}

/// An API-level error reported inside the stream (`event: error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Assembler {
    blocks: Vec<Option<Block>>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    pub error: Option<StreamError>,
    pub message_stopped: bool,
    /// Something was emitted to the app (a retry would duplicate it).
    pub emitted: bool,
}

/// What the assembler tells the outside world.
pub trait Sink {
    fn event(&mut self, e: AgentEvent);
    fn partial_input(&mut self, id: &str, name: &str, json: &str);
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles one SSE `data` payload.
    pub fn on_event(&mut self, data: &Value, sink: &mut dyn Sink) {
        let index = data.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        match data.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(u) = data.pointer("/message/usage") {
                    merge_usage(&mut self.usage, u);
                }
            }
            "content_block_start" => {
                let start = data.get("content_block").cloned().unwrap_or(Value::Null);
                let kind = start
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match kind.as_str() {
                    "text" => {
                        if let Some(t) = start
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                        {
                            self.emitted = true;
                            sink.event(AgentEvent::TextDelta(t.to_string()));
                        }
                    }
                    "tool_use" => {
                        self.emitted = true;
                        sink.event(AgentEvent::ToolUseStart {
                            id: str_field(&start, "id"),
                            name: normalize_tool_name(&str_field(&start, "name")).to_string(),
                        });
                    }
                    _ => {}
                }
                let text = start
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let thinking = start
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if self.blocks.len() <= index {
                    self.blocks.resize_with(index + 1, || None);
                }
                self.blocks[index] = Some(Block {
                    start,
                    kind,
                    text,
                    thinking,
                    signature: None,
                    json: String::new(),
                    stopped: false,
                });
            }
            "content_block_delta" => {
                let Some(Some(b)) = self.blocks.get_mut(index) else {
                    return;
                };
                let d = &data["delta"];
                match d.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let t = d.get("text").and_then(Value::as_str).unwrap_or("");
                        b.text.push_str(t);
                        if !t.is_empty() {
                            self.emitted = true;
                            sink.event(AgentEvent::TextDelta(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        let t = d.get("thinking").and_then(Value::as_str).unwrap_or("");
                        b.thinking.push_str(t);
                        if !t.is_empty() {
                            self.emitted = true;
                            sink.event(AgentEvent::ThinkingDelta(t.to_string()));
                        }
                    }
                    "signature_delta" => {
                        let s = d.get("signature").and_then(Value::as_str).unwrap_or("");
                        b.signature.get_or_insert_with(String::new).push_str(s);
                    }
                    "input_json_delta" => {
                        let p = d.get("partial_json").and_then(Value::as_str).unwrap_or("");
                        if p.is_empty() {
                            return;
                        }
                        b.json.push_str(p);
                        if b.kind == "tool_use" {
                            let id = str_field(&b.start, "id");
                            let name = str_field(&b.start, "name");
                            self.emitted = true;
                            sink.partial_input(&id, &name, &b.json);
                            sink.event(AgentEvent::ToolInputDelta {
                                id,
                                partial_json: b.json.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(Some(b)) = self.blocks.get_mut(index) else {
                    return;
                };
                b.stopped = true;
                if b.kind == "tool_use" {
                    let input = parse_input(&b.start, &b.json).unwrap_or(Value::Null);
                    sink.event(AgentEvent::ToolUseEnd {
                        id: str_field(&b.start, "id"),
                        input,
                    });
                }
            }
            "message_delta" => {
                if let Some(r) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(r.to_string());
                }
                if let Some(u) = data.get("usage") {
                    merge_usage(&mut self.usage, u);
                }
            }
            "message_stop" => self.message_stopped = true,
            "error" => {
                self.error = Some(StreamError {
                    kind: data
                        .pointer("/error/type")
                        .and_then(Value::as_str)
                        .unwrap_or("error")
                        .to_string(),
                    message: data
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("stream error")
                        .to_string(),
                });
            }
            _ => {}
        }
    }

    /// Client tool calls that started but did not finish (for cleanup).
    pub fn open_tool_calls(&self) -> Vec<(String, String)> {
        self.blocks
            .iter()
            .flatten()
            .filter(|b| b.kind == "tool_use")
            .map(|b| (str_field(&b.start, "id"), str_field(&b.start, "name")))
            .collect()
    }

    /// The finished content blocks, ready to echo back, and the tool calls.
    pub fn finish(self) -> (Vec<Value>, Vec<ToolCall>) {
        let mut content = Vec::new();
        let mut calls = Vec::new();
        for b in self.blocks.into_iter().flatten() {
            let mut v = match b.start {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            match b.kind.as_str() {
                "text" => {
                    v.insert("text".into(), Value::String(b.text));
                }
                "thinking" => {
                    v.insert("thinking".into(), Value::String(b.thinking));
                    if let Some(s) = b.signature {
                        v.insert("signature".into(), Value::String(s));
                    }
                }
                "tool_use" | "server_tool_use" => {
                    let start = Value::Object(v.clone());
                    let parsed = parse_input(&start, &b.json);
                    if b.kind == "tool_use" {
                        calls.push(ToolCall {
                            id: str_field(&start, "id"),
                            name: str_field(&start, "name"),
                            raw: b.json.clone(),
                            input: parsed.clone(),
                        });
                    }
                    let echo = parsed.unwrap_or_else(|_| json!({"INVALID_JSON": b.json}));
                    v.insert("input".into(), echo);
                }
                _ => {}
            }
            content.push(Value::Object(v));
        }
        (content, calls)
    }
}

fn str_field(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// The tool input: the streamed JSON, or the start block's `input` when no
/// delta came (an empty input).
fn parse_input(start: &Value, json: &str) -> Result<Value, String> {
    if json.trim().is_empty() {
        return Ok(start.get("input").cloned().unwrap_or_else(|| json!({})));
    }
    match serde_json::from_str::<Value>(json) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err("the tool input is not a JSON object".into()),
        Err(e) => Err(format!("the tool input is not valid JSON: {e}")),
    }
}

fn merge_usage(u: &mut Usage, src: &Value) {
    let get = |k: &str| src.get(k).and_then(Value::as_u64);
    // Counters in `message_delta` are cumulative: they replace earlier ones.
    if let Some(n) = get("input_tokens") {
        u.input_tokens = n;
    }
    if let Some(n) = get("output_tokens") {
        u.output_tokens = n;
    }
    if let Some(n) = get("cache_read_input_tokens") {
        u.cache_read = n;
    }
    if let Some(n) = get("cache_creation_input_tokens") {
        u.cache_write = n;
    }
}

/// Prepares content for echoing after a refusal fallback: before the last
/// `fallback` block, only `text` blocks (and the `fallback` blocks, kept in
/// place) are echoed; thinking, tool calls and anything else of the declined
/// attempt are dropped. Returns the IDs of the dropped tool calls.
pub fn sanitize_fallback(content: &mut Vec<Value>) -> Vec<String> {
    let Some(last) = content
        .iter()
        .rposition(|b| b.get("type").and_then(Value::as_str) == Some("fallback"))
    else {
        return Vec::new();
    };
    let mut dropped = Vec::new();
    let mut i = 0;
    content.retain(|b| {
        let keep = i >= last
            || matches!(
                b.get("type").and_then(Value::as_str),
                Some("text" | "fallback")
            );
        if !keep && b.get("type").and_then(Value::as_str) == Some("tool_use") {
            dropped.push(str_field(b, "id"));
        }
        i += 1;
        keep
    });
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::sse::SseParser;

    #[derive(Default)]
    struct Rec {
        events: Vec<AgentEvent>,
        partial: Vec<(String, String, String)>,
    }
    impl Sink for Rec {
        fn event(&mut self, e: AgentEvent) {
            self.events.push(e);
        }
        fn partial_input(&mut self, id: &str, name: &str, json: &str) {
            self.partial.push((id.into(), name.into(), json.into()));
        }
    }

    pub(crate) fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
            .collect()
    }

    fn tool_stream(chunks: &[&str]) -> String {
        let mut ev = vec![
            json!({"type":"message_start","message":{"id":"m1","usage":{"input_tokens":10,"cache_read_input_tokens":5,"cache_creation_input_tokens":2,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan."}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig=="}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"On it."}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"execute","input":{}}}),
        ];
        for c in chunks {
            ev.push(json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":c}}));
        }
        ev.push(json!({"type":"content_block_stop","index":2}));
        ev.push(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}));
        ev.push(json!({"type":"message_stop"}));
        sse(&ev)
    }

    fn run(stream: &str, split: usize) -> (Assembler, Rec) {
        let mut p = SseParser::new();
        let mut a = Assembler::new();
        let mut rec = Rec::default();
        let bytes = stream.as_bytes();
        let mut evs = p.push(&bytes[..split]);
        evs.extend(p.push(&bytes[split..]));
        for e in evs {
            a.on_event(&serde_json::from_str(&e.data).unwrap(), &mut rec);
        }
        (a, rec)
    }

    #[test]
    fn tool_use_with_awkward_chunks() {
        // Chunks split inside escapes, surrogate pairs and keys.
        let chunks = [
            "{\"desc",
            "ription\": \"Add \\",
            "\"x\\\"\", \"inp",
            "ut\": \"Print(\\\"\\ud83d",
            "\\ude00\\",
            "n\\u00",
            "e9\\\")\"",
            "}",
        ];
        let stream = tool_stream(&chunks);
        for split in [
            0,
            7,
            stream.len() / 3,
            stream.len() / 2,
            stream.len() - 3,
            stream.len(),
        ] {
            let (a, rec) = run(&stream, split);
            assert_eq!(a.stop_reason.as_deref(), Some("tool_use"));
            assert_eq!(a.usage.input_tokens, 10);
            assert_eq!(a.usage.output_tokens, 42);
            assert_eq!(a.usage.cache_read, 5);
            assert_eq!(rec.partial.len(), chunks.len());
            let scripts: Vec<_> = rec
                .partial
                .iter()
                .filter_map(|(_, _, j)| crate::execute_input_prefix(j))
                .collect();
            assert!(scripts.windows(2).all(|w| w[1].starts_with(&w[0])));
            assert_eq!(scripts.last().unwrap(), "Print(\"\u{1F600}\n\u{e9}\")");
            assert!(matches!(&rec.events[0], AgentEvent::ThinkingDelta(t) if t == "Plan."));
            assert!(rec.events.contains(&AgentEvent::ToolUseStart {
                id: "toolu_1".into(),
                name: "execute".into()
            }));
            let (content, calls) = a.finish();
            assert_eq!(
                content[0],
                json!({"type":"thinking","thinking":"Plan.","signature":"sig=="})
            );
            assert_eq!(content[1], json!({"type":"text","text":"On it."}));
            assert_eq!(content[2]["input"]["description"], "Add \"x\"");
            assert_eq!(calls.len(), 1);
            assert_eq!(
                calls[0].input.as_ref().unwrap()["input"],
                "Print(\"\u{1F600}\n\u{e9}\")"
            );
        }
    }

    #[test]
    fn invalid_and_empty_tool_input() {
        let (a, rec) = run(
            &tool_stream(&["{\"description\": \"x\", \"input\": \"a\"b\"}"]),
            0,
        );
        assert!(rec.events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolUseEnd {
                input: Value::Null,
                ..
            }
        )));
        let (content, calls) = a.finish();
        assert!(calls[0].input.is_err());
        assert!(content[2]["input"]["INVALID_JSON"].is_string());
        let (a, _) = run(&tool_stream(&[]), 0);
        let (_, calls) = a.finish();
        assert_eq!(calls[0].input, Ok(json!({})));
    }

    #[test]
    fn stream_error_event() {
        let s = sse(&[
            json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
        ]);
        let (a, rec) = run(&s, 0);
        assert_eq!(a.error.as_ref().unwrap().kind, "overloaded_error");
        assert!(!a.emitted && rec.events.is_empty());
    }

    #[test]
    fn fallback_sanitizing() {
        let mut c = vec![
            json!({"type":"thinking","thinking":"a","signature":"s"}),
            json!({"type":"text","text":"partial"}),
            json!({"type":"tool_use","id":"t0","name":"execute","input":{}}),
            json!({"type":"fallback","from":{"model":"a"},"to":{"model":"b"}}),
            json!({"type":"thinking","thinking":"b","signature":"s2"}),
            json!({"type":"tool_use","id":"t1","name":"execute","input":{}}),
        ];
        assert_eq!(sanitize_fallback(&mut c), vec!["t0".to_string()]);
        let kinds: Vec<_> = c.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["text", "fallback", "thinking", "tool_use"]);
        let mut none = vec![json!({"type":"thinking"})];
        assert!(sanitize_fallback(&mut none).is_empty());
        assert_eq!(none.len(), 1);
    }
}
