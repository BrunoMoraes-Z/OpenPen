use std::time::Duration;

use pen_doc::{SceneDoc, UndoStack};
use pen_layout::{NoAssets, Pipeline, TextEngine};
use pen_script::{
    Edit, ExecuteRequest, ExecuteResponse, HeadlessHost, Limits, ScriptEngine, complete_prefix,
};
use serde_json::Value;

struct H {
    scene: SceneDoc,
    pipe: Pipeline,
    undo: UndoStack,
    eng: ScriptEngine,
}

const EMPTY: &str = r##"{ "version": "2.22", "children": [] }"##;

const COMPONENTS: &str = r##"{
  "version": "2.22",
  "children": [
    { "type": "frame", "id": "btn", "name": "Button", "reusable": true, "x": 0, "y": 0,
      "fill": "#ff0000", "padding": 8, "layout": "horizontal",
      "children": [
        { "type": "text", "id": "lbl", "name": "Label", "content": "Button", "fontSize": 12 },
        { "type": "frame", "id": "ico", "name": "Icon", "width": 10, "height": 10, "fill": "#00ff00" }
      ] },
    { "type": "frame", "id": "card", "name": "Card", "reusable": true, "x": 0, "y": 100,
      "layout": "vertical",
      "children": [
        { "type": "ref", "id": "cb", "ref": "btn" },
        { "type": "frame", "id": "body", "name": "Body", "slot": ["btn"], "layout": "vertical", "children": [] }
      ] },
    { "type": "ref", "id": "i1", "ref": "btn", "x": 300, "y": 0,
      "descendants": { "lbl": { "content": "OK" } } },
    { "type": "ref", "id": "i2", "ref": "card", "x": 300, "y": 100 }
  ]
}"##;

impl H {
    fn new(json: &str) -> H {
        let (mut scene, _) =
            SceneDoc::load(json.as_bytes(), None, &pen_doc::NoLibraries).expect("load");
        let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
        pipe.rebuild(&mut scene);
        H {
            scene,
            pipe,
            undo: UndoStack::new(),
            eng: ScriptEngine::new(),
        }
    }

    fn req(src: &str) -> ExecuteRequest {
        ExecuteRequest {
            agent_id: "agent".into(),
            description: "test".into(),
            input: Some(src.into()),
            ..Default::default()
        }
    }

    fn exec(&mut self, req: ExecuteRequest) -> ExecuteResponse {
        self.eng.execute(
            &mut self.scene,
            &mut self.pipe,
            &mut self.undo,
            &mut HeadlessHost,
            req,
        )
    }

    fn run(&mut self, src: &str) -> ExecuteResponse {
        self.exec(Self::req(src))
    }

    fn ok(&mut self, src: &str) -> ExecuteResponse {
        let r = self.run(src);
        assert!(r.ok, "snippet failed:\n{}\n---\n{src}", r.text);
        r
    }

    fn prints(&mut self, src: &str) -> Vec<String> {
        self.ok(src).prints
    }

    fn json(&mut self, src: &str) -> Value {
        let p = self.prints(src);
        serde_json::from_str(&p[0]).unwrap_or_else(|e| panic!("{e}: {}", p[0]))
    }
}

#[test]
fn insert_nested_children_with_layout_bounds() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
pageId = Insert(document, {type: "frame", name: "Page", x: 0, y: 0, width: 200, layout: "vertical",
  padding: 10, gap: 5, children: [
    {type: "rectangle", name: "A", width: 100, height: 20, fill: "#ff0000", id: "ignored"},
    {type: "rectangle", name: "B", width: "fill_container", height: 30, fill: "#00ff00"}
  ]})
const kids = Get(pageId, {depth: 1}).children
Print(kids.length, kids[0].name, kids[1].name)
Get(pageId, (n, c) => c.depth === 1 && Print(n.name, c.index, c.bounds.x, c.bounds.y, c.bounds.width, c.bounds.height))
"##);
    assert_eq!(r.prints[0], "2 A B");
    assert_eq!(r.prints[1], "A 0 10 10 100 20");
    assert_eq!(r.prints[2], "B 1 10 35 180 30");
    let names: Vec<&str> = r.created.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(names, ["Page", "A", "B"]);
    assert!(r.created.iter().all(|(_, id)| id != "ignored"));
    let page = &h.scene.document().children_slice()[0];
    assert_eq!(page.children_slice().len(), 2);
    assert!(r.text.contains("Page"), "{}", r.text);
    assert_eq!(h.undo.undo_len(), 1);
}

