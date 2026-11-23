//! The API engine against a local mock of the Messages API (wiremock)
//! replaying SSE conversations: the whole loop runs (tool call -> executor ->
//! tool_result -> final text), plus retries, errors, stop and steering.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pen_agent::api::{ApiConfig, RetryPolicy};
use pen_agent::key::ApiKey;
use pen_agent::{
    AgentEvent, BoxFuture, Effort, EngineConfig, EventReceiver, Session, SessionConfig,
    ToolExecutor, ToolOutput, UserMessage, execute_input_prefix, start_session,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

// ---------------------------------------------------------------- helpers

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect()
}

fn sse_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn start_msg(input: u64, cache_read: u64, cache_write: u64) -> Value {
    json!({"type":"message_start","message":{"id":"msg","type":"message","role":"assistant","content":[],
        "usage":{"input_tokens":input,"cache_read_input_tokens":cache_read,"cache_creation_input_tokens":cache_write,"output_tokens":1}}})
}

/// An assistant message that thinks, says something and calls `execute`
/// with its input streamed in the given chunks.
fn tool_turn(id: &str, chunks: &[&str], stop: &str) -> String {
    let mut ev = vec![
        start_msg(100, 0, 900),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"I will add a frame."}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":id,"name":"execute","input":{}}}),
    ];
    for c in chunks {
        ev.push(json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":c}}));
    }
    ev.push(json!({"type":"content_block_stop","index":1}));
    ev.push(
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":50}}),
    );
    ev.push(json!({"type":"message_stop"}));
    sse(&ev)
}

fn text_turn(text: &str, stop: &str) -> String {
    sse(&[
        start_msg(20, 900, 30),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ])
}

const GOOD_CHUNKS: [&str; 5] = [
    "{\"description\": \"Add fr",
    "ame\", \"input\": \"const f = Insert(document, {type: \\\"fra",
    "me\\\"});\\n",
    "Print(f)\\u00e9\"",
    "}",
];

#[derive(Default)]
struct Rec {
    calls: Vec<(String, Value, String)>,
    partial: Vec<(String, String, String)>,
    abandoned: Vec<String>,
}

struct FakeExec {
    rec: Arc<Mutex<Rec>>,
    /// `execute` waits this long (stop/steer tests).
    delay: Duration,
}

impl ToolExecutor for FakeExec {
    fn call(&self, name: &str, input: Value, id: &str) -> BoxFuture<'static, ToolOutput> {
        self.rec
            .lock()
            .unwrap()
            .calls
            .push((name.into(), input, id.into()));
        let delay = if name == "execute" {
            self.delay
        } else {
            Duration::ZERO
        };
        let name = name.to_string();
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            if name == "get_app_state" {
                ToolOutput::text("Focused document: demo.pen")
            } else {
                ToolOutput::text("Created frame f1").with_image(vec![0x89, b'P', b'N', b'G', 1])
            }
        })
    }
    fn on_partial_input(&self, id: &str, name: &str, partial_json: &str) {
        self.rec
            .lock()
            .unwrap()
            .partial
            .push((id.into(), name.into(), partial_json.into()));
    }
    fn on_input_abandoned(&self, id: &str, _name: &str) {
        self.rec.lock().unwrap().abandoned.push(id.into());
    }
}

struct Harness {
    server: MockServer,
    session: Session,
    rx: EventReceiver,
    rec: Arc<Mutex<Rec>>,
}

async fn harness(delay: Duration, prefetch: bool) -> Harness {
    let server = MockServer::start().await;
    let rec = Arc::new(Mutex::new(Rec::default()));
    let mut cfg = ApiConfig::new(Arc::new(FakeExec {
        rec: rec.clone(),
        delay,
    }));
    cfg.api_key = Some(ApiKey::new("test-key-123"));
    cfg.base_url = server.uri();
    cfg.prefetch_app_state = prefetch;
    cfg.retry = RetryPolicy {
        max_retries: 2,
        base_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(50),
    };
    let mut session = start_session(SessionConfig {
        engine: EngineConfig::Api(cfg),
        system_prompt: "You are the OpenPen assistant.".into(),
        model: None,
        effort: Effort::Low,
        resume: None,
    })
    .unwrap();
    let rx = session.take_events().unwrap();
    Harness {
        server,
        session,
        rx,
        rec,
    }
}

