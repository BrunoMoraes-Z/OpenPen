//! End to end: the real `openpen-cli mcp --headless FILE` binary driven over
//! stdio with MCP JSON-RPC, like Claude Code would.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A scratch directory under `target/` (never inside `testdata/`).
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/headless-e2e")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const FIXTURE: &str = r##"{
  "version": "2.22",
  "variables": {"accent": {"type": "color", "value": "#2563EB"}},
  "children": [
    {"type": "frame", "id": "intro", "name": "Intro", "x": 0, "y": 0, "width": 200, "height": 120, "fill": "#F4F4F5"}
  ]
}"##;

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    next_id: u64,
}

impl Client {
    fn spawn(file: &Path, extra: &[&str]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_openpen-cli"))
            .arg("mcp")
            .arg("--headless")
            .arg(file)
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        let mut c = Client {
            child,
            stdin,
            lines: rx,
            next_id: 1,
        };
        let init = c.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "e2e-agent", "version": "1"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "openpen", "{init}");
        c.notify("notifications/initialized");
        c
    }

    fn send(&mut self, v: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        self.send(json!({"jsonrpc": "2.0", "method": method}));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// `tools/call`; returns `(text, images, is_error)`.
    fn call(&mut self, tool: &str, args: Value) -> (String, Vec<Vec<u8>>, bool) {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        let res = &r["result"];
        assert!(res.is_object(), "{tool}: {r}");
        let mut text = String::new();
        let mut images = Vec::new();
        for c in res["content"].as_array().unwrap() {
            match c["type"].as_str() {
                Some("text") => text.push_str(c["text"].as_str().unwrap()),
                Some("image") => {
                    assert_eq!(c["mimeType"], "image/png");
                    images.push(decode_b64(c["data"].as_str().unwrap()));
                }
                _ => {}
            }
        }
        (text, images, res["isError"] == json!(true))
    }

    fn execute(&mut self, description: &str, input: &str) -> (String, Vec<Vec<u8>>, bool) {
        self.call(
            "execute",
            json!({"description": description, "input": input}),
        )
    }

    fn close(mut self) {
        drop(self.stdin.take());
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "exit status {status}");
                return;
            }
            if started.elapsed() > Duration::from_secs(20) {
                let _ = self.child.kill();
                panic!("server did not exit after stdin closed");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn decode_b64(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for b in s.bytes().filter(|&b| b != b'=') {
        let v = T.iter().position(|&t| t == b).expect("base64") as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// `- name: id` lines under "Created nodes" of an execute response.
fn created_id(text: &str, name: &str) -> String {
    text.lines()
        .skip_while(|l| !l.starts_with("Created nodes"))
        .find_map(|l| {
            let (n, id) = l.strip_prefix("- ")?.rsplit_once(": ")?;
            (n == name).then(|| id.trim().to_string())
        })
        .unwrap_or_else(|| panic!("`{name}` not in created nodes:\n{text}"))
}

fn edit_id(text: &str) -> String {
    let i = text.find("editId \"").expect("editId in error") + 8;
    text[i..].split('"').next().unwrap().to_string()
}

const BUILD: &str = r##"
const p = FindEmptySpace({width: 360, height: 400, padding: 40})
btn = Insert(document, {type: "frame", name: "Button", reusable: true, x: p.x, y: p.y,
  layout: "horizontal", padding: [10, 16], cornerRadius: 8, fill: "$accent",
  children: [{type: "text", name: "Label", content: "Button", fontFamily: "Inter", fontSize: 14, fontWeight: "600", fill: "#FFFFFF"}]})
btnLabel = Get(btn, {depth: 1}).children[0].id
const q = FindEmptySpace({width: 360, height: 400, padding: 40, nodeId: btn})
screen = Insert(document, {type: "frame", name: "Welcome screen", x: q.x, y: q.y, width: 360,
  layout: "vertical", gap: 12, padding: 24, fill: "#FFFFFF", children: [
    {type: "text", name: "Title", content: "Welcome back", fontFamily: "Inter", fontSize: 24, fontWeight: "700", fill: "#18181B"},
    {type: "text", name: "Body", content: "Sign in to continue.", fontFamily: "Inter", fontSize: 14, fill: "#52525B",
     textGrowth: "fixed-width", width: "fill_container"}
  ]})
Insert(screen, {type: "ref", ref: btn, name: "CTA", descendants: {[btnLabel]: {content: "Get started"}}})
TakeScreenshot([screen])
"##;

#[test]
fn headless_session_end_to_end() {
    let dir = scratch("session");
    let file = dir.join("design.pen");
    std::fs::write(&file, FIXTURE).unwrap();
    let mut c = Client::spawn(&file, &[]);

    let list = c.request("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in [
        "get_app_state",
        "execute",
        "save",
        "read_skill",
        "start_task",
    ] {
        assert!(names.contains(&t), "{t} missing from {names:?}");
    }

    let (skills, _, err) = c.call(
        "start_task",
        json!({"title": "Welcome screen", "skills": ["SKILL.md", "execute.md"]}),
    );
    assert!(!err, "{skills}");
    assert!(
        skills.contains("FindEmptySpace") && skills.contains("Design quality"),
        "{skills}"
    );

    let (state, _, err) = c.call("get_app_state", json!({}));
    assert!(!err, "{state}");
    assert!(state.contains("design.pen"), "{state}");
    assert!(state.contains("`intro` (frame): Intro"), "{state}");
    assert!(state.contains("`accent` (color)"), "{state}");
    assert!(state.contains("e2e-agent"), "{state}");

    // A uri that is not the served file.
    let (msg, _, err) = c.call("get_app_state", json!({"uri": "C:/nope/other.pen"}));
    assert!(err && msg.contains("serves only"), "{msg}");

    // Build a small screen with a component and an instance override.
    let (text, images, err) = c.execute("Build welcome screen", BUILD);
    assert!(!err, "{text}");
    let screen = created_id(&text, "Welcome screen");
    let _btn = created_id(&text, "Button");
    let _cta = created_id(&text, "CTA");
    assert_eq!(images.len(), 1, "{text}");
    assert_eq!(&images[0][..8], b"\x89PNG\r\n\x1a\n");

    // Globals persisted for this agent: `screen` and `btn` are known.
    let (text, _, err) = c.execute(
        "Inspect screen",
        "Get(screen, (n, c) => Print(n.name, c.bounds.width, c.problems || 'ok'))",
    );
    assert!(!err, "{text}");
    assert!(text.contains("Welcome screen 360 ok"), "{text}");

    // A deliberate error: nothing applied, an editId comes back.
    let (text, _, err) = c.execute(
        "Add footer",
        "Insert(screen, {type: \"text\", name: \"Footer\", content: footerText, fontFamily: \"Inter\", fontSize: 12, fill: \"#71717A\"})",
    );
    assert!(err, "{text}");
    assert!(text.contains("footerText"), "{text}");
    let eid = edit_id(&text);
    let (text, _, err) = c.call(
        "execute",
        json!({"description": "Add footer", "editId": eid,
               "edits": [{"find": "content: footerText", "replace": "content: \"No account? Sign up\""}]}),
    );
    assert!(!err, "{text}");
    created_id(&text, "Footer");

    let (text, _, err) = c.call("save", json!({"uri": file.display().to_string()}));
    assert!(!err && text.starts_with("Saved"), "{text}");
    c.close();

    // Reload with pen-doc: everything is there.
    let bytes = std::fs::read(&file).unwrap();
    let (scene, _) = pen_doc::SceneDoc::load(&bytes, Some(&dir), &pen_doc::NoLibraries).unwrap();
    let by_name = |name: &str| {
        scene
            .roots()
            .iter()
            .flat_map(|&r| std::iter::once(r).chain(scene.descendants(r)))
            .find(|&k| scene.get(k).unwrap().name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no node named {name}"))
    };
    let s = by_name("Welcome screen");
    assert_eq!(&*scene.get(s).unwrap().id, screen.as_str());
    for n in ["Title", "Body", "Footer", "Button", "Intro"] {
        by_name(n);
    }
    let cta = by_name("CTA");
    let label = scene.children(cta)[0];
    let text = scene.get(label).unwrap().text().unwrap();
    assert_eq!(&*text.content, "Get started");
    let saved = String::from_utf8(bytes).unwrap();
    assert!(saved.contains("\"descendants\""), "{saved}");
}

#[test]
fn readonly_hides_save_and_keeps_file() {
    let dir = scratch("readonly");
    let file = dir.join("ro.pen");
    std::fs::write(&file, FIXTURE).unwrap();
    let mut c = Client::spawn(&file, &["--readonly"]);
    let list = c.request("tools/list", json!({}));
    assert!(
        !list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "save"),
        "{list}"
    );
    let (text, _, err) = c.execute(
        "Rename",
        "Update(\"intro\", {name: \"Changed\"})\nExport([\"intro\"], \"svg\", \"out\")",
    );
    assert!(!err, "{text}");
    assert!(dir.join("out").join("intro.svg").is_file(), "{text}");
    c.close();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), FIXTURE);
}

#[test]
fn missing_file_fails_clearly() {
    let out = Command::new(env!("CARGO_BIN_EXE_openpen-cli"))
        .args(["mcp", "--headless", "definitely-missing.pen"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("definitely-missing.pen"), "{err}");
    assert!(out.stdout.is_empty());
}

#[test]
fn print_config_headless() {
    let out = Command::new(env!("CARGO_BIN_EXE_openpen-cli"))
        .args(["mcp", "--print-config", "--headless", "a.pen", "--readonly"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let args = v["mcpServers"]["openpen"]["args"].as_array().unwrap();
    assert_eq!(args[0], "mcp");
    assert_eq!(args[1], "--headless");
    assert!(args[2].as_str().unwrap().ends_with("a.pen"));
    assert_eq!(args[3], "--readonly");
}

/// Corpus smoke: the biggest real design served headless (skipped when absent).
#[test]
fn corpus_t_process_recorder() {
    let src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/user/t-process-recorder.pen");
    if !src.is_file() {
        eprintln!("skip: {} not found", src.display());
        return;
    }
    let dir = scratch("corpus");
    let file = dir.join("t-process-recorder.pen");
    std::fs::copy(&src, &file).unwrap();
    let t0 = Instant::now();
    let mut c = Client::spawn(&file, &["--readonly"]);
    let t_init = t0.elapsed();
    let t1 = Instant::now();
    let (state, _, err) = c.call("get_app_state", json!({}));
    let t_state = t1.elapsed();
    assert!(!err, "{state}");
    assert!(state.contains("## Top-level nodes"), "{state}");
    let t2 = Instant::now();
    let (text, _, err) = c.execute(
        "Count nodes",
        "let n = 0, clipped = 0\nGet((node, c) => { n++; if (c.problems) clipped++ })\nPrint('nodes', n, 'clipped', clipped)",
    );
    let t_get = t2.elapsed();
    assert!(!err, "{text}");
    assert!(text.contains("nodes "), "{text}");
    eprintln!(
        "corpus headless: spawn+load+initialize {t_init:?}, get_app_state {t_state:?}, Get visitor {t_get:?} ({})",
        text.lines().find(|l| l.starts_with("nodes")).unwrap_or("")
    );
    c.close();
}
