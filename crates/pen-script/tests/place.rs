//! `insert_at_free_spot`: a node tree without ids lands at a free spot of the
//! root as one undo step.

use pen_doc::{SceneDoc, UndoStack};
use pen_layout::{NoAssets, Pipeline, TextEngine};
use pen_script::{HeadlessHost, ScriptEngine, insert_at_free_spot, inserted_id};
use serde_json::json;

const DOC: &str = r##"{ "version": "2.22", "children": [
  { "type": "frame", "id": "a", "name": "Existing", "x": 0, "y": 0, "width": 400, "height": 300 }
] }"##;

fn page() -> serde_json::Value {
    json!({
        "type": "frame", "name": "Example Domain", "width": 1440, "height": 900,
        "x": 5, "y": 5, "fill": "#ffffff", "clip": true,
        "children": [
            { "type": "text", "name": "h1", "content": "Example \"Domain\" \u{2028} </script>",
              "x": 100, "y": 80, "fontSize": 32 },
            { "type": "rectangle", "name": "box", "x": 100, "y": 160, "width": 600, "height": 40,
              "fill": "#eeeeee" }
        ]
    })
}

#[test]
fn inserts_once_at_a_free_spot_as_one_undo_step() {
    let (mut scene, _) = SceneDoc::load(DOC.as_bytes(), None, &pen_doc::NoLibraries).expect("load");
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.rebuild(&mut scene);
    let mut undo = UndoStack::new();
    let mut eng = ScriptEngine::new();
    let before = scene.save();

    let req = insert_at_free_spot("web-import", "Import web page", &page());
    let resp = eng.execute(&mut scene, &mut pipe, &mut undo, &mut HeadlessHost, req);
    assert!(resp.ok, "{}", resp.text);
    let id = inserted_id(&resp).expect("id");
    let key = scene.find(&id).expect("inserted node");
    assert_eq!(scene.node_count(), 4);
    assert_eq!(undo.undo_len(), 1);
    assert!(resp.touched.iter().any(|t| t.id == id && t.write));

    // Placed to the right of the existing frame, not at the page's own x/y.
    pipe.update(&mut scene);
    let saved: serde_json::Value = serde_json::from_str(&scene.save()).unwrap();
    let node = saved["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id.as_str())
        .unwrap();
    assert!(node["x"].as_f64().unwrap() >= 400.0, "{node}");
    assert_eq!(node["name"], "Example Domain");
    assert_eq!(
        node["children"][0]["content"],
        "Example \"Domain\" \u{2028} </script>"
    );
    let _ = key;

    // One undo restores the document exactly.
    assert!(undo.undo(&mut scene).unwrap());
    assert_eq!(scene.save(), before);
}

#[test]
fn invalid_tree_changes_nothing() {
    let (mut scene, _) = SceneDoc::load(DOC.as_bytes(), None, &pen_doc::NoLibraries).expect("load");
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.rebuild(&mut scene);
    let mut undo = UndoStack::new();
    let before = scene.save();
    let req = insert_at_free_spot("w", "Import", &json!({"type": "no-such-type"}));
    let resp =
        ScriptEngine::new().execute(&mut scene, &mut pipe, &mut undo, &mut HeadlessHost, req);
    assert!(!resp.ok);
    assert_eq!(inserted_id(&resp), None);
    assert!(!pen_script::place::insert_error(&resp).is_empty());
    assert_eq!(scene.save(), before);
    assert_eq!(undo.undo_len(), 0);
}
