use pen_doc::*;
use pen_render::Color;
use pen_schema::{
    Bind, ColorOrVar, Fill, Node, NodeField, OneOrMany, Sizing, StrOrVar, TextContent,
};

fn doc(json: &str) -> pen_schema::Document {
    pen_schema::load_document(json.as_bytes()).unwrap().0
}

fn scene(json: &str) -> SceneDoc {
    SceneDoc::new(doc(json))
}

fn solid(s: &SceneDoc, k: NodeKey) -> Color {
    match &s.get(k).unwrap().fills[0].paint {
        PaintSpec::Solid(c) => *c,
        p => panic!("not solid: {p:?}"),
    }
}

fn text(s: &SceneDoc, k: NodeKey) -> String {
    s.get(k).unwrap().text().unwrap().content.to_string()
}

fn hex(c: &str) -> Color {
    let r = pen_schema::Rgba::parse_hex(c).unwrap();
    Color::rgba(
        r.r as f32 / 255.0,
        r.g as f32 / 255.0,
        r.b as f32 / 255.0,
        r.a as f32 / 255.0,
    )
}

const COMPONENTS: &str = r##"{
  "version": "2.22",
  "children": [
    { "type": "frame", "id": "btn", "reusable": true, "x": 0, "y": 0, "fill": "#ff0000",
      "padding": 8, "layout": "horizontal",
      "children": [
        { "type": "text", "id": "lbl", "content": "Button", "fontSize": 12 },
        { "type": "frame", "id": "ico", "width": 10, "height": 10, "fill": "#00ff00" }
      ] },
    { "type": "frame", "id": "card", "reusable": true, "x": 0, "y": 100, "layout": "vertical",
      "children": [
        { "type": "ref", "id": "cb", "ref": "btn" },
        { "type": "frame", "id": "body", "slot": ["btn"], "layout": "vertical", "children": [] }
      ] },
    { "type": "ref", "id": "i1", "ref": "btn", "x": 300, "y": 0, "fill": "#0000ff",
      "descendants": { "lbl": { "content": "OK" }, "ico": { "enabled": false } } },
    { "type": "ref", "id": "i2", "ref": "card", "x": 300, "y": 100,
      "descendants": {
        "cb/lbl": { "content": "Nested" },
        "body": { "children": [ { "type": "text", "id": "slotText", "content": "In slot" } ] }
      } },
    { "type": "ref", "id": "i3", "ref": "btn", "x": 300, "y": 300,
      "descendants": { "ico": { "type": "rectangle", "id": "rep", "width": 5, "height": 5, "fill": "#123456" } } },
    { "type": "ref", "id": "v1", "ref": "btn", "reusable": true, "x": 600, "y": 0,
      "descendants": { "lbl": { "content": "Variant" } } },
    { "type": "ref", "id": "i4", "ref": "v1", "x": 600, "y": 100 },
    { "type": "ref", "id": "bad", "ref": "nope", "x": 0, "y": 500, "width": 40, "height": 20 }
  ]
}"##;

#[test]
fn instance_root_overrides_and_patches() {
    let s = scene(COMPONENTS);
    let i1 = s.find("i1").unwrap();
    let n = s.get(i1).unwrap();
    assert_eq!(n.kind, Kind::Frame);
    assert!(n.flags.has(NodeFlags::INSTANCE));
    assert!(!n.flags.has(NodeFlags::REUSABLE));
    assert_eq!((n.x, n.y), (300.0, 0.0));
    assert_eq!(n.layout.padding, [8.0; 4]);
    assert_eq!(solid(&s, i1), hex("#0000ff"));
    let lbl = s.find("i1/lbl").unwrap();
    assert_eq!(text(&s, lbl), "OK");
    assert_eq!(s.get(lbl).unwrap().text().unwrap().size, 12.0);
    let ico = s.find("i1/ico").unwrap();
    assert!(!s.get(ico).unwrap().enabled());
    // The component itself is untouched.
    assert_eq!(text(&s, s.find("lbl").unwrap()), "Button");
    assert_eq!(s.id_path(lbl).unwrap(), "i1/lbl");
    assert_eq!(
        s.node_ref(lbl),
        Some(NodeRef::Override {
            instance: "i1".into(),
            path: "lbl".into()
        })
    );
}

#[test]
fn nested_paths_and_slots() {
    let s = scene(COMPONENTS);
    let nested = s.find("i2/cb/lbl").unwrap();
    assert_eq!(text(&s, nested), "Nested");
    let body = s.find("i2/body").unwrap();
    let kids = s.children(body);
    assert_eq!(kids.len(), 1);
    assert_eq!(text(&s, kids[0]), "In slot");
    assert!(s.get(body).unwrap().flags.has(NodeFlags::SLOT));
    // The component's slot stays empty.
    assert!(s.children(s.find("body").unwrap()).is_empty());
}

