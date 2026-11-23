//! Script `@input`s and color variable bindings edited through the inspector
//! path (`PropEdit` on a `SceneDocument`): one undo step per edit, other
//! inputs and unknown fields kept, "Reload script" picks up header changes.

use openpen::inspector::fill::PaintOp;
use openpen::inspector::model::{PaintKind, PropEdit, PropertySource};
use openpen::inspector::script::ScriptValue;
use openpen::scene_document::{SceneDocument, id_of};
use pen_doc::NodeKey;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const SCRIPT: &str = r#"/**
 * @schema 2.10
 * @input count: number(min=1, max=10) = 3
 * @input color: color = #22C55E
 * @input on: boolean = true
 * @input mode: enum("a", "b") = "a"
 */
const out = [];
for (let i = 0; i < pencil.input.count; i++) {
  out.push({ type: "rectangle", x: i * 10, y: 0, width: 8, height: 8, fill: pencil.input.color });
}
return out;"#;

const DOC: &str = r##"{
  "version": "2.22",
  "variables": {
    "brand": { "type": "color", "value": "#3366ff" },
    "accent": { "type": "color", "value": "#ff8800" },
    "gap": { "type": "number", "value": 4 }
  },
  "children": [
    { "type": "script", "id": "s", "x": 0, "y": 0, "width": 100, "height": 20,
      "scriptUri": "gen.js", "inputs": { "mode": "b", "count": 2 }, "future": 1 },
    { "type": "rectangle", "id": "r", "x": 0, "y": 40, "width": 10, "height": 10, "fill": "#000000" },
    { "type": "rectangle", "id": "g", "x": 0, "y": 60, "width": 10, "height": 10,
      "fill": { "type": "gradient", "gradientType": "radial",
        "colors": [ { "color": "#ff0000", "position": 0 }, { "color": "$brand", "position": 1 } ] } }
  ]
}"##;

fn load(dir: &Path) -> SceneDocument {
    std::fs::write(dir.join("gen.js"), SCRIPT).unwrap();
    SceneDocument::from_bytes(DOC.as_bytes(), Some(dir), &[])
        .unwrap()
        .0
}

fn key(d: &SceneDocument, id: &str) -> NodeKey {
    d.scene.find(id).unwrap()
}

fn edit(d: &mut SceneDocument, k: NodeKey, e: PropEdit) {
    d.begin();
    d.apply_edit(k, &e);
    d.commit();
    d.flush();
    d.refresh_props(&[id_of(k)]);
}

fn node_json(d: &SceneDocument, id: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(&d.save_string()).unwrap();
    v["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == id)
        .unwrap()
        .clone()
}

fn generated(d: &SceneDocument, k: NodeKey) -> usize {
    d.scene.generated(k).len()
}

#[test]
fn script_inputs_edit_in_one_undo_step() {
    let dir = scratch("script-inputs");
    let mut d = load(&dir);
    let s = key(&d, "s");
    d.refresh_props(&[id_of(s)]);
    let sp = d.props(id_of(s)).unwrap().script.clone().unwrap();
    assert_eq!(sp.uri, "gen.js");
    let inputs = sp.inputs.unwrap();
    let names: Vec<&str> = inputs.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["count", "color", "on", "mode"]);
    assert_eq!(inputs[0].value, ScriptValue::Number(2.0));
    assert_eq!(inputs[1].color(), 0x22c55eff);
    assert_eq!(inputs[3].text(), "b");
    assert_eq!(generated(&d, s), 2);

    let steps = d.undo_steps();
    edit(
        &mut d,
        s,
        PropEdit::ScriptInput {
            name: "count".into(),
            value: ScriptValue::Number(50.0),
        },
    );
    assert_eq!(d.undo_steps(), steps + 1);
    let j = node_json(&d, "s");
    // Clamped to the declared max; order and the other input kept.
    assert_eq!(j["inputs"], serde_json::json!({ "mode": "b", "count": 10 }));
    assert_eq!(j["future"], 1);
    assert_eq!(generated(&d, s), 10);

    edit(
        &mut d,
        s,
        PropEdit::ScriptInput {
            name: "color".into(),
            value: ScriptValue::Text("#112233".into()),
        },
    );
    edit(
        &mut d,
        s,
        PropEdit::ScriptInput {
            name: "on".into(),
            value: ScriptValue::Bool(false),
        },
    );
    let j = node_json(&d, "s");
    assert_eq!(j["inputs"]["color"], "#112233");
    assert_eq!(j["inputs"]["on"], false);
    // Unknown names do nothing.
    let before = d.undo_steps();
    edit(
        &mut d,
        s,
        PropEdit::ScriptInput {
            name: "nope".into(),
            value: ScriptValue::Bool(true),
        },
    );
    assert_eq!(d.undo_steps(), before);

    assert!(d.undo());
    assert!(d.undo());
    assert!(d.undo());
    d.flush();
    assert_eq!(
        node_json(&d, "s")["inputs"],
        serde_json::json!({ "mode": "b", "count": 2 })
    );
    assert_eq!(generated(&d, s), 2);
}