#[test]
fn update_move_copy_replace_delete() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
a = Insert(document, {type: "frame", name: "A", x: 0, y: 0, width: 100, height: 100, layout: "none"})
b = Insert(document, {type: "frame", name: "B", x: 200, y: 0, width: 100, height: 100})
r = Insert(a, {type: "rectangle", name: "R", x: 5, y: 5, width: 10, height: 10, fill: "#000000"})
Update(r, {fill: "#ffffff", x: 7, name: "R2"})
Move(r, b)
Move(b, undefined, 0)
c = Copy(a, document, {name: "A copy", x: 400})
Print(Get(r).fill, Get(r).x, Get(b, {depth: 1}).children[0].id === r)
t = Replace(r, {type: "text", name: "T", content: "hi", fontFamily: "Inter", fontSize: 12, fill: "#111111"})
Print(Get(b, {depth: 1}).children[0].type)
Delete(c)
"##);
    assert_eq!(r.prints[0], "#ffffff 7 true");
    assert_eq!(r.prints[1], "text");
    let doc = h.scene.document();
    let ids: Vec<&str> = doc
        .children_slice()
        .iter()
        .map(|n| n.name.as_deref().unwrap())
        .collect();
    assert_eq!(ids, ["B", "A"]);
    // Update with null removes a property.
    let b = doc.children_slice()[0].id.clone().unwrap();
    h.ok(&format!("Update({b:?}, {{width: null}})"));
    assert!(h.scene.schema_node(&b).unwrap().width.is_none());
}

#[test]
fn copy_regenerates_ids_and_applies_descendants() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
s = Insert(document, {type: "frame", name: "Screen", x: 0, y: 0, width: 300, height: 200, children: [
  {type: "text", name: "Title", content: "Hello", fontFamily: "Inter", fontSize: 20, fill: "#000000"},
  {type: "frame", name: "Box", width: 50, height: 50, fill: "#ff0000"}
]})
const title = Get(s, {depth: 1}).children[0].id
c = Copy(s, document, {name: "Screen 2", x: 400, descendants: {[title]: {content: "Bye"}, "Box": {fill: "#0000ff"}}})
const kids = Get(c, {depth: 1}).children
Print(kids[0].content, kids[1].fill, kids[0].id !== title)
"##);
    assert_eq!(r.prints[0], "Bye #0000ff true");
}

