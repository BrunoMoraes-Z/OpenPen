//! Agent scripts (MCP `execute`) through the document the app uses, without a
//! window: one call = one undo step that restores the file byte for byte,
//! dirty state, failures leave nothing behind, and the flash bookkeeping.

use std::path::PathBuf;
use std::time::Instant;

use openpen::agent::fx::AgentFx;
use openpen::agent::{AppScriptHost, touched_nodes};
use openpen::scene_document::SceneDocument;
use pen_render::Vec2;
use pen_script::{ExecuteRequest, ScriptEngine};

const SCREEN: &str = r##"
s = Insert(document, {type: "frame", name: "Agent Screen", x: 0, y: 0, width: 360, height: 240,
  layout: "vertical", gap: 8, padding: 16, fill: "#FFFFFF", children: [
    {type: "text", name: "Title", content: "Hello from the agent", fontFamily: "Inter", fontSize: 20, fill: "#111111"},
    {type: "rectangle", name: "Card", width: "fill_container", height: 80, fill: "#E0E7FF"}
]})
"##;

fn req(src: &str) -> ExecuteRequest {
    ExecuteRequest {
        agent_id: "client-1".into(),
        description: "Create a screen".into(),
        input: Some(src.into()),
        ..Default::default()
    }
}

fn names(d: &SceneDocument) -> Vec<String> {
    d.scene
        .roots()
        .iter()
        .filter_map(|&k| {
            d.get(k)
                .and_then(|n| n.name.as_ref().map(|s| s.to_string()))
        })
        .collect()
}

#[test]
fn execute_is_one_undo_step_and_marks_dirty() {
    let mut d = SceneDocument::empty(&[]);
    let mut eng = ScriptEngine::new();
    let before = d.save_string();
    assert!(!d.is_dirty());
    let rev = d.revision();

    let resp = d.run_script(&mut eng, &mut AppScriptHost, req(SCREEN));
    assert!(resp.ok, "{}", resp.text);
    assert_eq!(names(&d), vec!["Agent Screen".to_string()]);
    let root = d.scene.roots()[0];
    assert_eq!(d.scene.children(root).len(), 2);
    assert!(d.is_dirty());
    assert_eq!(d.undo_steps(), 1);
    // Layout and the display list are up to date (the canvas sees it).
    assert_ne!(d.revision(), rev);
    let r = d.world_aabb(root).unwrap();
    assert_eq!((r.w, r.h), (360.0, 240.0));
    assert!(d.display_list().bounds().is_some());

    // One undo removes everything, back to the saved state.
    assert!(d.undo());
    d.flush();
    assert!(d.scene.roots().is_empty());
    assert_eq!(d.save_string(), before);
    assert!(!d.is_dirty());
    // Redo brings it back.
    assert!(d.redo());
    d.flush();
    assert_eq!(names(&d), vec!["Agent Screen".to_string()]);
    assert!(d.is_dirty());
}

#[test]
fn failed_execute_changes_nothing() {
    let mut d = SceneDocument::empty(&[]);
    let mut eng = ScriptEngine::new();
    let before = d.save_string();
    let src = format!("{SCREEN}\nthrow new Error(\"boom\")");
    let resp = d.run_script(&mut eng, &mut AppScriptHost, req(&src));
    assert!(!resp.ok);
    assert!(resp.edit_id.is_some());
    assert!(resp.text.contains("boom"), "{}", resp.text);
    assert!(d.scene.roots().is_empty());
    assert_eq!(d.save_string(), before);
    assert!(!d.is_dirty());
    assert_eq!(d.undo_steps(), 0);
    assert!(!d.can_undo());
}

#[test]
fn touched_nodes_flash_once_per_subtree() {
    let mut d = SceneDocument::empty(&[]);
    let mut eng = ScriptEngine::new();
    let resp = d.run_script(&mut eng, &mut AppScriptHost, req(SCREEN));
    assert!(resp.ok);
    assert!(!resp.touched.is_empty());
    let nodes = touched_nodes(&d, &resp.touched);
    // The new frame flashes; its children are covered by it.
    assert_eq!(nodes.len(), 1, "{nodes:?}");
    assert!(nodes[0].write);
    assert_eq!((nodes[0].rect.w, nodes[0].rect.h), (360.0, 240.0));

    // Reads flash too (long hold).
    // `s` persisted from the previous call (global without const/let).
    let read = d.run_script(
        &mut eng,
        &mut AppScriptHost,
        req("Print(Get(s, {depth: 1}).children.length)"),
    );
    assert!(read.ok, "{}", read.text);
    assert_eq!(read.ops, 0);
    let rn = touched_nodes(&d, &read.touched);
    assert!(!rn.is_empty());
    assert!(rn.iter().all(|n| !n.write));

    let mut fx = AgentFx::new();
    let now = Instant::now();
    fx.touch("client-1", "claude", 0x8b5cf6, &nodes, Vec2::ZERO, now);
    assert_eq!(fx.flashes.len(), 1);
    assert!(fx.animating(now));
}

fn corpus_file() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/user/t-process-recorder.pen");
    p.is_file().then_some(p)
}

#[test]
fn execute_on_real_document_undoes_to_identical_bytes() {
    let Some(path) = corpus_file() else {
        eprintln!("testdata/user missing: skipped");
        return;
    };
    let (mut d, _) = SceneDocument::load(&path, &[]).unwrap();
    let mut eng = ScriptEngine::new();
    let before = d.save_string();
    let count = d.scene.node_count();
    let src = SCREEN.replace("x: 0, y: 0", "x: 0, y: -2000");
    let resp = d.run_script(&mut eng, &mut AppScriptHost, req(&src));
    assert!(resp.ok, "{}", resp.text);
    assert_eq!(d.scene.node_count(), count + 3);
    assert!(d.is_dirty());
    assert!(d.undo());
    d.flush();
    assert_eq!(d.scene.node_count(), count);
    assert_eq!(d.save_string(), before);
    assert!(!d.is_dirty());
}
