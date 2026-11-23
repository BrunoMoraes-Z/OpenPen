//! Web import without a window or a browser: the stub page goes into a
//! document as one undo step at a free spot, flashes once, and a failure
//! leaves nothing behind. The job runner reports progress and finishes.

use openpen::agent::touched_nodes;
use openpen::scene_document::SceneDocument;
use openpen::web_import::{self, Failure, ImportRequest, Stage, insert_into, stub_outcome};
use pen_script::ScriptEngine;

fn doc_with_screen() -> SceneDocument {
    let mut d = SceneDocument::empty(&[]);
    let mut eng = ScriptEngine::new();
    let resp = d.run_script(
        &mut eng,
        &mut openpen::agent::AppScriptHost,
        pen_script::ExecuteRequest {
            agent_id: "setup".into(),
            description: "setup".into(),
            input: Some(
                "Insert(document, {type: 'frame', name: 'Existing', x: 0, y: 0, width: 800, height: 600})"
                    .into(),
            ),
            ..Default::default()
        },
    );
    assert!(resp.ok, "{}", resp.text);
    d.mark_saved();
    d
}

#[test]
fn stub_page_is_one_undo_step_beside_the_content() {
    let mut d = doc_with_screen();
    let before = d.save_string();
    let steps = d.undo_steps();
    let outcome = stub_outcome(&ImportRequest::new("https://example.com/".into()));
    let mut eng = ScriptEngine::new();

    let (id, resp) = insert_into(
        &mut d,
        &mut eng,
        web_import::DIALOG_AGENT_ID,
        &outcome.frame(),
    )
    .unwrap();
    assert!(d.is_dirty());
    assert_eq!(d.undo_steps(), steps + 1);
    let k = d.scene.find(&id).expect("new frame");
    assert_eq!(d.scene.roots().len(), 2);
    assert_eq!(d.scene.children(k).len(), 4);
    let r = d.world_aabb(k).unwrap();
    assert!(r.x >= 800.0, "placed over the existing screen: {r:?}");
    assert_eq!((r.w, r.h), (1440.0, 640.0));
    // One flash for the whole page, not one per layer.
    let flashes = touched_nodes(&d, &resp.touched);
    assert_eq!(flashes.len(), 1);
    assert_eq!(flashes[0].id, id);

    // A single undo restores the saved text exactly; redo brings it back.
    assert!(d.undo());
    assert_eq!(d.save_string(), before);
    assert!(!d.is_dirty());
    assert!(d.redo());
    assert!(d.scene.find(&id).is_some());
}

#[test]
fn a_second_import_does_not_overlap_the_first() {
    let mut d = doc_with_screen();
    let outcome = stub_outcome(&ImportRequest::new("https://example.com/".into()));
    let mut eng = ScriptEngine::new();
    let (a, _) = insert_into(&mut d, &mut eng, "web", &outcome.frame()).unwrap();
    let (b, _) = insert_into(&mut d, &mut eng, "web", &outcome.frame()).unwrap();
    let ra = d.world_aabb(d.scene.find(&a).unwrap()).unwrap();
    let rb = d.world_aabb(d.scene.find(&b).unwrap()).unwrap();
    let overlap =
        ra.x < rb.x + rb.w && rb.x < ra.x + ra.w && ra.y < rb.y + rb.h && rb.y < ra.y + ra.h;
    assert!(!overlap, "{ra:?} {rb:?}");
}

#[test]
fn a_bad_frame_changes_nothing() {
    let mut d = doc_with_screen();
    let before = d.save_string();
    let steps = d.undo_steps();
    let mut eng = ScriptEngine::new();
    let bad = serde_json::json!({"type": "webview", "name": "x"});
    let err = insert_into(&mut d, &mut eng, "web", &bad).unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(d.save_string(), before);
    assert_eq!(d.undo_steps(), steps);
    assert!(!d.is_dirty());
}

#[test]
fn stub_job_reports_progress_and_finishes() {
    let mut req = ImportRequest::new("https://example.com/".into());
    req.stub = true;
    let mut job = web_import::spawn(req.clone());
    let r = futures::executor::block_on(&mut job.result).unwrap();
    let o = r.expect("stub import");
    assert_eq!(o.title, "Example Domain");
    let mut stages = Vec::new();
    while let Ok(s) = job.progress.try_recv() {
        stages.push(s);
    }
    assert_eq!(stages.first(), Some(&Stage::Launching));
    assert!(stages.contains(&Stage::Converting));

    // Cancelling a running job resolves it as cancelled.
    let mut job = web_import::spawn(req);
    job.cancel.cancel();
    let r = futures::executor::block_on(&mut job.result).unwrap();
    assert_eq!(r.unwrap_err(), Failure::Cancelled);
}

/// Serves one HTML page on 127.0.0.1 for the real-browser test.
fn serve_page(html: &'static str) -> String {
    use std::io::{BufRead as _, BufReader, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { continue };
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                loop {
                    let mut l = String::new();
                    if r.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
                        break;
                    }
                }
                let _ = write!(
                    c,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
            });
        }
    });
    format!("http://{addr}/")
}

/// The app adapter over the real pen-import: progress, outcome mapping and
/// the insert. Skipped when no Edge/Chrome/Chromium is installed.
#[test]
fn real_browser_import_through_the_adapter() {
    if pen_import::find_browser().is_none() {
        eprintln!("skipped: no Edge/Chrome/Chromium found");
        return;
    }
    let url = serve_page(
        "<!doctype html><title>Adapter Page</title><body style=\"margin:0\">\
         <h1 style=\"font:700 32px Arial;margin:40px\">Adapter heading</h1>\
         <div style=\"width:200px;height:80px;background:#3b82f6;margin:0 40px\"></div></body>",
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/web-import-it")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let mut req = ImportRequest::new(url);
    req.width = 800;
    req.height = 600;
    req.full_page = false;
    req.assets_dir = Some(dir);
    req.stub = false;
    let mut job = web_import::spawn(req);
    let r = futures::executor::block_on(&mut job.result).unwrap();
    let o = r.expect("import");
    let mut stages = Vec::new();
    while let Ok(s) = job.progress.try_recv() {
        stages.push(s);
    }
    assert!(stages.contains(&Stage::Launching), "{stages:?}");
    assert!(stages.contains(&Stage::Converting), "{stages:?}");
    assert_eq!(o.title, "Adapter Page");
    assert!(o.screenshot_png.is_some());
    assert!(o.stats.iter().any(|(k, _)| k == "layers"));

    let mut d = doc_with_screen();
    let mut eng = ScriptEngine::new();
    let (id, _) = insert_into(&mut d, &mut eng, "web", &o.frame()).unwrap();
    let k = d.scene.find(&id).unwrap();
    assert_eq!(
        d.get(k)
            .and_then(|n| n.name.as_deref().map(str::to_string))
            .as_deref(),
        Some("Adapter Page")
    );
    assert_eq!(d.undo_steps(), 2);
}
