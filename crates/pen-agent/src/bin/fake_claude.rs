//! Test helper standing in for the `claude` CLI: replays a recorded
//! stream-json transcript for every user message read from stdin.
//!
//! Environment:
//! - `PEN_AGENT_FAKE_TRANSCRIPT`: transcript to replay (one JSON per line);
//! - `PEN_AGENT_FAKE_LOG`: file that receives the arguments (first line, as
//!   a JSON array) and every stdin line;
//! - `PEN_AGENT_FAKE_MODE`: `replay` (default), `hang` (stops after the first
//!   `input_json_delta` and waits), `crash` (exits with an error at once) or
//!   `exit_after_turn` (replays one turn, then exits).

use std::io::{BufRead, Write};

fn main() {
    let mode = std::env::var("PEN_AGENT_FAKE_MODE").unwrap_or_else(|_| "replay".into());
    let mut log = std::env::var_os("PEN_AGENT_FAKE_LOG").and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(l) = log.as_mut() {
        let quoted: Vec<String> = args
            .iter()
            .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        let _ = writeln!(l, "[{}]", quoted.join(","));
    }
    if mode == "crash" {
        eprintln!("fatal: something broke");
        std::process::exit(3);
    }
    let transcript = std::env::var_os("PEN_AGENT_FAKE_TRANSCRIPT")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if let Some(l) = log.as_mut() {
            let _ = writeln!(l, "{line}");
        }
        if !line.contains("\"type\":\"user\"") {
            continue;
        }
        for t in transcript.lines() {
            let _ = writeln!(out, "{t}");
            let _ = out.flush();
            if mode == "hang" && t.contains("input_json_delta") && t.contains("Print") {
                std::thread::sleep(std::time::Duration::from_secs(60));
                return;
            }
        }
        if mode == "exit_after_turn" {
            return;
        }
    }
}
