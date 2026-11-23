//! Tests against the real services. They skip unless enabled:
//!
//! - `ANTHROPIC_API_KEY` set: a short two-turn API conversation that checks
//!   prompt caching (`cache_read_input_tokens > 0` on the second request).
//!   Model: `PEN_AGENT_TEST_MODEL` (default `claude-haiku-5-5`, cheapest).
//! - `PEN_AGENT_REAL_CLAUDE=1`: the Claude Code engine with the installed
//!   `claude`, against `openpen-cli mcp --headless` on a copy of a small
//!   document (`OPENPEN_CLI`, default `C:\drivers\pen\openpen-cli.exe`;
//!   `PEN_AGENT_TEST_PEN`, default `C:\drivers\pen\demo\type-specimen.pen`).
//!   Uses the logged-in Claude Code account.
//! - `PEN_AGENT_NET_TESTS=1`: TLS + error mapping against api.anthropic.com
//!   with a bogus key (expects 401, costs nothing).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pen_agent::api::ApiConfig;
use pen_agent::cli::CliConfig;
use pen_agent::key::ApiKey;
use pen_agent::{
    AgentEvent, BoxFuture, Effort, EngineConfig, EventReceiver, SessionConfig, ToolExecutor,
    ToolOutput, UserMessage, start_session,
};
use serde_json::Value;

struct Echo;
impl ToolExecutor for Echo {
    fn call(&self, name: &str, _input: Value, _id: &str) -> BoxFuture<'static, ToolOutput> {
        let name = name.to_string();
        Box::pin(async move { ToolOutput::text(format!("{name}: no document is open.")) })
    }
}

async fn until_end(rx: &mut EventReceiver, secs: u64) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(secs), async {
        while let Some(e) = rx.recv().await {
            let end = matches!(e, AgentEvent::TurnEnd { .. });
            out.push(e);
            if end {
                return;
            }
        }
    })
    .await;
    out
}

fn usage(ev: &[AgentEvent]) -> pen_agent::Usage {
    ev.iter()
        .find_map(|e| match e {
            AgentEvent::Usage(u) => Some(*u),
            _ => None,
        })
        .unwrap_or_default()
}

fn skill_prompt() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/openpen");
    let skill = pen_agent::prompt::load_skill_index(&dir).unwrap_or_default();
    pen_agent::prompt::chat_system_prompt(&skill)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_api_conversation_uses_the_cache() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        eprintln!("skipped: ANTHROPIC_API_KEY not set");
        return;
    };
    let model = std::env::var("PEN_AGENT_TEST_MODEL").unwrap_or_else(|_| "claude-haiku-5-5".into());
    let mut cfg = ApiConfig::new(Arc::new(Echo));
    cfg.api_key = Some(ApiKey::new(key));
    cfg.prefetch_app_state = false;
    cfg.max_tokens = 2000;
    let mut s = start_session(SessionConfig {
        engine: EngineConfig::Api(cfg),
        system_prompt: skill_prompt(),
        model: Some(model),
        effort: Effort::Low,
        resume: None,
    })
    .unwrap();
    let mut rx = s.take_events().unwrap();
    s.send(UserMessage::text("Reply with just the word: ready"))
        .unwrap();
    let first = until_end(&mut rx, 120).await;
    assert!(
        matches!(first.last(), Some(AgentEvent::TurnEnd { stop_reason }) if stop_reason == "end_turn"),
        "{first:#?}"
    );
    s.send(UserMessage::text("Now reply with just the word: done"))
        .unwrap();
    let second = until_end(&mut rx, 120).await;
    let u = usage(&second);
    eprintln!("first: {:?}\nsecond: {u:?}", usage(&first));
    assert!(
        u.cache_read > 0,
        "the stable prefix should be read from the cache: {u:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_api_tls_and_auth_error() {
    if std::env::var("PEN_AGENT_NET_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: PEN_AGENT_NET_TESTS != 1");
        return;
    }
    let mut cfg = ApiConfig::new(Arc::new(Echo));
    cfg.api_key = Some(ApiKey::new("sk-ant-invalid-test-key"));
    cfg.prefetch_app_state = false;
    let mut s = start_session(SessionConfig {
        engine: EngineConfig::Api(cfg),
        system_prompt: "x".into(),
        model: None,
        effort: Effort::Low,
        resume: None,
    })
    .unwrap();
    let mut rx = s.take_events().unwrap();
    s.send(UserMessage::text("hi")).unwrap();
    let ev = until_end(&mut rx, 60).await;
    assert!(
        ev.iter().any(|e| matches!(e, AgentEvent::Error(m) if m.contains("401") && !m.contains("sk-ant-invalid"))),
        "{ev:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_claude_cli_streams_execute() {
    if std::env::var("PEN_AGENT_REAL_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: PEN_AGENT_REAL_CLAUDE != 1");
        return;
    }
    let cli_exe =
        std::env::var("OPENPEN_CLI").unwrap_or_else(|_| r"C:\drivers\pen\openpen-cli.exe".into());
    let pen = std::env::var("PEN_AGENT_TEST_PEN")
        .unwrap_or_else(|_| r"C:\drivers\pen\demo\type-specimen.pen".into());
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pen-agent-real");
    std::fs::create_dir_all(&dir).unwrap();
    let doc = dir.join("doc.pen");
    std::fs::copy(&pen, &doc).unwrap();
    let mut cli = CliConfig::new(cli_exe);
    cli.mcp_extra_args = vec!["--headless".into(), doc.display().to_string()];
    cli.allow_web = false;
    let mut s = start_session(SessionConfig {
        engine: EngineConfig::ClaudeCli(cli),
        system_prompt: skill_prompt(),
        model: Some(std::env::var("PEN_AGENT_TEST_MODEL").unwrap_or_else(|_| "haiku".into())),
        effort: Effort::Low,
        resume: None,
    })
    .unwrap();
    let mut rx = s.take_events().unwrap();
    s.send(UserMessage::text(
        "Call the execute tool exactly once, with description \"Print a list\" and an input \
         script of 8 lines, each one Print(\"line N\") for N from 1 to 8, written out (no loop). \
         Do not call any other tool. Then reply with one short sentence.",
    ))
    .unwrap();
    let ev = until_end(&mut rx, 180).await;
    for e in &ev {
        eprintln!("{e:?}");
    }
    let deltas = ev
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolInputDelta { .. }))
        .count();
    assert!(
        deltas >= 2,
        "partial MCP tool input should stream ({deltas} deltas)"
    );
    assert!(ev.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { ok: true, text, .. } if text.contains("line 8"))
    ));
    assert!(
        matches!(ev.last(), Some(AgentEvent::TurnEnd { stop_reason }) if stop_reason == "end_turn")
    );
    assert!(s.session_id().is_some());
}
