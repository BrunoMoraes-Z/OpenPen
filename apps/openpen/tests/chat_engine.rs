//! The chat's view model driven by real engine output, without a window:
//!
//! - the recorded Claude Code transcript (`pen-agent`'s
//!   `claude-cli-execute.jsonl`) mapped by `CliMapper`, and the same through
//!   the fake `claude` binary and the real CLI engine;
//! - the API engine built by `chat::engine::session_config` against a mock
//!   Messages API, with the app's `GuiBackend` answered by a fake dispatcher:
//!   the partial hook streams `execute`, the final call carries the tool use
//!   id.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use openpen::agent::backend::{AgentRequest, GuiBackend};
use openpen::chat::engine::{EngineEnv, session_config};
use openpen::chat::model::{ChatModel, Effect, Item, ToolState};
use openpen::chat::settings::{ChatSettings, EngineKind};
use pen_agent::cli::CliMapper;
use pen_agent::{
    AgentEvent, EngineConfig, EventReceiver, SessionConfig, UserMessage, start_session,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/pen-agent/tests/fixtures/claude-cli-execute.jsonl")
}

/// Feeds events to a model; returns the stream partial prefixes emitted.
fn drive(m: &mut ChatModel, events: Vec<AgentEvent>) -> Vec<String> {
    let mut partials = Vec::new();
    for e in events {
        for fx in m.apply(e) {
            if let Effect::StreamPartial { prefix, .. } = fx {
                partials.push(prefix);
            }
        }
    }
    partials
}

/// What the recorded turn must produce in the panel.
fn check_recorded_turn(m: &ChatModel, partials: &[String]) {
    // Growing prefixes of the streamed script, each a prefix of the next.
    assert!(partials.len() >= 10, "{partials:?}");
    for w in partials.windows(2) {
        assert!(w[1].starts_with(&w[0]) && w[1].len() > w[0].len());
    }
    assert!(partials.last().unwrap().contains("Print(\"line 12\");"));
    let tools: Vec<_> = m
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Tool(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 1);
    let t = tools[0];
    assert_eq!(t.name, "execute");
    assert!(t.label().starts_with("Print a list"), "{}", t.label());
    assert_eq!(t.state, ToolState::Done);
    assert!(t.code.starts_with("Print(\"line 1\");\nPrint(\"line 2\");"));
    assert!(t.result.as_deref().unwrap().contains("line 12"));
    let text: String = m
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(text.contains("lines 1 through 12"), "{text}");
    match m.items.last() {
        Some(Item::TurnEnd { stop_reason, usage }) => {
            assert_eq!(stop_reason, "end_turn");
            let u = usage.expect("usage");
            assert!(u.cost_usd.unwrap() > 0.0);
            assert!(u.summary().contains('$'));
        }
        other => panic!("{other:?}"),
    }
    assert!(!m.running);
    assert_eq!(
        m.session_id.as_deref(),
        Some("00000000-0000-4000-8000-000000000001")
    );
}

#[test]
fn recorded_cli_transcript_builds_the_panel() {
    let text = std::fs::read_to_string(fixture()).unwrap();
    let mut mapper = CliMapper::new();
    let mut events = vec![AgentEvent::TurnStart];
    for line in text.lines() {
        events.extend(mapper.map_line(line));
    }
    let mut m = ChatModel::new(true);
    m.push_user("Print a list", false);
    let partials = drive(&mut m, events);
    check_recorded_turn(&m, &partials);
}

/// `pen-agent-fake-claude` next to this test's target dir (built by
/// `cargo test --workspace`); `None` when only this package was built.
fn fake_claude() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    let name = if cfg!(windows) {
        "pen-agent-fake-claude.exe"
    } else {
        "pen-agent-fake-claude"
    };
    Some(dir.join(name)).filter(|p| p.is_file())
}