#[test]
fn components_instances_and_overrides() {
    let mut h = H::new(COMPONENTS);
    let r = h.ok(r##"
x = Insert(document, {type: "ref", ref: "btn", name: "Save", x: 0, y: 300,
  descendants: {lbl: {content: "Save"}, "Icon": {fill: "#0000ff"}}})
Print(Get(x + "/lbl").content, Get(x + "/ico").fill)
Update("i1/lbl", {content: "Changed"})
Print(Get("i1/lbl").content)
y = Copy("btn", document, {name: "From copy"})
Print(Get(y).type, Get(y).ref)
Replace("i1/ico", {type: "ellipse", name: "Dot", width: 6, height: 6, fill: "#123456"})
Print(Get("i1/ico").type)
Insert("i2/body", {type: "text", name: "Slot text", content: "In slot", fontFamily: "Inter", fontSize: 12, fill: "#000000"})
Update("i2/cb/lbl", {content: "Nested"})
Print(Get("i2", {resolveInstances: true, depth: 2}).children.map(c => c.id).join(","))
Print(Get("i2/cb/lbl").content)
"##);
    assert_eq!(r.prints[0], "Save #0000ff");
    assert_eq!(r.prints[1], "Changed");
    assert_eq!(r.prints[2], "ref btn");
    assert_eq!(r.prints[3], "ellipse");
    assert_eq!(r.prints[4], "i2/cb,i2/body");
    assert_eq!(r.prints[5], "Nested");
    let i1 = h.scene.schema_node("i1").unwrap().clone();
    let d = i1.descendants.unwrap();
    assert_eq!(
        d["lbl"].content.as_ref().unwrap().text().unwrap(),
        "Changed"
    );
    assert!(d["ico"].is_typed());
    let i2 = h.scene.schema_node("i2").unwrap().clone();
    let d = i2.descendants.unwrap();
    assert_eq!(d["body"].children_slice().len(), 1);
    assert!(d.contains_key("cb/lbl"));
    // Update merges `descendants` of an instance (keys may be names).
    let r = h.ok(r##"
Update("i2", {descendants: {"Body": {fill: "#abcdef"}, "cb/lbl": {fontSize: 20}}})
Print(Get("i2/body").fill, Get("i2/cb/lbl").content, Get("i2/cb/lbl").fontSize)
"##);
    assert_eq!(r.prints, ["#abcdef Nested 20"]);
    // Deleting inside an instance is refused with a hint.
    let r = h.run(r##"Delete("i1/lbl")"##);
    assert!(!r.ok);
    assert!(r.text.contains("enabled: false"), "{}", r.text);
}

#[test]
fn variables_and_themes() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
SetVariables({
  bg: {type: "color", value: [{value: "#ffffff", theme: {mode: "light"}}, {value: "#000000", theme: {mode: "dark"}}]},
  gap: {type: "number", value: 16}
})
f = Insert(document, {type: "frame", name: "F", x: 0, y: 0, width: 10, height: 10, fill: "$bg", gap: "$gap"})
Print(GetVariables().themes)
Print(Get(f, {resolveVariables: true}).fill, Get(f, {resolveVariables: true}).gap, Get(f).fill)
SetVariables({gap: {type: "number", value: 8}})
Print(Object.keys(GetVariables().variables).join(","), GetVariables().variables.gap.value)
"##);
    assert_eq!(r.prints[0], r##"{"mode":["light","dark"]}"##);
    assert_eq!(r.prints[1], "#ffffff 16 $bg");
    assert_eq!(r.prints[2], "bg,gap 8");
    let bad = h.run(r##"SetVariables({"$x": {type: "color", value: "#fff"}})"##);
    assert!(
        !bad.ok && bad.text.contains("must not start with '$'"),
        "{}",
        bad.text
    );
    let bad = h.run(r##"SetVariables({x: "#fff"})"##);
    assert!(
        !bad.ok && bad.text.contains("{type, value}"),
        "{}",
        bad.text
    );
    h.ok(r##"SetVariables({only: {type: "string", value: "x"}}, true)"##);
    let v = h.scene.document().variables.clone().unwrap();
    assert_eq!(v.keys().collect::<Vec<_>>(), ["only"]);
}

#[test]
fn one_undo_step_restores_byte_identical_save() {
    let mut h = H::new(COMPONENTS);
    let before = h.scene.save();
    h.ok(r##"
a = Insert(document, {type: "frame", name: "A", x: 0, y: 500, width: 100, height: 100})
Insert(a, {type: "text", name: "T", content: "x", fontFamily: "Inter", fontSize: 10, fill: "#000000"})
Update("i1", {x: 333})
Update("i1/lbl", {content: "New"})
Update("btn", {fill: "#00ffff"})
Move("i2", undefined, 0)
SetVariables({c: {type: "color", value: "#123456"}})
Delete("card")
"##);
    assert_eq!(h.undo.undo_len(), 1);
    assert_ne!(h.scene.save(), before);
    assert!(h.undo.undo(&mut h.scene).unwrap());
    h.pipe.update(&mut h.scene);
    assert_eq!(h.scene.save(), before);
    assert!(h.undo.redo(&mut h.scene).unwrap());
    assert!(h.scene.schema_node("card").is_none());
}

#[test]
fn error_rolls_back_and_reports_location() {
    let mut h = H::new(COMPONENTS);
    let before = h.scene.save();
    let r = h.run(
        "a = Insert(document, {type: \"frame\", name: \"A\"})\nUpdate(a, {fill: \"#fff\"})\n  Update(\"nope\", {fill: \"#000\"})\nPrint(\"never\")",
    );
    assert!(!r.ok);
    let e = r.error.clone().unwrap();
    assert_eq!((e.line, e.column), (Some(3), Some(3)), "{}", r.text);
    assert_eq!(
        e.call.as_deref(),
        Some("Update(\"nope\", {fill: \"#000\"})")
    );
    assert!(r.text.contains("node not found: 'nope'"), "{}", r.text);
    assert!(r.edit_id.is_some());
    assert!(r.prints.is_empty());
    assert_eq!(h.scene.save(), before);
    assert_eq!(h.undo.undo_len(), 0);
    // QuickJS may point inside the arguments; the call text still starts at the call.
    let r = h.run("a = \"nope\"\nUpdate(a + \"/x\", {fill: \"#000\"})");
    assert!(!r.ok);
    let e = r.error.clone().unwrap();
    assert_eq!(
        e.call.as_deref(),
        Some("Update(a + \"/x\", {fill: \"#000\"})"),
        "{}",
        r.text
    );
    // Syntax errors too.
    let r = h.run("x = 1\nInsert(document, {type: \"frame\",, })");
    assert!(!r.ok);
    assert_eq!(r.error.unwrap().line, Some(2), "{}", r.text);
    // Globals of a failed call are discarded.
    let r = h.run("lost = 5\nthrow new Error(\"boom\")");
    assert!(!r.ok && r.text.contains("boom"));
    let r = h.ok("Print(typeof lost)");
    assert_eq!(r.prints, ["undefined"]);
    // Thrown non-API errors keep their type.
    let r = h.run("null.x");
    assert!(r.text.contains("TypeError"), "{}", r.text);
}

#[test]
fn edit_id_patch_flow() {
    let mut h = H::new(EMPTY);
    let r = h.run("f = Insert(document, {type: \"frame\", name: \"F\", width: 10, height: 10})\nUpdate(\"missing\", {x: 1})\nPrint(\"done\")");
    assert!(!r.ok);
    let eid = r.edit_id.clone().unwrap();
    // A non-matching find leaves the stored snippet alone.
    let bad = h.exec(ExecuteRequest {
        agent_id: "agent".into(),
        edit_id: Some(eid.clone()),
        edits: vec![Edit {
            find: "nothing like this".into(),
            replace: "x".into(),
            all: false,
        }],
        ..Default::default()
    });
    assert!(!bad.ok && bad.text.contains("not found"), "{}", bad.text);
    // Ambiguous find.
    let bad = h.exec(ExecuteRequest {
        agent_id: "agent".into(),
        edit_id: Some(eid.clone()),
        edits: vec![Edit {
            find: "10".into(),
            replace: "20".into(),
            all: false,
        }],
        ..Default::default()
    });
    assert!(!bad.ok && bad.text.contains("matches"), "{}", bad.text);
    // First fix fails again (still the same editId, patched text kept).
    let r2 = h.exec(ExecuteRequest {
        agent_id: "agent".into(),
        edit_id: Some(eid.clone()),
        edits: vec![Edit {
            find: "\"missing\"".into(),
            replace: "\"stillmissing\"".into(),
            all: false,
        }],
        ..Default::default()
    });
    assert!(!r2.ok);
    assert_eq!(r2.edit_id.as_deref(), Some(eid.as_str()));
    let r3 = h.exec(ExecuteRequest {
        agent_id: "agent".into(),
        edit_id: Some(eid.clone()),
        edits: vec![Edit {
            find: "\"stillmissing\"".into(),
            replace: "f".into(),
            all: false,
        }],
        ..Default::default()
    });
    assert!(r3.ok, "{}", r3.text);
    assert_eq!(r3.prints, ["done"]);
    assert_eq!(h.scene.document().children_slice().len(), 1);
    // The edit id is consumed.
    let gone = h.exec(ExecuteRequest {
        agent_id: "agent".into(),
        edit_id: Some(eid),
        edits: vec![],
        ..Default::default()
    });
    assert!(!gone.ok && gone.text.contains("unknown editId"));
}

#[test]
fn globals_persist_per_agent() {
    let mut h = H::new(EMPTY);
    h.ok("myId = Insert(document, {type: \"frame\", name: \"F\"})\nconst local = 1\nlet l2 = 2\nobj = {a: [1, 2]}\nfn = function () {}\n");
    let r = h.ok("Print(typeof myId, typeof local, typeof l2, JSON.stringify(obj), typeof fn)");
    assert_eq!(
        r.prints,
        ["string undefined undefined {\"a\":[1,2]} undefined"]
    );
    let id = h.scene.document().children_slice()[0].id.clone().unwrap();
    let r = h.ok("Print(myId)");
    assert_eq!(r.prints, [id]);
    // Another agent does not see them.
    let r = h.exec(ExecuteRequest {
        agent_id: "other".into(),
        input: Some("Print(typeof myId)".into()),
        ..Default::default()
    });
    assert_eq!(r.prints, ["undefined"]);
    // API names cannot be clobbered.
    let r = h.ok("Insert = 5; Print(typeof Insert)");
    assert_eq!(r.prints, ["function"]);
}

#[test]
fn get_visitor_skip_children_and_problems() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
p = Insert(document, {type: "frame", name: "P", x: 0, y: 0, width: 100, height: 100, layout: "none", children: [
  {type: "frame", name: "Inside", x: 10, y: 10, width: 20, height: 20, children: [
    {type: "rectangle", name: "Deep", width: 5, height: 5}
  ]},
  {type: "rectangle", name: "Half", x: 90, y: 10, width: 20, height: 20},
  {type: "rectangle", name: "Out", x: 200, y: 10, width: 20, height: 20}
]})
Get(p, (n, c) => c.problems && Print(n.name, c.problems, c.parentCtx.node.name))
const names = Get(p, (n, c) => { if (n.name === "Inside") c.skipChildren(); return n.name })
Print(names.join(","))
Print(Get(n => n.type === "rectangle" ? n.name : undefined).join(","))
Print(Get(p, n => n.name, {depth: 1}).length)
"##);
    assert_eq!(r.prints[0], "Half partially clipped P");
    assert_eq!(r.prints[1], "Out fully clipped P");
    assert_eq!(r.prints[2], "P,Inside,Half,Out");
    assert_eq!(r.prints[3], "Deep,Half,Out");
    assert_eq!(r.prints[4], "4");
    // Whole-document read without visitor is refused.
    let r = h.run("Get()");
    assert!(!r.ok && r.text.contains("without a visitor"), "{}", r.text);
    let r = h.run("Get(document)");
    assert!(!r.ok);
    // Passing a node object instead of its id.
    let r = h.run(&format!(
        "Update(Get({:?}), {{x: 1}})",
        h.scene.document().children_slice()[0].id_str()
    ));
    assert!(!r.ok && r.text.contains("pass its .id"), "{}", r.text);
}

#[test]
fn invalid_properties_become_warnings() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
f = Insert(document, {type: "frame", name: "F", width: "huge", bogus: 1, fontSize: 12, fill: "#ff0000"})
Update(f, {height: "tall", alsoBogus: true})
"##);
    let w = r.warnings.join("\n");
    assert!(w.contains("invalid value for 'width'"), "{w}");
    assert!(w.contains("unknown property 'bogus'"), "{w}");
    assert!(w.contains("fontSize not allowed on frame"), "{w}");
    assert!(w.contains("invalid value for 'height'"), "{w}");
    assert!(w.contains("unknown property 'alsoBogus'"), "{w}");
    assert!(r.text.contains("Warnings"));
    let n = h.scene.document().children_slice()[0].clone();
    assert!(n.width.is_none() && !n.extra.contains_key("width"));
    // Unknown types are errors.
    let r = h.run(r##"Insert(document, {type: "image", name: "I"})"##);
    assert!(!r.ok && r.text.contains("no image node"), "{}", r.text);
    let r = h.run(r##"Insert(document, {name: "no type"})"##);
    assert!(!r.ok && r.text.contains("\"type\""), "{}", r.text);
}

#[test]
fn limits_interrupt_loops_and_allocations() {
    let mut h = H::new(EMPTY);
    h.eng.limits = Limits {
        cpu_per_chunk: Duration::from_millis(300),
        memory_bytes: 64 * 1024 * 1024,
        ..Limits::default()
    };
    let before = h.scene.save();
    let t = std::time::Instant::now();
    let r = h.run("Insert(document, {type: \"frame\", name: \"F\"})\nwhile (true) {}");
    assert!(!r.ok);
    assert!(r.text.contains("CPU budget"), "{}", r.text);
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(h.scene.save(), before);

    let r = h.run("const a = []; for (;;) a.push(new Array(1000000).fill(1.5))");
    assert!(!r.ok);
    assert!(r.text.to_lowercase().contains("memory"), "{}", r.text);
    assert_eq!(h.scene.save(), before);

    let r = h.run("function f(n) { return f(n + 1) + 1 }\nf(0)");
    assert!(!r.ok);
    assert!(r.text.contains("stack"), "{}", r.text);

    // Host time does not count: many host calls under a small CPU budget still pass.
    h.ok("for (let i = 0; i < 300; i++) Insert(document, {type: \"rectangle\", name: \"R\" + i, width: 1, height: 1})");

    // The total timeout also stops the run.
    h.eng.limits.total = Duration::from_millis(200);
    h.eng.limits.cpu_per_chunk = Duration::from_secs(10);
    let r = h.run("while (true) {}");
    assert!(!r.ok && r.text.contains("timed out"), "{}", r.text);
}

#[test]
fn screenshot_and_export_png() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
f = Insert(document, {type: "frame", name: "F", x: 0, y: 0, width: 40, height: 30, fill: "#ff0000"})
TakeScreenshot([f])
TakeScreenshot(["document"])
"##);
    assert_eq!(r.images.len(), 2);
    for img in &r.images {
        assert_eq!(&img[..8], b"\x89PNG\r\n\x1a\n");
    }
    let dec = image::load_from_memory(&r.images[0]).unwrap();
    assert_eq!((dec.width(), dec.height()), (40, 30));
    let dir = std::env::temp_dir().join(format!("pen-script-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = dir.to_string_lossy().replace('\\', "/");
    let id = h.scene.document().children_slice()[0].id.clone().unwrap();
    let r = h.ok(&format!(
        "Export([{id:?}], \"png\", {out:?})\nExport([{id:?}], \"jpeg\", {out:?}, {{scale: 1, quality: 80}})"
    ));
    assert_eq!(r.exported.len(), 2);
    let png = image::open(&r.exported[0]).unwrap();
    assert_eq!((png.width(), png.height()), (80, 60));
    assert!(r.exported[1].extension().unwrap() == "jpg");
    // Vector formats (pen-export): default scale 1 = document units.
    let r = h.ok(&format!(
        "Export([{id:?}], \"svg\", {out:?})
Export([{id:?}, \"document\"], \"pdf\", {out:?})"
    ));
    assert_eq!(r.exported.len(), 3, "{:?}", r.exported);
    let svg = std::fs::read_to_string(&r.exported[0]).unwrap();
    assert!(svg.starts_with("<svg") || svg.starts_with("<?xml"), "{svg}");
    assert!(
        svg.contains("width=\"40\"") && svg.contains("#ff0000"),
        "{svg}"
    );
    for pdf in &r.exported[1..] {
        assert_eq!(pdf.extension().unwrap(), "pdf");
        assert!(std::fs::read(pdf).unwrap().starts_with(b"%PDF-"));
    }
    assert!(r.exported[2].ends_with("document.pdf"));
    assert!(r.text.contains(".svg"), "{}", r.text);
    let _ = std::fs::remove_dir_all(&dir);
    // Code export (pen-codegen): a directory gets index.html (+ styles.css).
    let r = h.ok(&format!("Export([{id:?}], \"html-css\", {out:?})"));
    assert_eq!(r.exported.len(), 2, "{:?}", r.exported);
    assert!(r.exported[0].ends_with("index.html"));
    let css = std::fs::read_to_string(&r.exported[1]).unwrap();
    assert!(css.contains("background-color: #ff0000;"), "{css}");
    let html_file = format!("{out}/tw/page.html");
    let r = h.ok(&format!(
        "Export([{id:?}], \"html-tailwind\", {html_file:?})"
    ));
    assert_eq!(r.exported.len(), 1, "{:?}", r.exported);
    let html = std::fs::read_to_string(&r.exported[0]).unwrap();
    assert!(html.contains("bg-[#ff0000]"), "{html}");
    let _ = std::fs::remove_dir_all(&dir);
    let r = h.run("Generate(\"ai\", \"x\", \"a cat\")");
    assert!(!r.ok && r.text.contains("not supported"), "{}", r.text);
}

#[test]
fn find_empty_space() {
    let mut h = H::new(EMPTY);
    let r = h.ok(r##"
a = Insert(document, {type: "frame", name: "A", x: 0, y: 0, width: 100, height: 100})
b = Insert(document, {type: "frame", name: "B", x: 150, y: 0, width: 100, height: 100})
const p = FindEmptySpace({width: 50, height: 50, padding: 10, nodeId: a})
Print(p.x, p.y, p.parentId === undefined)
const q = FindEmptySpace({width: 50, height: 50, direction: "bottom", padding: 20})
Print(q.x, q.y)
"##);
    assert_eq!(r.prints[0], "260 0 true");
    assert_eq!(r.prints[1], "0 120");
}

#[test]
fn streaming_evaluates_complete_statements_then_commits_once() {
    let mut h = H::new(EMPTY);
    let src = "a = Insert(document, {type: \"frame\", name: \"A\", width: 10, height: 10})\nb = Insert(document, {type: \"frame\", name: \"B\"})\nPrint(Get(a).name)\n";
    let mut evaluated = 0;
    for cut in [10, 40, 80, 100, src.len() - 2] {
        let s = h
            .eng
            .execute_partial(&mut h.scene, &mut h.pipe, "agent", &src[..cut]);
        assert!(s.error.is_none());
        assert!(s.evaluated >= evaluated);
        evaluated = s.evaluated;
        if cut >= 80 {
            assert!(!h.scene.document().children_slice().is_empty());
        }
    }
    assert_eq!(h.undo.undo_len(), 0);
    let r = h.ok(src);
    assert_eq!(r.prints, ["A"]);
    assert_eq!(h.scene.document().children_slice().len(), 2);
    assert_eq!(h.undo.undo_len(), 1);
    assert!(h.undo.undo(&mut h.scene).unwrap());
    assert!(h.scene.document().children_slice().is_empty());

    // A cancelled stream leaves nothing behind.
    h.eng.execute_partial(
        &mut h.scene,
        &mut h.pipe,
        "agent",
        "Insert(document, {type: \"frame\"})\nx",
    );
    assert_eq!(h.scene.document().children_slice().len(), 1);
    assert!(h.eng.cancel_stream(&mut h.scene, &mut h.pipe, "agent"));
    assert!(h.scene.document().children_slice().is_empty());
    assert_eq!(complete_prefix("a()\nb"), 4);
}

#[test]
fn engine_is_send() {
    fn is_send<T: Send>() {}
    is_send::<ScriptEngine>();
    // And it can be moved to another thread and used there.
    let mut h = H::new(EMPTY);
    let r = std::thread::spawn(move || {
        let r = h.ok("Print(1 + 1)");
        r.prints
    })
    .join()
    .unwrap();
    assert_eq!(r, ["2"]);
}

#[test]
fn deep_recursion_is_safe_on_a_1mib_thread() {
    // The Windows main thread (where the GUI calls `execute`) has 1 MiB of stack.
    let r = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut h = H::new(EMPTY);
            let r = h.run("function f(n) { return f(n + 1) + 1 }\nf(0)");
            let deep = h.ok(
                "const d = n => n === 0 ? Insert(document, {type: \"frame\", name: \"F\"}) : d(n - 1)\nd(80)",
            );
            (r.ok, r.text, deep.ok)
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(!r.0 && r.1.contains("stack"), "{}", r.1);
    assert!(r.2);
}

#[test]
fn json_print_formats() {
    let mut h = H::new(EMPTY);
    let v = h.json("Print({a: 1, b: [true, null]})");
    assert_eq!(v, serde_json::json!({"a": 1, "b": [true, null]}));
    let r = h.ok("Print(\"x\", 1, undefined, [1])");
    assert_eq!(r.prints, ["x 1 undefined [1]"]);
}

#[test]
fn literal_dollar_text_warns_and_escape_is_silent() {
    let mut h = H::new(EMPTY);
    h.ok(r##"SetVariables({brand: {type: "color", value: "#336699"}})"##);
    let r = h.ok(
        r##"price = Insert(document, {type: "text", name: "Price", content: "$18,420", fill: "$brand"})"##,
    );
    let hits: Vec<_> = r
        .warnings
        .iter()
        .filter(|w| w.contains("undefined variable"))
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", r.warnings);
    assert!(hits[0].contains("'$18,420'") && hits[0].contains(r"\$18,420"));
    assert!(hits[0].starts_with("Price: "), "{}", hits[0]);

    // Update is checked too; an escaped literal and a defined variable are silent.
    let r = h.ok(r##"Update(price, {content: "$9"})"##);

    assert!(
        r.warnings.iter().any(|w| w.contains("'$9'")),
        "{:?}",
        r.warnings
    );
    let r = h.ok(
        r##"Update(price, {content: "\\$18,420", fill: "$brand"}); Print(Get(price).content)"##,
    );
    assert_eq!(r.prints, [r"\$18,420"]);
    assert!(
        !r.warnings.iter().any(|w| w.contains("undefined variable")),
        "{:?}",
        r.warnings
    );
}
