//! `openpen-cli mcp` against an in-test socket server.

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pen_mcp::socket::SocketServer;
use pen_mcp::{BoxFuture, CallCtx, ExecuteArgs, McpOptions, ToolBackend, ToolOutput, ToolResult};
use serde_json::{Value, json};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_openpen-cli"))
}

#[derive(Default)]
struct Recorder {
    connected: Mutex<Vec<CallCtx>>,
    disconnected: Mutex<Vec<CallCtx>>,
}

impl ToolBackend for Recorder {
    fn get_app_state(&self, ctx: CallCtx, _uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        Box::pin(async move { Ok(ToolOutput::text(format!("hello {}", ctx.agent_name))) })
    }
    fn execute(&self, _ctx: CallCtx, args: ExecuteArgs) -> BoxFuture<'static, ToolResult> {
        Box::pin(async move { Ok(ToolOutput::text(args.description)) })
    }
    fn save(&self, _ctx: CallCtx, _uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        Box::pin(async { Ok(ToolOutput::text("saved")) })
    }
    fn agent_connected(&self, ctx: &CallCtx) {
        self.connected.lock().unwrap().push(ctx.clone());
    }
    fn agent_disconnected(&self, ctx: &CallCtx) {
        self.disconnected.lock().unwrap().push(ctx.clone());
    }
}

fn unique_socket(tag: &str) -> String {
    let name = format!("cli-{tag}-{}", std::process::id());
    if cfg!(unix) {
        std::env::temp_dir()
            .join(format!("{name}.sock"))
            .display()
            .to_string()
    } else {
        name
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shim_reaches_running_server() {
    let backend = Arc::new(Recorder::default());
    let name = unique_socket("shim");
    let b: Arc<dyn ToolBackend> = backend.clone();
    let server = SocketServer::bind(&name, b, McpOptions::default())
        .await
        .unwrap();

    let mut child = bin()
        .args(["mcp", "--socket", &name, "--agent", "cli-agent"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let exchange = tokio::task::spawn_blocking(move || {
        let mut send = |v: Value| {
            writeln!(stdin, "{v}").unwrap();
            stdin.flush().unwrap();
        };
        let mut recv = || {
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        };
        send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "claude-code", "version": "1"}}}),
        );
        let init = recv();
        send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let list = recv();
        send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "get_app_state", "arguments": {}}}));
        let state = recv();
        drop(stdin);
        (init, list, state)
    });
    let (init, list, state) = tokio::time::timeout(Duration::from_secs(20), exchange)
        .await
        .expect("no answer through the shim")
        .unwrap();

    assert_eq!(init["result"]["serverInfo"]["name"], "openpen");
    let tools = list["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "execute"));
    assert_eq!(
        state["result"]["content"][0]["text"], "hello cli-agent",
        "{state}"
    );

    let status = tokio::task::spawn_blocking(move || child.wait().unwrap())
        .await
        .unwrap();
    assert!(status.success());
    for _ in 0..200 {
        if !backend.disconnected.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let connected = backend.connected.lock().unwrap().clone();
    assert_eq!(connected.len(), 1);
    assert_eq!(connected[0].agent_name, "cli-agent");
    assert_eq!(*backend.disconnected.lock().unwrap(), connected);
    server.shutdown().await;
}

#[test]
fn shim_without_app_fails_clearly() {
    let out = bin()
        .args(["mcp", "--socket", &unique_socket("absent")])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("OpenPen is not running"), "{err}");
    assert!(out.stdout.is_empty());
}

#[test]
fn print_config_is_opt_in_snippet() {
    let out = bin().args(["mcp", "--print-config"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let entry = &v["mcpServers"]["openpen"];
    assert_eq!(entry["args"], json!(["mcp"]));
    assert!(entry["command"].as_str().unwrap().contains("openpen-cli"));
}