#[test]
fn subtree_replacement() {
    let s = scene(COMPONENTS);
    let i3 = s.find("i3").unwrap();
    let kids = s.children(i3);
    assert_eq!(kids.len(), 2);
    let rep = s.get(kids[1]).unwrap();
    assert_eq!(rep.kind, Kind::Rectangle);
    assert_eq!(&*rep.id, "rep");
    assert_eq!(rep.width, SizeSpec::Fixed(5.0));
    assert_eq!(solid(&s, kids[1]), hex("#123456"));
    // Addressed by the replaced id.
    assert_eq!(s.find("i3/ico"), Some(kids[1]));
}

#[test]
fn variants_and_broken_refs() {
    let s = scene(COMPONENTS);
    let i4 = s.find("i4").unwrap();
    assert_eq!(text(&s, s.children(i4)[0]), "Variant");
    assert_eq!((s.get(i4).unwrap().x, s.get(i4).unwrap().y), (600.0, 100.0));
    assert!(!s.get(i4).unwrap().flags.has(NodeFlags::REUSABLE));
    assert!(
        s.get(s.find("v1").unwrap())
            .unwrap()
            .flags
            .has(NodeFlags::REUSABLE)
    );
    let bad = s.get(s.find("bad").unwrap()).unwrap();
    assert_eq!(bad.kind, Kind::BrokenRef);
    assert_eq!(bad.width, SizeSpec::Fixed(40.0));
    assert!(s.warnings().iter().any(|w| w.contains("nope")));
}

#[test]
fn reference_cycle_is_broken() {
    let s = scene(
        r#"{"version":"2.22","children":[
        {"type":"frame","id":"a","reusable":true,"children":[{"type":"ref","id":"x","ref":"b"}]},
        {"type":"frame","id":"b","reusable":true,"children":[{"type":"ref","id":"y","ref":"a"}]}
    ]}"#,
    );
    assert!(s.node_count() < 20);
    assert!(s.warnings().iter().any(|w| w.contains("cycle")));
}

const THEMED: &str = r##"{
  "version": "2.22",
  "themes": { "Mode": ["Light", "Dark"], "Base": ["Zinc", "Slate"] },
  "variables": {
    "bg": { "type": "color", "value": [
      { "value": "#ffffff" },
      { "value": "#000000", "theme": { "Mode": "Dark" } },
      { "value": "#020617", "theme": { "Mode": "Dark", "Base": "Slate" } } ] },
    "gap": { "type": "number", "value": 12 },
    "alias": { "type": "color", "value": "$bg" },
    "label": { "type": "string", "value": "Hello" },
    "on": { "type": "boolean", "value": false }
  },
  "children": [
    { "type": "frame", "id": "light", "fill": "$bg", "gap": "$gap", "children": [
      { "type": "frame", "id": "dark", "theme": { "Mode": "Dark" }, "fill": "$alias", "children": [
        { "type": "frame", "id": "slate", "theme": { "Base": "Slate" }, "fill": "$bg" } ] },
      { "type": "text", "id": "t", "content": "$label", "enabled": "$on" },
      { "type": "text", "id": "price", "content": "$67,890" } ] }
  ]
}"##;

#[test]
fn variables_and_themes() {
    let s = scene(THEMED);
    assert_eq!(solid(&s, s.find("light").unwrap()), hex("#ffffff"));
    assert_eq!(s.get(s.find("light").unwrap()).unwrap().layout.gap, 12.0);
    assert_eq!(solid(&s, s.find("dark").unwrap()), hex("#000000"));
    // Inherited Mode=Dark + own Base=Slate: the last matching value wins.
    assert_eq!(solid(&s, s.find("slate").unwrap()), hex("#020617"));
    let t = s.find("t").unwrap();
    assert_eq!(text(&s, t), "Hello");
    assert!(!s.get(t).unwrap().enabled());
    // Unknown variable in content stays literal.
    assert_eq!(text(&s, s.find("price").unwrap()), "$67,890");
}

#[test]
fn active_theme_selection() {
    let mut s = scene(THEMED);
    s.set_theme("Mode", "Dark");
    assert_eq!(solid(&s, s.find("light").unwrap()), hex("#000000"));
    s.set_theme("Base", "Slate");
    assert_eq!(solid(&s, s.find("light").unwrap()), hex("#020617"));
    assert_eq!(
        s.resolve_variable("gap", None).and_then(|v| v.as_number()),
        Some(12.0)
    );
}

