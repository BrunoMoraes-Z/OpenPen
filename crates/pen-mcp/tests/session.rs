//! Full MCP sessions against a mock backend, in process over a duplex pipe.

mod common;

use std::sync::Arc;

use common::{Event, MockBackend, PNG};
use pen_mcp::{McpOptions, ToolBackend};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ErrorCode,
    Implementation, JsonObject,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ServiceError, ServiceExt as _};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ClientConfig>;

async fn start(opts: McpOptions) -> (Client, Arc<MockBackend>, tokio::task::JoinHandle<()>) {
    let backend = MockBackend::new();
    let (client_io, server_io) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_io);
    let b: Arc<dyn ToolBackend> = backend.clone();
    let server = tokio::spawn(async move {
        let _ = pen_mcp::serve_io(b, opts, "test-1", sr, sw).await;
    });
    let config = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "9.9.9"),
    );
    let client = config.serve(client_io).await.expect("client handshake");
    (client, backend, server)
}

fn args(v: Value) -> JsonObject {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

async fn call(
    client: &Client,
    name: &'static str,
    v: Value,
) -> Result<CallToolResult, ServiceError> {
    client
        .call_tool(CallToolRequestParams::new(name).with_arguments(args(v)))
        .await
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn error_code(e: ServiceError) -> i32 {
    match e {
        ServiceError::McpError(e) => e.code.0,
        other => panic!("expected a JSON-RPC error, got {other:?}"),
    }
}

#[tokio::test]
async fn initialize_and_list_tools() {
    let (client, _backend, _server) = start(McpOptions::default()).await;
    let info = client.peer_info().expect("server info");
    assert_eq!(
        info.server_info.as_ref().map(|s| s.name.as_str()),
        Some("openpen")
    );
    let instructions = info.instructions.clone().unwrap_or_default();
    assert!(instructions.contains("execute"));
    assert!(!instructions.to_lowercase().contains("encrypt"));

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    for n in [
        "start_task",
        "read_skill",
        "get_app_state",
        "execute",
        "save",
        "get_style",
    ] {
        assert!(names.contains(&n.to_string()), "missing {n}: {names:?}");
    }
    let exec = tools.iter().find(|t| t.name == "execute").unwrap();
    let schema = Value::Object(exec.input_schema.as_ref().clone());
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["description"]));
    assert!(schema["properties"]["editId"]["description"].is_string());
    assert!(
        schema["properties"]["uri"]["description"]
            .as_str()
            .unwrap()
            .contains("file://")
    );
    let edits = &schema["properties"]["edits"];
    assert_eq!(edits["type"], "array");
    let annotations = exec.annotations.as_ref().unwrap();
    assert_eq!(annotations.destructive_hint, Some(true));

    let start = tools.iter().find(|t| t.name == "start_task").unwrap();
    let schema = Value::Object(start.input_schema.as_ref().clone());
    assert_eq!(schema["required"], json!(["skills"]));
    assert!(schema["properties"].get("uri").is_none());
    for t in &tools {
        if !["read_skill", "start_task"].contains(&t.name.as_ref()) {
            assert!(
                t.input_schema["properties"].get("uri").is_some(),
                "{} lacks uri",
                t.name
            );
        }
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn save_can_be_disabled() {
    let opts = McpOptions {
        enable_save: false,
        ..McpOptions::default()
    };
    let (client, _backend, _server) = start(opts).await;
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().all(|t| t.name != "save"));
    let err = call(&client, "save", json!({})).await.unwrap_err();
    assert_eq!(error_code(err), ErrorCode::INVALID_PARAMS.0);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn backend_tools_receive_ctx_and_args() {
    let (client, backend, server) = start(McpOptions::default()).await;

    let r = call(&client, "get_app_state", json!({})).await.unwrap();
    assert_eq!(r.is_error, Some(false));
    assert!(text(&r).contains("Agent: claude-code"));

    let r = call(
        &client,
        "execute",
        json!({"uri": "file:///demo.pen", "description": "Add hero", "input": "Insert(document, {type: 'frame'})"}),
    )
    .await
    .unwrap();
    assert_eq!(text(&r), "ran: Add hero");

    let r = call(
        &client,
        "execute",
        json!({"description": "Fix hero", "editId": "e1", "edits": [{"find": "a", "replace": "b", "all": true}]}),
    )
    .await
    .unwrap();
    assert_eq!(text(&r), "ran: Fix hero");

    let r = call(&client, "save", json!({"uri": "C:/x/demo.pen"}))
        .await
        .unwrap();
    assert_eq!(text(&r), "saved");

    let r = call(&client, "get_style", json!({"name": "Bento"}))
        .await
        .unwrap();
    assert!(text(&r).contains("not available"));

    client.cancel().await.unwrap();
    server.await.unwrap();

    let events = backend.events();
    let ctx = match &events[0] {
        Event::Connected(ctx) => ctx.clone(),
        e => panic!("first event should be Connected, got {e:?}"),
    };
    assert_eq!(ctx.client_id, "test-1");
    assert_eq!(ctx.agent_name, "claude-code");
    assert_eq!(events[1], Event::AppState(ctx.clone(), None));
    match &events[2] {
        Event::Execute(c, a) => {
            assert_eq!(c, &ctx);
            assert_eq!(a.uri.as_deref(), Some("file:///demo.pen"));
            assert!(a.input.as_deref().unwrap().starts_with("Insert"));
        }
        e => panic!("{e:?}"),
    }
    match &events[3] {
        Event::Execute(_, a) => {
            assert_eq!(a.edit_id.as_deref(), Some("e1"));
            assert!(a.edits[0].all);
        }
        e => panic!("{e:?}"),
    }
    assert_eq!(
        events[4],
        Event::Save(ctx.clone(), Some("C:/x/demo.pen".into()))
    );
    assert_eq!(events.last(), Some(&Event::Disconnected(ctx)));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::Connected(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn error_mapping() {
    let (client, _backend, _server) = start(McpOptions::default()).await;

    // Script error reported by the backend as a tool-level error.
    let r = call(
        &client,
        "execute",
        json!({"description": "x", "input": "throw"}),
    )
    .await
    .unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("editId: e1"));

    // ToolError::Failed / NoDocument -> isError result.
    let r = call(
        &client,
        "execute",
        json!({"description": "x", "input": "fail"}),
    )
    .await
    .unwrap();
    assert_eq!(
        (r.is_error, text(&r).as_str()),
        (Some(true), "script failed")
    );
    let r = call(&client, "get_app_state", json!({"uri": "missing.pen"}))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("no document"));

    // ToolError::InvalidParams -> -32602; Unavailable -> -32603.
    let e = call(
        &client,
        "execute",
        json!({"description": "x", "input": "invalid"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(e), ErrorCode::INVALID_PARAMS.0);
    let e = call(
        &client,
        "execute",
        json!({"description": "x", "input": "crash"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error_code(e), ErrorCode::INTERNAL_ERROR.0);

    // Schema violations and invalid combinations never reach the backend.
    let e = call(&client, "execute", json!({"input": "x"}))
        .await
        .unwrap_err();
    assert_eq!(error_code(e), ErrorCode::INVALID_PARAMS.0);
    let e = call(&client, "execute", json!({"description": "x"}))
        .await
        .unwrap_err();
    assert_eq!(error_code(e), ErrorCode::INVALID_PARAMS.0);
    let e = call(&client, "execute", json!({"description": "x", "input": "a", "editId": "e1", "edits": [{"find": "a", "replace": "b"}]}))
        .await
        .unwrap_err();
    assert_eq!(error_code(e), ErrorCode::INVALID_PARAMS.0);
    let e = call(&client, "no_such_tool", json!({})).await.unwrap_err();
    assert_eq!(error_code(e), ErrorCode::INVALID_PARAMS.0);

    client.cancel().await.unwrap();
}

#[tokio::test]
async fn images_are_base64_png() {
    use base64::Engine as _;
    let (client, _backend, _server) = start(McpOptions::default()).await;
    let r = call(
        &client,
        "execute",
        json!({"description": "Shoot", "input": "screenshot"}),
    )
    .await
    .unwrap();
    assert_eq!(r.content.len(), 3);
    assert_eq!(text(&r), "1 screenshot");
    for c in &r.content[1..] {
        let img = c.as_image().expect("image content");
        assert_eq!(img.mime_type, "image/png");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&img.data)
            .unwrap();
        assert_eq!(bytes, PNG);
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn skills_tools() {
    let dir = common::skills_dir("session");
    let opts = McpOptions {
        skills_dir: Some(dir),
        ..McpOptions::default()
    };
    let (client, backend, _server) = start(opts).await;

    let r = call(&client, "read_skill", json!({})).await.unwrap();
    assert_eq!(text(&r), "# OpenPen skills index");
    let r = call(&client, "read_skill", json!({"path": "execute.md"}))
        .await
        .unwrap();
    assert_eq!(text(&r), "execute API");

    for bad in [
        "../outside.txt",
        "..\\outside.txt",
        "/etc/passwd",
        "C:\\Windows\\win.ini",
    ] {
        let e = call(&client, "read_skill", json!({"path": bad}))
            .await
            .unwrap_err();
        assert_eq!(error_code(e), ErrorCode::INVALID_PARAMS.0, "{bad}");
    }
    let r = call(&client, "read_skill", json!({"path": "nope.md"}))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));

    let r = call(
        &client,
        "start_task",
        json!({"skills": ["execute.md", "pen-schema.md"], "title": "Pricing page"}),
    )
    .await
    .unwrap();
    let t = text(&r);
    assert!(t.starts_with("Task: Pricing page"));
    assert!(t.contains("execute API") && t.contains("schema docs"));
    assert!(t.find("execute API") < t.find("schema docs"));

    let e = call(&client, "start_task", json!({"skills": ["../outside.txt"]}))
        .await
        .unwrap_err();
    assert_eq!(error_code(e), ErrorCode::INVALID_PARAMS.0);

    // Skill tools do not touch the document backend.
    assert!(
        backend
            .events()
            .iter()
            .all(|e| matches!(e, Event::Connected(_)))
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn skills_missing_is_reported() {
    let (client, _backend, _server) = start(McpOptions::default()).await;
    let r = call(&client, "read_skill", json!({})).await.unwrap();
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("OPENPEN_SKILLS_DIR"));
    client.cancel().await.unwrap();
}