#[test]
fn reload_picks_up_a_changed_header() {
    let dir = scratch("script-reload");
    let mut d = load(&dir);
    let s = key(&d, "s");
    d.refresh_props(&[id_of(s)]);
    assert_eq!(generated(&d, s), 2);
    // A new input and a new body: same mtime granularity is avoided by a
    // different file size.
    let src = SCRIPT
        .replace(
            " * @input mode",
            " * @input label: string = \"x\"\n * @input mode",
        )
        .replace("i * 10", "i * 12");
    std::fs::write(dir.join("gen.js"), src).unwrap();
    assert!(d.refresh_scripts());
    d.refresh_props(&[id_of(s)]);
    let sp = d.props(id_of(s)).unwrap().script.clone().unwrap();
    assert!(sp.inputs.unwrap().iter().any(|i| i.name == "label"));
}

#[test]
fn bind_and_detach_color_variables() {
    let dir = scratch("bind-vars");
    let mut d = load(&dir);
    let r = key(&d, "r");
    assert_eq!(
        d.color_variables(r),
        vec![
            ("accent".to_string(), 0xff8800ff),
            ("brand".to_string(), 0x3366ffff)
        ]
    );
    let bind = |var: Option<&str>, stop: Option<usize>| PropEdit::Paint {
        stroke: false,
        index: 0,
        op: PaintOp::BindColor {
            stop,
            var: var.map(str::to_string),
        },
    };
    let steps = d.undo_steps();
    edit(&mut d, r, bind(Some("accent"), None));
    assert_eq!(d.undo_steps(), steps + 1);
    assert_eq!(node_json(&d, "r")["fill"], "$accent");
    let p = d.props(id_of(r)).unwrap();
    assert_eq!(p.fills[0].var.as_deref(), Some("accent"));
    assert_eq!(p.fills[0].kind, PaintKind::Solid(0xff8800ff));
    // Detach: the resolved color stays, as a literal.
    edit(&mut d, r, bind(None, None));
    assert_eq!(node_json(&d, "r")["fill"], "#FF8800");
    assert!(d.props(id_of(r)).unwrap().fills[0].var.is_none());

    // Gradient stops.
    let g = key(&d, "g");
    d.refresh_props(&[id_of(g)]);
    edit(&mut d, g, bind(Some("accent"), Some(0)));
    edit(&mut d, g, bind(None, Some(1)));
    let j = node_json(&d, "g");
    assert_eq!(j["fill"]["colors"][0]["color"], "$accent");
    assert_eq!(j["fill"]["colors"][1]["color"], "#3366FF");
    let st = &d.props(id_of(g)).unwrap().fills[0]
        .gradient
        .as_ref()
        .unwrap()
        .stops;
    assert_eq!(st[0].var.as_deref(), Some("accent"));
    assert_eq!(st[0].color, 0xff8800ff);
    assert!(d.undo());
    d.flush();
    assert_eq!(node_json(&d, "g")["fill"]["colors"][1]["color"], "$brand");
}

#[test]
fn nested_script_input_edit_stays_in_place() {
    let dir = scratch("script-nested");
    std::fs::write(dir.join("gen.js"), SCRIPT).unwrap();
    let doc = r##"{"version":"2.22","children":[{"type":"frame","id":"f","x":0,"y":0,"width":400,"height":300,"layout":"none","children":[
        {"type":"rectangle","id":"a","x":0,"y":0,"width":10,"height":10},
        {"type":"script","id":"s","x":100,"y":100,"width":100,"height":20,"scriptUri":"gen.js","inputs":{"count":2}},
        {"type":"rectangle","id":"b","x":0,"y":0,"width":10,"height":10}]}]}"##;
    let mut d = SceneDocument::from_bytes(doc.as_bytes(), Some(&dir), &[])
        .unwrap()
        .0;
    let s = key(&d, "s");
    d.refresh_props(&[id_of(s)]);
    edit(
        &mut d,
        s,
        PropEdit::ScriptInput {
            name: "count".into(),
            value: ScriptValue::Number(5.0),
        },
    );
    let v: serde_json::Value = serde_json::from_str(&d.save_string()).unwrap();
    let kids: Vec<&str> = v["children"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(kids, ["a", "s", "b"], "{v}");
    assert_eq!(v["children"].as_array().unwrap().len(), 1);
    let s = key(&d, "s");
    assert_eq!(d.scene.generated(s).len(), 5);
}
