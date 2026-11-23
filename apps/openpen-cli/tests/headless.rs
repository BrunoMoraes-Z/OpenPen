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
        Self::spawn_env(file, extra, &[])
    }

    fn spawn_env(file: &Path, extra: &[&str], env: &[(&str, &str)]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_openpen-cli"))
            .arg("mcp")
            .arg("--headless")
            .arg(file)
            .args(extra)
            .envs(env.iter().copied())
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

/// `import_website` against the browserless stub: one new frame beside the
/// existing content, saved with the document; bad addresses are refused.
#[test]
fn headless_import_website_with_stub() {
    let dir = scratch("import");
    let file = dir.join("design.pen");
    std::fs::write(&file, FIXTURE).unwrap();
    let mut c = Client::spawn_env(&file, &[], &[("OPENPEN_WEB_IMPORT_STUB", "1")]);
    let tools = c.request("tools/list", json!({}));
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "import_website"),
        "{tools}"
    );

    let (text, _, err) = c.call(
        "import_website",
        json!({"url": "example.com", "width": 390}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("https://example.com/"), "{text}");
    assert!(text.contains("390 x 640"), "{text}");
    let id = text.split('`').nth(1).expect("frame id").to_string();

    // The frame is in the document, right of the 200 px wide intro frame.
    let (out, _, err) = c.execute(
        "Check import",
        &format!("const n = Get(\"{id}\", {{depth: 0}}); Print(n.name, n.x, n.width)"),
    );
    assert!(!err, "{out}");
    let line = out
        .lines()
        .find(|l| l.starts_with("Example Domain"))
        .expect(&out);
    let x: f64 = line.split(' ').nth(2).unwrap().parse().unwrap();
    assert!(x >= 200.0, "{line}");

    let r = c.request(
        "tools/call",
        json!({"name": "import_website", "arguments": {"url": "ftp://example.com"}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let (text, _, err) = c.call(
        "import_website",
        json!({"url": "example.com", "uri": dir.join("other.pen").display().to_string()}),
    );
    assert!(err && text.contains("serves only"), "{text}");

    let (_, _, err) = c.call("save", json!({}));
    assert!(!err);
    c.close();
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("Example Domain"));
}

/// `--readonly` refuses imports (they write asset files).
#[test]
fn headless_import_website_readonly() {
    let dir = scratch("import-ro");
    let file = dir.join("design.pen");
    std::fs::write(&file, FIXTURE).unwrap();
    let mut c = Client::spawn_env(&file, &["--readonly"], &[("OPENPEN_WEB_IMPORT_STUB", "1")]);
    let (text, _, err) = c.call("import_website", json!({"url": "example.com"}));
    assert!(err && text.contains("--readonly"), "{text}");
    c.close();
}

/// A one-page HTTP server for the real-browser test (stops when dropped).
struct Site {
    base: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    addr: std::net::SocketAddr,
}

impl Drop for Site {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.addr);
    }
}

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Fixture Shop</title>
<style>body{margin:0;font-family:Arial,sans-serif;background:#fafafa}
header{height:64px;background:#1e293b;color:#fff;display:flex;align-items:center;padding:0 32px}
main{padding:48px 32px}h1{font-size:40px;margin:0 0 16px;color:#0f172a}
.card{width:320px;padding:24px;border-radius:12px;background:#fff;box-shadow:0 2px 8px #0002}
img{width:64px;height:64px;display:block}</style></head>
<body><header>Fixture Shop</header><main><h1>Hello import</h1>
<div class="card"><img src="/dot.png" alt="dot"><p>A small card with an image.</p></div></main></body></html>"#;

/// A 1x1 red PNG.
fn dot_png() -> Vec<u8> {
    let mut buf = Vec::new();
    {
        buf.extend_from_slice(&[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D,
            0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]);
    }
    buf
}

fn serve_site() -> Site {
    use std::io::{BufRead as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s = stop.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if s.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let Ok(mut c) = conn else { continue };
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut first = String::new();
                let _ = r.read_line(&mut first);
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/");
                let (ctype, body) = match path {
                    "/" => ("text/html; charset=utf-8", PAGE.as_bytes().to_vec()),
                    "/dot.png" => ("image/png", dot_png()),
                    _ => ("text/plain", b"not found".to_vec()),
                };
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = c.write_all(&body);
            });
        }
    });
    Site {
        base: format!("http://{addr}/"),
        stop,
        addr,
    }
}

/// The real thing: Edge/Chrome loads a local page and the frame lands in the
/// document with its image file next to it. Skipped without a browser.
#[test]
fn headless_import_website_real_browser() {
    if pen_import::find_browser().is_none() {
        eprintln!("skipped: no Edge/Chrome/Chromium found");
        return;
    }
    let site = serve_site();
    let dir = scratch("import-real");
    let file = dir.join("design.pen");
    std::fs::write(&file, FIXTURE).unwrap();
    let mut c = Client::spawn(&file, &[]);
    let (text, images, err) = c.call(
        "import_website",
        json!({"url": site.base, "width": 800, "height": 600}),
    );
    assert!(!err, "{text}");
    assert!(text.contains("Fixture Shop"), "{text}");
    assert_eq!(images.len(), 1, "viewport screenshot");
    assert!(images[0].starts_with(&[0x89, b'P', b'N', b'G']));
    let id = text.split('`').nth(1).expect("frame id").to_string();
    let (out, _, err) = c.execute(
        "Read import",
        &format!(
            "const texts = []; Get(\"{id}\", (n) => {{ if (n.type === \"text\") texts.push(n.content) }}); Print(texts.join(\"|\"))"
        ),
    );
    assert!(!err, "{out}");
    assert!(out.contains("Hello import"), "{out}");
    // The image was downloaded next to the document.
    let images_dir = dir.join("images");
    let n = walk(&images_dir);
    assert!(n >= 1, "no image under {}: {text}", images_dir.display());
    c.close();
}

fn walk(d: &Path) -> usize {
    std::fs::read_dir(d)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() { walk(&p) } else { 1 }
                })
                .sum()
        })
        .unwrap_or(0)
}