#[test]
fn library_imports() {
    let lib = doc(r##"{"version":"2.22",
        "themes": {"Mode": ["Light", "Dark"]},
        "variables": {"accent": {"type": "color", "value": [{"value": "#111111"}, {"value": "#eeeeee", "theme": {"Mode": "Dark"}}]}},
        "children":[
          {"type":"frame","id":"Comp","reusable":true,"fill":"$accent","children":[
            {"type":"text","id":"tx","content":"lib"},
            {"type":"ref","id":"inner","ref":"Small"}]},
          {"type":"frame","id":"Small","reusable":true,"width":4,"height":4,"fill":"#ababab"}
        ]}"##);
    let main = doc(r##"{"version":"2.22",
        "imports": {"ui": "./ui.lib.pen", "gone": "./missing.lib.pen"},
        "variables": {"mine": {"type": "color", "value": "$ui:accent"}},
        "children":[
          {"type":"ref","id":"a","ref":"ui:Comp","descendants":{"ui:tx":{"content":"patched"},"ui:inner":{"fill":"$mine"}}},
          {"type":"ref","id":"b","ref":"ui:Comp","theme":{"ui:Mode":"Dark"}},
          {"type":"ref","id":"c","ref":"gone:X"}
        ]}"##);
    let mut libs = MapLibraries::default();
    libs.0.insert("./ui.lib.pen".into(), lib);
    let s = SceneDoc::with_libraries(main, None, &libs);
    assert_eq!(s.libraries().len(), 2);
    assert!(s.libraries()[1].doc.is_none());
    let a = s.find("a").unwrap();
    assert_eq!(solid(&s, a), hex("#111111"));
    let kids = s.children(a);
    assert_eq!(&*s.get(kids[0]).unwrap().id, "ui:tx");
    assert_eq!(text(&s, kids[0]), "patched");
    assert_eq!(solid(&s, kids[1]), hex("#111111"));
    assert_eq!(s.id_path(kids[0]).unwrap(), "a/ui:tx");
    let b = s.find("b").unwrap();
    assert_eq!(solid(&s, b), hex("#eeeeee"));
    assert_eq!(s.get(s.find("c").unwrap()).unwrap().kind, Kind::BrokenRef);
}

fn fill(c: &str) -> Node {
    Node {
        fill: Some(OneOrMany::One(Fill::Solid(ColorOrVar(c.into())))),
        ..Node::default()
    }
}

#[test]
fn set_fields_inverse_and_dirty() {
    let mut s = scene(THEMED);
    let before = s.save();
    let _ = s.take_changes();
    let light = s.find("light").unwrap();
    let inv = s
        .apply(DocOp::set(NodeRef::id("light"), fill("#ff00ff")))
        .unwrap();
    assert_eq!(solid(&s, light), hex("#ff00ff"));
    let ch = s.take_changes();
    assert_eq!(ch.dirty, vec![(light, Dirty::PAINT)]);
    // Layout-affecting change.
    let gap = Node {
        gap: Some(Bind::Value(3.0)),
        ..Node::default()
    };
    let inv2 = s.apply(DocOp::set(NodeRef::id("light"), gap)).unwrap();
    assert_eq!(s.take_changes().dirty, vec![(light, Dirty::LAYOUT)]);
    s.apply(inv2).unwrap();
    s.apply(inv).unwrap();
    assert_eq!(s.save(), before);
    assert_eq!(s.find("light"), Some(light), "keys are stable");
}

#[test]
fn override_edits_create_and_remove_entries() {
    let mut s = scene(COMPONENTS);
    let before = s.save();
    let k = s.find("i1/ico").unwrap();
    let target = s.node_ref(s.find("i1/lbl").unwrap()).unwrap();
    let values = Node {
        content: Some(TextContent::Plain(StrOrVar("Changed".into()))),
        ..Node::default()
    };
    let inv = s.apply(DocOp::set(target, values)).unwrap();
    assert_eq!(text(&s, s.find("i1/lbl").unwrap()), "Changed");
    assert_eq!(s.find("i1/ico"), Some(k), "keys reused across rebuild");
    // New override entry on a node without one.
    let t2 = NodeRef::Override {
        instance: "i1".into(),
        path: "lbl".into(),
    };
    let inv_w = s
        .apply(DocOp::set(
            t2,
            Node {
                width: Some(Sizing::Fixed(77.0)),
                ..Node::default()
            },
        ))
        .unwrap();
    assert_eq!(
        s.get(s.find("i1/lbl").unwrap()).unwrap().width,
        SizeSpec::Fixed(77.0)
    );
    s.apply(inv_w).unwrap();
    s.apply(inv).unwrap();
    assert_eq!(s.save(), before);
}

