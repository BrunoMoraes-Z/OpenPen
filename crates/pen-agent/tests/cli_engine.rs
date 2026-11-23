//! The Claude Code engine against a fake `claude` that replays a transcript
//! recorded from the real CLI (`tests/fixtures/claude-cli-execute.jsonl`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pen_agent::cli::CliConfig;
use pen_agent::{
    AgentError, AgentEvent, Effort, EngineConfig, EventReceiver, Session, SessionConfig,
    UserMessage, start_session,
};

const SID: &str = "00000000-0000-4000-8000-000000000001";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-cli-execute.jsonl")
}

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("pen-agent-cli")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn start(name: &str, mode: &str) -> (Session, EventReceiver, PathBuf) {
    let dir = scratch(name);
    let log = dir.join("log.txt");
    let mut cli = CliConfig::new("C:/opt/openpen-cli.exe");
    cli.claude_path = Some(PathBuf::from(env!("CARGO_BIN_EXE_pen-agent-fake-claude")));
    cli.cwd = Some(dir.clone());
    cli.env = vec![
        (
            "PEN_AGENT_FAKE_TRANSCRIPT".into(),
            fixture().display().to_string(),
        ),
        ("PEN_AGENT_FAKE_LOG".into(), log.display().to_string()),
        ("PEN_AGENT_FAKE_MODE".into(), mode.into()),
    ];
    let mut s = start_session(SessionConfig {
        engine: EngineConfig::ClaudeCli(cli),
        system_prompt: "SYSTEM PROMPT\nline two".into(),
        model: Some("haiku".into()),
        effort: Effort::Low,
        resume: None,
    })
    .unwrap();
    let rx = s.take_events().unwrap();
    (s, rx, log)
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

fn turn_end(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::TurnEnd { .. })
}

fn spawned_args(log: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with('['))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

#[test]
fn replays_a_turn_and_keeps_the_process() {
    let (s, mut rx, log) = start("replay", "replay");
    assert!(s.agent_name().unwrap().starts_with("OpenPen Chat#"));
    s.send(UserMessage::text("draw")).unwrap();
    let ev = until(&mut rx, turn_end);
    assert_eq!(ev[0], AgentEvent::TurnStart);
    assert_eq!(ev[1], AgentEvent::SessionId(SID.into()));
    let kinds: Vec<&str> = ev
        .iter()
        .map(|e| match e {
            AgentEvent::TurnStart => "start",
            AgentEvent::SessionId(_) => "sid",
            AgentEvent::TextDelta(_) => "text",
            AgentEvent::ThinkingDelta(_) => "thinking",
            AgentEvent::ToolUseStart { .. } => "tool_start",
            AgentEvent::ToolInputDelta { .. } => "tool_delta",
            AgentEvent::ToolUseEnd { .. } => "tool_end",
            AgentEvent::ToolResult { .. } => "tool_result",
            AgentEvent::Usage(_) => "usage",
            AgentEvent::TurnEnd { .. } => "end",
            AgentEvent::Error(_) => "error",
        })
        .collect();
    let mut dedup = kinds.clone();
    dedup.dedup();
    assert_eq!(
        dedup,
        [
            "start",
            "sid",
            "tool_start",
            "tool_delta",
            "tool_end",
            "tool_result",
            "text",
            "usage",
            "end"
        ]
    );
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "end_turn".into()
        })
    );
    assert_eq!(s.session_id().as_deref(), Some(SID));

    // While the session lives, its temp files exist and say what we expect.
    let args = &spawned_args(&log)[0];
    let mcp = std::fs::read_to_string(arg_after(args, "--mcp-config").unwrap()).unwrap();
    let mcp: serde_json::Value = serde_json::from_str(&mcp).unwrap();
    assert_eq!(
        mcp["mcpServers"]["openpen"]["command"],
        "C:/opt/openpen-cli.exe"
    );
    assert_eq!(
        mcp["mcpServers"]["openpen"]["args"],
        serde_json::json!(["mcp", "--agent", s.agent_name().unwrap()])
    );
    let prompt =
        std::fs::read_to_string(arg_after(args, "--append-system-prompt-file").unwrap()).unwrap();
    assert_eq!(prompt, "SYSTEM PROMPT\nline two");
    assert_eq!(arg_after(args, "--model"), Some("haiku"));
    assert_eq!(arg_after(args, "--effort"), Some("low"));
    assert!(arg_after(args, "--resume").is_none());

    // Second turn: same process, no new spawn.
    s.send(UserMessage::text("again")).unwrap();
    let ev = until(&mut rx, turn_end);
    assert_eq!(ev[0], AgentEvent::TurnStart);
    assert!(
        ev.iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { ok: true, .. }))
    );
    assert_eq!(spawned_args(&log).len(), 1);
    let stdin: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(str::to_string)
        .collect();
    assert_eq!(stdin.len(), 2);
    let first: serde_json::Value = serde_json::from_str(&stdin[0]).unwrap();
    assert_eq!(first["message"]["content"][0]["text"], "draw");

    let mcp_path = PathBuf::from(arg_after(args, "--mcp-config").unwrap());
    drop(s);
    assert!(
        !mcp_path.exists(),
        "temp files are removed with the session"
    );
}

#[test]
fn stop_kills_and_next_message_resumes() {
    let (s, mut rx, log) = start("stop", "hang");
    s.send(UserMessage::text("draw")).unwrap();
    until(&mut rx, |e| matches!(e, AgentEvent::ToolInputDelta { .. }));
    let t = Instant::now();
    s.stop();
    let ev = until(&mut rx, turn_end);
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "interrupted".into()
        })
    );
    assert!(t.elapsed() < Duration::from_secs(5));
    s.send(UserMessage::text("go on")).unwrap();
    until(&mut rx, |e| matches!(e, AgentEvent::ToolInputDelta { .. }));
    let spawns = spawned_args(&log);
    assert_eq!(spawns.len(), 2);
    assert_eq!(arg_after(&spawns[1], "--resume"), Some(SID));
}

#[test]
fn process_exit_between_turns_respawns_with_resume() {
    let (s, mut rx, log) = start("exit", "exit_after_turn");
    s.send(UserMessage::text("one")).unwrap();
    until(&mut rx, turn_end);
    // Give the fake time to exit.
    std::thread::sleep(Duration::from_millis(300));
    s.send(UserMessage::text("two")).unwrap();
    let ev = until(&mut rx, turn_end);
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "end_turn".into()
        })
    );
    let spawns = spawned_args(&log);
    assert_eq!(spawns.len(), 2);
    assert_eq!(arg_after(&spawns[1], "--resume"), Some(SID));
}

#[test]
fn crash_reports_stderr() {
    let (s, mut rx, _) = start("crash", "crash");
    s.send(UserMessage::text("x")).unwrap();
    let ev = until(&mut rx, turn_end);
    assert!(
        ev.iter().any(|e| matches!(e, AgentEvent::Error(m) if m.contains("something broke") && m.contains("exit code 3"))),
        "{ev:#?}"
    );
    assert_eq!(
        ev.last(),
        Some(&AgentEvent::TurnEnd {
            stop_reason: "error".into()
        })
    );
}

#[test]
fn missing_claude_is_reported() {
    let mut cli = CliConfig::new("openpen-cli");
    cli.claude_path = Some(PathBuf::from("Z:/definitely/not/claude.exe"));
    let r = start_session(SessionConfig {
        engine: EngineConfig::ClaudeCli(cli),
        system_prompt: String::new(),
        model: None,
        effort: Effort::Low,
        resume: None,
    });
    assert_eq!(r.err(), Some(AgentError::ClaudeNotFound));
}