/// Mounts responses served in order (each once; the last one repeats).
async fn script(server: &MockServer, responses: Vec<ResponseTemplate>) {
    let n = responses.len();
    for (i, r) in responses.into_iter().enumerate() {
        let m = Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(r)
            .with_priority((i + 1) as u8);
        let m = if i + 1 < n { m.up_to_n_times(1) } else { m };
        m.mount(server).await;
    }
}

async fn until(rx: &mut EventReceiver, pred: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let r = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(e) = rx.recv().await {
            let stop = pred(&e);
            out.push(e);
            if stop {
                return;
            }
        }
    })
    .await;
    assert!(r.is_ok(), "timed out; got {out:#?}");
    out
}

fn turn_end(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::TurnEnd { .. })
}

fn bodies(reqs: &[Request]) -> Vec<Value> {
    reqs.iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

// ------------------------------------------------------------------ tests

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_tool_loop_with_retry() {
    let mut h = harness(Duration::ZERO, true).await;
    script(
        &h.server,
        vec![
            ResponseTemplate::new(529)
                .insert_header("retry-after", "0")
                .set_body_string(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
            sse_response(tool_turn("toolu_A", &GOOD_CHUNKS, "tool_use")),
            sse_response(text_turn("Done.", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("Add a frame")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;

    assert_eq!(ev[0], AgentEvent::TurnStart);
    assert!(matches!(&ev[1], AgentEvent::SessionId(s) if s.starts_with("api-")));
    assert!(ev.contains(&AgentEvent::ThinkingDelta("I will add a frame.".into())));
    assert!(ev.contains(&AgentEvent::ToolUseStart {
        id: "toolu_A".into(),
        name: "execute".into()
    }));
    assert!(ev.contains(&AgentEvent::ToolResult {
        id: "toolu_A".into(),
        ok: true,
        text: "Created frame f1".into(),
        images: 1
    }));
    assert!(ev.contains(&AgentEvent::TextDelta("Done.".into())));
    let usage = ev.iter().find_map(|e| match e {
        AgentEvent::Usage(u) => Some(*u),
        _ => None,
    });
    let u = usage.unwrap();
    assert_eq!(
        (u.input_tokens, u.output_tokens, u.cache_read, u.cache_write),
        (120, 57, 900, 930)
    );
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "end_turn".into()
        })
    );

    // The executor saw the script grow while it was streamed.
    {
        let rec = h.rec.lock().unwrap();
        let scripts: Vec<String> = rec
            .partial
            .iter()
            .filter_map(|(_, _, j)| execute_input_prefix(j))
            .collect();
        assert!(scripts.len() >= 3);
        assert!(scripts.windows(2).all(|w| w[1].starts_with(&w[0])));
        assert_eq!(
            scripts.last().unwrap(),
            "const f = Insert(document, {type: \"frame\"});\nPrint(f)\u{e9}"
        );
        // Prefetch + the execute call, with the validated input.
        assert_eq!(rec.calls.len(), 2);
        assert_eq!(rec.calls[0].0, "get_app_state");
        assert_eq!(rec.calls[1].0, "execute");
        assert_eq!(rec.calls[1].1["description"], "Add frame");
        assert_eq!(rec.calls[1].2, "toolu_A");
        assert!(rec.abandoned.is_empty());
    }

    let reqs = h.server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3, "529, retry, follow-up");
    let r0 = &reqs[0];
    assert_eq!(r0.headers.get("x-api-key").unwrap(), "test-key-123");
    assert_eq!(r0.headers.get("anthropic-version").unwrap(), "2026-10-21");
    assert_eq!(
        r0.headers.get("anthropic-beta").unwrap(),
        "server-side-fallback-2026-07-01"
    );
    let b = bodies(&reqs);
    assert_eq!(b[0], b[1], "a retry resends the same body");
    let first = &b[0];
    assert_eq!(first["model"], "claude-opus-5-5");
    assert_eq!(first["max_tokens"], 64000);
    assert_eq!(first["stream"], true);
    assert_eq!(
        first["thinking"],
        json!({"type":"adaptive","display":"summarized"})
    );
    assert_eq!(first["output_config"], json!({"effort":"low"}));
    assert_eq!(first["tool_choice"], json!({"type":"auto"}));
    assert_eq!(first["fallbacks"], "default");
    assert_eq!(
        first["system"][0]["cache_control"],
        json!({"type":"ephemeral"})
    );
    let names: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "start_task",
            "read_skill",
            "get_app_state",
            "execute",
            "save"
        ]
    );
    assert!(
        first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["eager_input_streaming"] == true)
    );
    // First user message: prefetched editor state, then the text (with the
    // conversation breakpoint on the last block).
    let content = first["messages"][0]["content"].as_array().unwrap();
    assert!(
        content[0]["text"]
            .as_str()
            .unwrap()
            .contains("<editor_state>\nFocused document: demo.pen")
    );
    assert_eq!(content[1]["text"], "Add a frame");
    assert_eq!(content[1]["cache_control"], json!({"type":"ephemeral"}));
    assert!(content[0].get("cache_control").is_none());

    let last = &b[2];
    let msgs = last["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    // The breakpoint moved: only the newest block carries it.
    assert!(msgs[0]["content"][1].get("cache_control").is_none());
    // Thinking passed back unchanged, with its signature.
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(
        msgs[1]["content"][0],
        json!({"type":"thinking","thinking":"I will add a frame.","signature":"SIG"})
    );
    assert_eq!(msgs[1]["content"][1]["input"]["description"], "Add frame");
    let result = &msgs[2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["tool_use_id"], "toolu_A");
    assert_eq!(result["content"][0]["text"], "Created frame f1");
    assert_eq!(result["content"][1]["type"], "image");
    assert_eq!(result["content"][1]["source"]["media_type"], "image/png");
    assert_eq!(result["cache_control"], json!({"type":"ephemeral"}));
    assert!(result.get("is_error").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_tool_json_is_returned_as_error() {
    let mut h = harness(Duration::ZERO, false).await;
    // Unescaped inner quote: the accumulated input is not valid JSON.
    let bad = ["{\"description\": \"x\", \"input\": \"Print(\"oops\")\"}"];
    script(
        &h.server,
        vec![
            sse_response(tool_turn("toolu_B", &bad, "tool_use")),
            sse_response(text_turn("Fixed.", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert!(ev.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { id, ok: false, text, .. }
        if id == "toolu_B" && text.contains("INVALID_JSON"))
    ));
    {
        let rec = h.rec.lock().unwrap();
        assert!(rec.calls.is_empty(), "never run invalid input");
        assert_eq!(rec.abandoned, ["toolu_B"]);
    }
    let b = bodies(&h.server.received_requests().await.unwrap());
    let result = &b[1]["messages"][2]["content"][0];
    assert_eq!(result["is_error"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["INVALID_JSON"], bad[0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_violation_is_not_run() {
    let mut h = harness(Duration::ZERO, false).await;
    let no_desc = ["{\"input\": \"Print(1)\"}"];
    script(
        &h.server,
        vec![
            sse_response(tool_turn("toolu_C", &no_desc, "tool_use")),
            sse_response(text_turn("ok", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert!(ev.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { ok: false, text, .. }
        if text.contains("description"))
    ));
    assert!(h.rec.lock().unwrap().calls.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_request_is_not_retried() {
    let mut h = harness(Duration::ZERO, false).await;
    script(
        &h.server,
        vec![ResponseTemplate::new(400).set_body_string(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad field"}}"#,
        )],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert!(ev.iter().any(
        |e| matches!(e, AgentEvent::Error(m) if m.contains("400") && m.contains("bad field"))
    ));
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "error".into()
        })
    );
    assert_eq!(h.server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_are_bounded() {
    let mut h = harness(Duration::ZERO, false).await;
    script(
        &h.server,
        vec![ResponseTemplate::new(500).set_body_string("{}")],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert!(
        ev.iter()
            .any(|e| matches!(e, AgentEvent::Error(m) if m.contains("500")))
    );
    assert_eq!(
        h.server.received_requests().await.unwrap().len(),
        3,
        "1 + max_retries"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overloaded_mid_stream_before_output_is_retried() {
    let mut h = harness(Duration::ZERO, false).await;
    let overloaded =
        sse(&[json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}})]);
    script(
        &h.server,
        vec![
            sse_response(overloaded),
            sse_response(text_turn("Hi", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "end_turn".into()
        })
    );
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, AgentEvent::TextDelta(_)))
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_tokens_tool_call_is_not_run() {
    let mut h = harness(Duration::ZERO, false).await;
    // A cut-off input that still parses.
    let cut = ["{\"description\": \"x\", \"input\": \"Print(1)\"}"];
    script(
        &h.server,
        vec![
            sse_response(tool_turn("toolu_D", &cut, "max_tokens")),
            sse_response(text_turn("ok", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "max_tokens".into()
        })
    );
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Error(_))));
    assert!(h.rec.lock().unwrap().calls.is_empty());
    assert_eq!(h.rec.lock().unwrap().abandoned, ["toolu_D"]);
    // The next message carries the missing tool_result first.
    h.session.send(UserMessage::text("try smaller")).unwrap();
    until(&mut h.rx, turn_end).await;
    let b = bodies(&h.server.received_requests().await.unwrap());
    let user = &b[1]["messages"][2];
    assert_eq!(user["role"], "user");
    assert_eq!(user["content"][0]["type"], "tool_result");
    assert_eq!(user["content"][0]["tool_use_id"], "toolu_D");
    assert_eq!(user["content"][0]["is_error"], true);
    assert_eq!(user["content"][1]["text"], "try smaller");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusal_runs_no_tools_and_keeps_history_valid() {
    let mut h = harness(Duration::ZERO, false).await;
    script(
        &h.server,
        vec![
            sse_response(tool_turn("toolu_E", &GOOD_CHUNKS, "refusal")),
            sse_response(text_turn("ok", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "refusal".into()
        })
    );
    assert!(h.rec.lock().unwrap().calls.is_empty());
    h.session.send(UserMessage::text("something else")).unwrap();
    until(&mut h.rx, turn_end).await;
    let b = bodies(&h.server.received_requests().await.unwrap());
    let msgs = b[1]["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["role"], "assistant");
    assert!(
        msgs[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["type"] == "text")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_tool_call() {
    let mut h = harness(Duration::from_secs(30), false).await;
    script(
        &h.server,
        vec![
            sse_response(tool_turn("toolu_F", &GOOD_CHUNKS, "tool_use")),
            sse_response(text_turn("ok", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    until(&mut h.rx, |e| matches!(e, AgentEvent::ToolUseEnd { .. })).await;
    // Let the executor start.
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.session.stop();
    let ev = until(&mut h.rx, turn_end).await;
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "interrupted".into()
        })
    );
    h.session.send(UserMessage::text("next")).unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "end_turn".into()
        })
    );
    let b = bodies(&h.server.received_requests().await.unwrap());
    let user = &b[1]["messages"][2];
    assert_eq!(user["content"][0]["tool_use_id"], "toolu_F");
    assert!(
        user["content"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("stopped")
    );
    assert_eq!(user["content"][1]["text"], "next");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_lands_after_tool_results() {
    let mut h = harness(Duration::from_millis(400), false).await;
    script(
        &h.server,
        vec![
            sse_response(tool_turn("toolu_G", &GOOD_CHUNKS, "tool_use")),
            sse_response(text_turn("ok", "end_turn")),
        ],
    )
    .await;
    h.session.send(UserMessage::text("go")).unwrap();
    until(&mut h.rx, |e| matches!(e, AgentEvent::ToolUseEnd { .. })).await;
    h.session.steer("make it blue").unwrap();
    let ev = until(&mut h.rx, turn_end).await;
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "end_turn".into()
        })
    );
    let b = bodies(&h.server.received_requests().await.unwrap());
    assert_eq!(b.len(), 2);
    let user = &b[1]["messages"][2]["content"];
    assert_eq!(user[0]["type"], "tool_result");
    assert_eq!(user[1]["text"], "make it blue");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_messages_run_in_order() {
    let mut h = harness(Duration::ZERO, false).await;
    script(&h.server, vec![sse_response(text_turn("ok", "end_turn"))]).await;
    h.session.send(UserMessage::text("one")).unwrap();
    h.session.send(UserMessage::text("two")).unwrap();
    until(&mut h.rx, turn_end).await;
    until(&mut h.rx, turn_end).await;
    let b = bodies(&h.server.received_requests().await.unwrap());
    assert_eq!(b.len(), 2);
    let msgs = b[1]["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2]["content"][0]["text"], "two");
}