fn until(rx: &mut EventReceiver, pred: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut out = Vec::new();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Some(e) => {
                let stop = pred(&e);
                out.push(e);
                if stop {
                    return out;
                }
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("timed out; got {out:#?}");
}

#[test]
fn fake_claude_cli_engine_builds_the_panel() {
    let Some(fake) = fake_claude() else {
        eprintln!("skipped: pen-agent-fake-claude not built (run cargo test --workspace)");
        return;
    };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chat-fake-claude");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The engine config the panel builds, pointed at the fake binary.
    let settings = ChatSettings {
        cli_model: "haiku".into(),
        ..ChatSettings::default()
    };
    let env = EngineEnv {
        backend: GuiBackend::new().0,
        skills_dir: None,
        cli_path: Some(PathBuf::from("openpen-cli")),
        socket: Some("desktop".into()),
        doc_uri: None,
        agent_id: "chat-test".into(),
        resume: None,
        api_base_url: None,
        api_key: None,
    };
    let mut cfg = session_config(&settings, &env).unwrap();
    let EngineConfig::ClaudeCli(cli) = &mut cfg.engine else {
        panic!("wrong engine");
    };
    cli.claude_path = Some(fake);
    cli.cwd = Some(dir.clone());
    cli.env = vec![(
        "PEN_AGENT_FAKE_TRANSCRIPT".into(),
        fixture().display().to_string(),
    )];
    let mut session = start_session(cfg).unwrap();
    assert!(session.agent_name().unwrap().starts_with("OpenPen Chat#"));
    let mut rx = session.take_events().unwrap();
    session
        .send(UserMessage {
            text: "Print a list".into(),
            attachments: Vec::new(),
            context: Some(pen_agent::prompt::app_state_context("Focused: demo.pen")),
        })
        .unwrap();
    let events = until(&mut rx, |e| matches!(e, AgentEvent::TurnEnd { .. }));
    let mut m = ChatModel::new(true);
    m.push_user("Print a list", false);
    let partials = drive(&mut m, events);
    check_recorded_turn(&m, &partials);
}

// ------------------------------------------------------------- API engine

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

fn start_msg() -> Value {
    json!({"type":"message_start","message":{"id":"msg","type":"message","role":"assistant","content":[],
        "usage":{"input_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":900,"output_tokens":1}}})
}

#[derive(Default, Debug)]
struct Seen {
    partials: Vec<(String, String, Option<String>, String)>,
    executes: Vec<(String, Option<String>, String)>,
    app_states: usize,
}

/// Answers the `GuiBackend` channel like the app's dispatcher would.
fn fake_dispatcher(mut rx: openpen::agent::backend::RequestRx, seen: Arc<Mutex<Seen>>) {
    std::thread::spawn(move || {
        futures::executor::block_on(async move {
            while let Some(req) = rx.next().await {
                let mut s = seen.lock().unwrap();
                match req {
                    AgentRequest::StreamPartial {
                        agent_id,
                        tool_use_id,
                        uri,
                        prefix,
                    } => s.partials.push((agent_id, tool_use_id, uri, prefix)),
                    AgentRequest::Execute { ctx, args, reply } => {
                        s.executes.push((
                            ctx.client_id,
                            ctx.tool_use_id,
                            args.input.unwrap_or_default(),
                        ));
                        let _ = reply.send(Ok(pen_mcp::ToolOutput::text("Created card c1")));
                    }
                    AgentRequest::AppState { reply, .. } => {
                        s.app_states += 1;
                        let _ = reply.send(Ok(pen_mcp::ToolOutput::text("Focused: demo.pen")));
                    }
                    AgentRequest::Save { reply, .. } => {
                        let _ = reply.send(Ok(pen_mcp::ToolOutput::text("saved")));
                    }
                    _ => {}
                }
            }
        });
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_engine_streams_execute_through_the_backend() {
    let server = MockServer::start().await;
    let chunks = [
        "{\"description\": \"Add a card\", \"input\": \"const c = Insert(document, {type: \\\"fra",
        "me\\\"});\\nSet(c, {name: \\\"Card\\\"});\\n",
        "Print(c)\"}",
    ];
    let mut tool = vec![
        start_msg(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_api_1","name":"execute","input":{}}}),
    ];
    for c in chunks {
        tool.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":c}}));
    }
    tool.push(json!({"type":"content_block_stop","index":0}));
    tool.push(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":40}}));
    tool.push(json!({"type":"message_stop"}));
    let text = sse(&[
        start_msg(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Added **a card**."}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(sse(&tool)))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse_response(text))
        .with_priority(2)
        .mount(&server)
        .await;

    let (backend, rx) = GuiBackend::new();
    let seen = Arc::new(Mutex::new(Seen::default()));
    fake_dispatcher(rx, seen.clone());
    let settings = ChatSettings {
        engine: EngineKind::AnthropicApi,
        api_model: "claude-haiku-5-5".into(),
        ..ChatSettings::default()
    };
    let env = EngineEnv {
        backend: backend.clone(),
        skills_dir: None,
        cli_path: None,
        socket: None,
        doc_uri: Some("C:/designs/demo.pen".into()),
        agent_id: "chat-api-1".into(),
        resume: Some("ignored".into()),
        api_base_url: Some(server.uri()),
        api_key: Some(pen_agent::key::ApiKey::new("test-key")),
    };
    let cfg: SessionConfig = session_config(&settings, &env).unwrap();
    assert_eq!(cfg.model.as_deref(), Some("claude-haiku-5-5"));
    let mut session = start_session(cfg).unwrap();
    let mut rx = session.take_events().unwrap();
    // The panel prefixes the first message with its document's state.
    session
        .send(UserMessage {
            text: "Add a card".into(),
            attachments: Vec::new(),
            context: Some(pen_agent::prompt::app_state_context("Focused: demo.pen")),
        })
        .unwrap();
    let mut events = Vec::new();
    let r = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(e) = rx.recv().await {
            let end = matches!(e, AgentEvent::TurnEnd { .. });
            events.push(e);
            if end {
                break;
            }
        }
    })
    .await;
    assert!(r.is_ok(), "timed out: {events:#?}");
    // The API engine streams through the hook, not through the reducer.
    let mut m = ChatModel::new(false);
    m.push_user("Add a card", false);
    assert!(drive(&mut m, events).is_empty());

    let s = seen.lock().unwrap();
    assert_eq!(s.app_states, 0, "no prefetch: the panel sent the context");
    assert!(!s.partials.is_empty(), "{s:?}");
    for (agent, tool, uri, _) in &s.partials {
        assert_eq!(agent, "chat-api-1");
        assert_eq!(tool, "toolu_api_1");
        assert_eq!(uri.as_deref(), Some("C:/designs/demo.pen"));
    }
    for w in s.partials.windows(2) {
        assert!(w[1].3.starts_with(&w[0].3));
    }
    assert_eq!(s.executes.len(), 1);
    let (client, tool_use_id, input) = &s.executes[0];
    assert_eq!(client, "chat-api-1");
    assert_eq!(tool_use_id.as_deref(), Some("toolu_api_1"));
    assert!(input.ends_with("Print(c)"));
    assert!(s.partials.last().unwrap().3.len() <= input.len());

    let t = m
        .items
        .iter()
        .find_map(|i| match i {
            Item::Tool(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!((t.label(), t.state), ("Add a card", ToolState::Done));
    assert_eq!(t.result.as_deref(), Some("Created card c1"));
    assert!(
        m.items
            .iter()
            .any(|i| matches!(i, Item::Text { text } if text == "Added **a card**."))
    );
    match m.items.last() {
        Some(Item::TurnEnd { usage: Some(u), .. }) => {
            assert_eq!(u.output_tokens, 45);
            assert_eq!(u.cost_usd, None, "the API reports no cost");
        }
        other => panic!("{other:?}"),
    }
}