#[test]
fn component_edit_updates_instances() {
    let mut s = scene(COMPONENTS);
    s.apply(DocOp::set(NodeRef::id("ico"), fill("#abcdef")))
        .unwrap();
    // i2's nested `cb` instance sees the component change too.
    assert_eq!(solid(&s, s.find("i2/cb/ico").unwrap()), hex("#abcdef"));
    assert_eq!(solid(&s, s.find("i4/ico").unwrap()), hex("#abcdef"));
    // i1 overrides `fill` on the root only.
    assert_eq!(solid(&s, s.find("i1/ico").unwrap()), hex("#abcdef"));
}

#[test]
fn structural_ops_and_undo_blocks() {
    let mut s = scene(COMPONENTS);
    let before = s.save();
    let mut undo = UndoStack::new();
    undo.begin();
    let mut n = Node::new(pen_schema::NodeKind::Rectangle, "newR");
    n.width = Some(Sizing::Fixed(10.0));
    undo.apply(
        &mut s,
        DocOp::Insert {
            parent: Some("btn".into()),
            index: 1,
            node: Box::new(n),
        },
    )
    .unwrap();
    undo.apply(
        &mut s,
        DocOp::Insert {
            parent: None,
            index: 0,
            node: Box::new(Node::new(pen_schema::NodeKind::Frame, "top")),
        },
    )
    .unwrap();
    undo.commit();
    assert_eq!(undo.undo_len(), 1);
    assert_eq!(s.get(s.roots()[0]).unwrap().id.as_ref(), "top");
    assert_eq!(
        s.children(s.find("i1").unwrap()).len(),
        3,
        "instances see the insert"
    );
    undo.apply(
        &mut s,
        DocOp::Reorder {
            id: "ico".into(),
            index: 0,
        },
    )
    .unwrap();
    assert_eq!(
        &*s.get(s.children(s.find("btn").unwrap())[0]).unwrap().id,
        "ico"
    );
    undo.apply(
        &mut s,
        DocOp::Move {
            id: "newR".into(),
            parent: Some("body".into()),
            index: 0,
        },
    )
    .unwrap();
    assert_eq!(s.children(s.find("body").unwrap()).len(), 1);
    assert_eq!(
        s.apply(DocOp::Move {
            id: "card".into(),
            parent: Some("body".into()),
            index: 0
        }),
        Err(OpError::Cycle)
    );
    undo.apply(&mut s, DocOp::Delete { id: "btn".into() })
        .unwrap();
    assert_eq!(s.get(s.find("i1").unwrap()).unwrap().kind, Kind::BrokenRef);
    while undo.undo(&mut s).unwrap() {}
    assert_eq!(s.save(), before);
    assert_eq!(s.get(s.find("i1").unwrap()).unwrap().kind, Kind::Frame);
    // Redo everything and undo again.
    while undo.redo(&mut s).unwrap() {}
    assert!(s.find("btn").is_none());
    while undo.undo(&mut s).unwrap() {}
    assert_eq!(s.save(), before);
}

#[test]
fn set_variable_and_clear() {
    let mut s = scene(THEMED);
    let before = s.save();
    let inv = s
        .apply(DocOp::SetVariable {
            name: "gap".into(),
            value: None,
            index: None,
        })
        .unwrap();
    assert_eq!(s.get(s.find("light").unwrap()).unwrap().layout.gap, 0.0);
    s.apply(inv).unwrap();
    assert_eq!(s.save(), before);
    let inv = s
        .apply(DocOp::clear(NodeRef::id("light"), &[NodeField::fill]))
        .unwrap();
    assert!(s.get(s.find("light").unwrap()).unwrap().fills.is_empty());
    s.apply(inv).unwrap();
    assert_eq!(s.save(), before);
}

#[test]
fn save_keeps_line_endings_and_trailing_newline() {
    let lf = "{\n  \"version\": \"2.22\",\n  \"children\": [\n    {\n      \"type\": \"rectangle\",\n      \"id\": \"r\"\n    }\n  ]\n}";
    for src in [
        lf.to_string(),
        format!("{}\r\n", lf.replace('\n', "\r\n")),
        format!("{lf}\n"),
    ] {
        let (s, _) = SceneDoc::load(src.as_bytes(), None, &pen_doc::NoLibraries).unwrap();
        assert_eq!(s.save(), src);
    }
}
