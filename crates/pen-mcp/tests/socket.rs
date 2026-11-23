//! Socket server + shim end to end, each test on its own pipe/socket name.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Event, MockBackend};
use interprocess::local_socket::tokio::Stream as LocalStream;
use interprocess::local_socket::traits::tokio::Stream as _;
use interprocess::local_socket::{GenericFilePath, ToFsName as _};
use pen_mcp::socket::{self, ShimError, SocketServer};
use pen_mcp::{CallCtx, McpOptions, ToolBackend};
use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

async fn bind(tag: &str) -> (SocketServer, Arc<MockBackend>, String) {
    let backend = MockBackend::new();
    let name = common::unique_socket(tag);
    let b: Arc<dyn ToolBackend> = backend.clone();
    let server = SocketServer::bind(&name, b, McpOptions::default())
        .await
        .expect("bind");
    (server, backend, name)
}

async fn raw_connect(name: &str) -> LocalStream {
    let path = socket::socket_path(name);
    LocalStream::connect(path.to_fs_name::<GenericFilePath>().unwrap())
        .await
        .expect("connect")
}

async fn wait_for(
    backend: &MockBackend,
    what: &str,
    pred: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    for _ in 0..200 {
        let ev = backend.events();
        if pred(&ev) {
            return ev;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}: {:?}", backend.events());
}

fn connected(ev: &[Event]) -> Vec<CallCtx> {
    ev.iter()
        .filter_map(|e| match e {
            Event::Connected(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

fn disconnected(ev: &[Event]) -> Vec<CallCtx> {
    ev.iter()
        .filter_map(|e| match e {
            Event::Disconnected(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_concurrent_clients() {
    let (server, backend, name) = bind("two").await;

    // Client A announces itself with a hello line (as the shim does).
    let mut a_stream = raw_connect(&name).await;
    a_stream
        .write_all(socket::hello_line(Some("codex")).as_bytes())
        .await
        .unwrap();
    let a = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("codex-mcp-client", "1"),
    )
    .serve(a_stream)
    .await
    .unwrap();
    // Client B speaks plain MCP; its name comes from clientInfo.
    let b = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("claude-code", "1"),
    )
    .serve(raw_connect(&name).await)
    .await
    .unwrap();

    let (ra, rb) = tokio::join!(
        a.call_tool(CallToolRequestParams::new("get_app_state")),
        b.call_tool(CallToolRequestParams::new("get_app_state")),
    );
    let text = |r: rmcp::model::CallToolResult| r.content[0].as_text().unwrap().text.clone();
    assert!(text(ra.unwrap()).contains("Agent: codex"));
    assert!(text(rb.unwrap()).contains("Agent: claude-code"));

    let ev = backend.events();
    let conns = connected(&ev);
    assert_eq!(conns.len(), 2);
    assert_ne!(conns[0].client_id, conns[1].client_id);
    let a_ctx = conns
        .iter()
        .find(|c| c.agent_name == "codex")
        .unwrap()
        .clone();
    let b_ctx = conns
        .iter()
        .find(|c| c.agent_name == "claude-code")
        .unwrap()
        .clone();

    // Closing A reports only A.
    a.cancel().await.unwrap();
    let ev = wait_for(&backend, "A disconnect", |ev| !disconnected(ev).is_empty()).await;
    assert_eq!(disconnected(&ev), vec![a_ctx]);

    // B still works.
    let r = b
        .call_tool(CallToolRequestParams::new("get_style"))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(false));
    b.cancel().await.unwrap();
    let ev = wait_for(&backend, "B disconnect", |ev| disconnected(ev).len() == 2).await;
    assert_eq!(disconnected(&ev)[1], b_ctx);

    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_server_on_same_name_fails() {
    let (server, _backend, name) = bind("dup").await;
    let b: Arc<dyn ToolBackend> = MockBackend::new();
    assert!(
        SocketServer::bind(&name, b, McpOptions::default())
            .await
            .is_err()
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_sessions() {
    let (server, backend, name) = bind("shutdown").await;
    let client = ClientConfig::new(ClientCapabilities::default(), Implementation::new("x", "1"))
        .serve(raw_connect(&name).await)
        .await
        .unwrap();
    assert_eq!(connected(&backend.events()).len(), 1);
    tokio::time::timeout(Duration::from_secs(5), server.shutdown())
        .await
        .expect("shutdown hangs");
    assert_eq!(disconnected(&backend.events()).len(), 1);
    drop(client);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shim_pumps_stdio() {
    let (server, backend, name) = bind("shim").await;

    let (mut stdin_w, stdin_r) = tokio::io::duplex(1 << 16);
    let (stdout_w, stdout_r) = tokio::io::duplex(1 << 16);
    let shim_name = name.clone();
    let shim = tokio::spawn(async move {
        socket::run_shim(&shim_name, Some("shim-agent"), stdin_r, stdout_w).await
    });
    let mut out = BufReader::new(stdout_r).lines();

    let mut send = async |v: Value| {
        let mut line = v.to_string();
        line.push('\n');
        stdin_w.write_all(line.as_bytes()).await.unwrap();
    };
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "raw-client", "version": "1"}}}),
    )
    .await;
    let init: Value = serde_json::from_str(&out.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "openpen");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
    send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "get_app_state", "arguments": {}}}),
    )
    .await;
    let resp: Value = serde_json::from_str(&out.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(resp["id"], 2);
    assert!(
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Agent: shim-agent")
    );

    // Closing stdin ends the shim and the session.
    drop(send);
    drop(stdin_w);
    tokio::time::timeout(Duration::from_secs(5), shim)
        .await
        .expect("shim did not exit")
        .unwrap()
        .unwrap();
    wait_for(&backend, "shim disconnect", |ev| {
        disconnected(ev).len() == 1
    })
    .await;
    server.shutdown().await;
}

#[tokio::test]
async fn shim_reports_not_running() {
    let name = common::unique_socket("absent");
    let err = socket::run_shim(&name, None, tokio::io::empty(), tokio::io::sink())
        .await
        .unwrap_err();
    assert!(matches!(err, ShimError::NotRunning { .. }));
    assert!(err.to_string().contains("OpenPen is not running"));
}
