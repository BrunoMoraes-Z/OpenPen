use pen_doc::{DocOp, NodeRef, SceneDoc};
use pen_layout::{LayoutEngine, NoAssets, Pipeline, TextEngine};
use pen_render::DisplayItem;
use std::sync::{Mutex, OnceLock};

/// One shared text engine (system font loading is slow).
fn engine() -> &'static Mutex<TextEngine> {
    static E: OnceLock<Mutex<TextEngine>> = OnceLock::new();
    E.get_or_init(|| Mutex::new(TextEngine::new()))
}

fn lay(json: &str) -> (SceneDoc, LayoutEngine) {
    let doc = pen_schema::load_document(json.as_bytes()).unwrap().0;
    let scene = SceneDoc::new(doc);
    let mut l = LayoutEngine::new();
    l.layout_all(
        &scene,
        &mut engine().lock().unwrap_or_else(|e| e.into_inner()),
    );
    (scene, l)
}

fn rect(s: &SceneDoc, l: &LayoutEngine, id: &str) -> [f32; 4] {
    let n = l
        .get(s.find(id).unwrap())
        .unwrap_or_else(|| panic!("{id} has no layout"));
    [n.x, n.y, n.w, n.h]
}

fn doc(children: &str) -> String {
    format!(r#"{{"version":"2.22","children":[{children}]}}"#)
}

#[test]
fn hug_gap_padding() {
    let (s, l) = lay(&doc(
        r#"{"type":"frame","id":"f","x":5,"y":6,"padding":[4,8],"gap":10,"children":[
        {"type":"rectangle","id":"a","width":20,"height":30},
        {"type":"rectangle","id":"b","width":10,"height":10}]}"#,
    ));
    assert_eq!(
        rect(&s, &l, "f"),
        [5.0, 6.0, 8.0 + 20.0 + 10.0 + 10.0 + 8.0, 4.0 + 30.0 + 4.0]
    );
    assert_eq!(rect(&s, &l, "a"), [8.0, 4.0, 20.0, 30.0]);
    assert_eq!(rect(&s, &l, "b"), [38.0, 4.0, 10.0, 10.0]);
}

#[test]
fn fill_container_main_and_cross() {
    let (s, l) = lay(&doc(
        r#"{"type":"frame","id":"f","width":200,"height":100,"gap":10,"children":[
        {"type":"rectangle","id":"fixed","width":50,"height":20},
        {"type":"rectangle","id":"f1","width":"fill_container","height":"fill_container"},
        {"type":"rectangle","id":"f2","width":"fill_container","height":20}]}"#,
    ));
    assert_eq!(rect(&s, &l, "fixed"), [0.0, 0.0, 50.0, 20.0]);
    // (200 - 50 - 2*10) / 2 = 65
    assert_eq!(rect(&s, &l, "f1"), [60.0, 0.0, 65.0, 100.0]);
    assert_eq!(rect(&s, &l, "f2"), [135.0, 0.0, 65.0, 20.0]);
}

#[test]
fn fill_outside_flex_uses_fallback() {
    let (s, l) = lay(&doc(
        r#"{"type":"frame","id":"f","layout":"none","width":300,"height":300,"children":[
        {"type":"rectangle","id":"r","x":7,"y":9,"width":"fill_container(42)","height":"fit_content(13)"}]}"#,
    ));
    assert_eq!(rect(&s, &l, "r"), [7.0, 9.0, 42.0, 13.0]);
}

#[test]
fn justify_and_align() {
    let (s, l) = lay(&doc(r#"
        {"type":"frame","id":"c","width":100,"height":50,"justifyContent":"center","alignItems":"center","children":[
            {"type":"rectangle","id":"a","width":20,"height":10}]},
        {"type":"frame","id":"e","layout":"vertical","width":100,"height":50,"justifyContent":"end","alignItems":"end","children":[
            {"type":"rectangle","id":"b","width":20,"height":10}]},
        {"type":"frame","id":"sb","width":100,"height":10,"justifyContent":"space_between","children":[
            {"type":"rectangle","id":"x","width":10,"height":10},
            {"type":"rectangle","id":"y","width":10,"height":10}]}"#));
    assert_eq!(rect(&s, &l, "a"), [40.0, 20.0, 20.0, 10.0]);
    assert_eq!(rect(&s, &l, "b"), [80.0, 40.0, 20.0, 10.0]);
    assert_eq!(rect(&s, &l, "y")[0], 90.0);
}

#[test]
fn absolute_children_and_disabled() {
    let (s, l) = lay(&doc(
        r#"{"type":"frame","id":"f","padding":10,"gap":5,"children":[
        {"type":"rectangle","id":"a","width":20,"height":20},
        {"type":"rectangle","id":"abs","layoutPosition":"absolute","x":100,"y":3,"width":5,"height":5},
        {"type":"rectangle","id":"off","enabled":false,"width":500,"height":500},
        {"type":"rectangle","id":"b","width":20,"height":20}]}"#,
    ));
    assert_eq!(rect(&s, &l, "abs"), [100.0, 3.0, 5.0, 5.0]);
    assert_eq!(rect(&s, &l, "b"), [10.0 + 20.0 + 5.0, 10.0, 20.0, 20.0]);
    assert_eq!(rect(&s, &l, "f"), [0.0, 0.0, 10.0 + 45.0 + 10.0, 40.0]);
    assert!(l.get(s.find("off").unwrap()).is_none());
}

#[test]
fn nested_hug_with_fill_children() {
    let (s, l) = lay(&doc(
        r#"{"type":"frame","id":"outer","layout":"vertical","gap":4,"children":[
        {"type":"rectangle","id":"wide","width":120,"height":10},
        {"type":"frame","id":"row","width":"fill_container","padding":2,"children":[
            {"type":"rectangle","id":"cell","width":"fill_container","height":6}]}]}"#,
    ));
    assert_eq!(rect(&s, &l, "outer"), [0.0, 0.0, 120.0, 10.0 + 4.0 + 10.0]);
    assert_eq!(rect(&s, &l, "row"), [0.0, 14.0, 120.0, 10.0]);
    assert_eq!(rect(&s, &l, "cell"), [2.0, 2.0, 116.0, 6.0]);
}

#[test]
fn text_growth_modes() {
    let long = "the quick brown fox jumps over the lazy dog again and again";
    let (s, l) = lay(&doc(&format!(
        r#"{{"type":"frame","id":"f","layout":"vertical","width":120,"children":[
        {{"type":"text","id":"auto","content":"{long}","fontSize":14}},
        {{"type":"text","id":"wrap","textGrowth":"fixed-width","width":"fill_container","content":"{long}","fontSize":14,"lineHeight":1.5}},
        {{"type":"text","id":"fixed","textGrowth":"fixed-width-height","width":50,"height":17,"content":"{long}","fontSize":14}}]}}"#
    )));
    let a = rect(&s, &l, "auto");
    assert!(a[2] > 200.0, "auto width grows: {a:?}");
    assert!((a[3] - 14.0 * 1.2).abs() < 0.01);
    let w = rect(&s, &l, "wrap");
    assert_eq!(w[2], 120.0);
    let tl = l
        .get(s.find("wrap").unwrap())
        .unwrap()
        .text
        .clone()
        .unwrap();
    assert!(tl.lines.len() >= 3, "{} lines", tl.lines.len());
    assert!((w[3] - tl.lines.len() as f32 * 21.0).abs() < 0.01, "{w:?}");
    assert!(tl.lines.iter().all(|line| line.width <= 120.0 + 0.5));
    // Lines cover the text without overlap, trailing spaces excluded.
    for pair in tl.lines.windows(2) {
        assert!(pair[0].range.end <= pair[1].range.start);
        assert!(pair[0].baseline < pair[1].baseline);
    }
    assert_eq!(rect(&s, &l, "fixed")[2..], [50.0, 17.0]);
}

#[test]
fn rotation_flip_and_display_list() {
    let json = doc(
        r##"{"type":"frame","id":"f","x":100,"y":0,"width":100,"height":100,"clip":true,"fill":"#ffffff",
        "effect":{"type":"shadow","offset":{"x":0,"y":4},"blur":8,"color":"#00000040"},"children":[
        {"type":"rectangle","id":"r","x":0,"y":0,"width":10,"height":20,"rotation":90,"opacity":0.5,"fill":"#ff0000"},
        {"type":"frame","id":"g","layout":"none","x":50,"y":50,"width":20,"height":20,"opacity":0.5,"children":[
            {"type":"ellipse","id":"e","width":10,"height":10,"fill":"#00ff00","innerRadius":0.5}]}]}"##,
    );
    let doc = pen_schema::load_document(json.as_bytes()).unwrap().0;
    let mut scene = SceneDoc::new(doc);
    let text = std::mem::take(&mut *engine().lock().unwrap_or_else(|e| e.into_inner()));
    let mut pipe = Pipeline::new(text, Box::new(NoAssets));
    pipe.rebuild(&mut scene);
    let r = pipe.node_rect(&scene, scene.find("r").unwrap()).unwrap();
    // Rotated 90deg counter-clockwise around its top-left corner at (100, 0).
    assert!(
        (r.x - 100.0).abs() < 1e-3 && (r.y + 10.0).abs() < 1e-3 && (r.w - 20.0).abs() < 1e-3,
        "{r:?}"
    );
    let chunk = &pipe.list.chunks[0];
    // Shadow is part of the bounds.
    assert!(chunk.bounds.bottom() > 100.0 + 4.0);
    let items = &chunk.items;
    // Leaf opacity folded into the paint; group opacity -> one layer.
    let layers = items
        .iter()
        .filter(|i| matches!(i, DisplayItem::PushLayer { .. }))
        .count();
    assert_eq!(layers, 1);
    assert!(
        items
            .iter()
            .any(|i| matches!(i, DisplayItem::PushClip { .. }))
    );
    let folded = items.iter().any(|i| match i {
        DisplayItem::Shape { fills, .. } => fills.iter().any(
            |p| matches!(p, pen_render::Paint::Solid(c) if (c.a - 0.5).abs() < 1e-3 && c.r == 1.0),
        ),
        _ => false,
    });
    assert!(folded);
    // Incremental: a fill edit rebuilds one chunk without relayout.
    let v1 = chunk.version;
    let n = pen_schema::Node {
        fill: Some(pen_schema::OneOrMany::One(pen_schema::Fill::Solid(
            pen_schema::ColorOrVar("#0000ff".into()),
        ))),
        ..Default::default()
    };
    scene.apply(DocOp::set(NodeRef::id("r"), n)).unwrap();
    let stats = pipe.update(&mut scene);
    assert!(stats.relaid.is_empty());
    assert_eq!(stats.rebuilt_chunks.len(), 1);
    assert!(pipe.list.chunks[0].version > v1);
    // A size edit relays out.
    let n = pen_schema::Node {
        width: Some(pen_schema::Sizing::Fixed(300.0)),
        ..Default::default()
    };
    scene.apply(DocOp::set(NodeRef::id("f"), n)).unwrap();
    let stats = pipe.update(&mut scene);
    assert_eq!(stats.relaid.len(), 1);
    assert_eq!(pipe.layout.get(scene.find("f").unwrap()).unwrap().w, 300.0);
    *engine().lock().unwrap_or_else(|e| e.into_inner()) = pipe.text;
}

/// Moving a top-level root translates its chunk; the result matches a full rebuild.
#[test]
fn moving_a_root_translates_its_chunk() {
    let json = doc(
        r##"{"type":"frame","id":"f","x":10,"y":20,"width":100,"height":80,"fill":"#ff0000","layout":"vertical","padding":8,"gap":4,"clip":true,"children":[
        {"type":"rectangle","id":"a","width":"fill_container","height":20,"fill":"#00ff00","stroke":{"thickness":2,"fill":"#000000"}},
        {"type":"text","id":"t","content":"Hello","fontSize":12,"fill":"#000000"},
        {"type":"frame","id":"g","width":30,"height":30,"opacity":0.5,"children":[
          {"type":"ellipse","id":"e","width":10,"height":10,"fill":"#0000ff"},
          {"type":"rectangle","id":"r","width":10,"height":10,"fill":"#0000ff"}]}]},
        {"type":"rectangle","id":"other","x":300,"y":0,"width":10,"height":10,"fill":"#000000"}"##,
    );
    let doc = pen_schema::load_document(json.as_bytes()).unwrap().0;
    let mut scene = SceneDoc::new(doc);
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.rebuild(&mut scene);
    let other_before = pipe.list.chunks[1].clone();
    let patch = pen_schema::Node {
        x: Some(47.5),
        y: Some(-3.0),
        ..pen_schema::Node::default()
    };
    scene.apply(DocOp::set(NodeRef::id("f"), patch)).unwrap();
    let st = pipe.update(&mut scene);
    assert!(st.relaid.is_empty(), "a pure move must not relayout");
    assert_eq!(st.rebuilt_chunks.len(), 1);
    // The untouched chunk is shared, not copied.
    assert!(std::sync::Arc::ptr_eq(&other_before, &pipe.list.chunks[1]));
    let f = scene.find("f").unwrap();
    let l = pipe.layout.get(f).unwrap();
    assert_eq!((l.x, l.y), (47.5, -3.0));

    let moved = pipe.list.clone();
    let mut fresh = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    fresh.rebuild(&mut scene);
    assert_eq!(moved.chunks.len(), fresh.list.chunks.len());
    let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
    for (a, b) in moved.chunks.iter().zip(&fresh.list.chunks) {
        let (ab, bb) = (a.bounds, b.bounds);
        assert!(close(ab.x, bb.x) && close(ab.y, bb.y) && close(ab.w, bb.w) && close(ab.h, bb.h));
        assert_eq!(a.items.len(), b.items.len());
        for (x, y) in a.items.iter().zip(&b.items) {
            let tr = |i: &DisplayItem| match i {
                DisplayItem::Shape { transform, .. }
                | DisplayItem::Text { transform, .. }
                | DisplayItem::PushClip { transform, .. } => Some(*transform),
                _ => None,
            };
            match (tr(x), tr(y)) {
                (Some(p), Some(q)) => assert!(p.abs_diff_eq(q, 1e-3), "{p:?} vs {q:?}"),
                (None, None) => {}
                _ => panic!("item kinds differ"),
            }
            if let (
                DisplayItem::PushLayer { bounds: p, .. },
                DisplayItem::PushLayer { bounds: q, .. },
            ) = (x, y)
            {
                assert!(close(p.x, q.x) && close(p.y, q.y));
            }
        }
    }

    // Moving a nested node still relays out its root.
    let patch = pen_schema::Node {
        x: Some(3.0),
        ..pen_schema::Node::default()
    };
    scene.apply(DocOp::set(NodeRef::id("e"), patch)).unwrap();
    let st = pipe.update(&mut scene);
    assert_eq!(st.relaid, vec![f]);
}

/// Characters the bundled fonts lack load system fallbacks on demand.
#[test]
fn missing_glyphs_load_fallback_fonts() {
    let mut te = TextEngine::new();
    let spec = |s: &str| pen_doc::TextSpec {
        content: s.into(),
        family: "Inter".into(),
        size: 16.0,
        weight: 400,
        italic: false,
        letter_spacing: 0.0,
        line_height: 0.0,
        align: pen_render::TextAlign::Left,
        valign: pen_doc::VAlign::Top,
        underline: false,
        strikethrough: false,
        growth: pen_doc::TextGrowth::Auto,
    };
    let before = te.font_system().db().len();
    let latin = te.layout(&spec("Olá, mundo"), None);
    assert!(latin.width > 0.0);
    // Latin text never loads system fonts.
    assert_eq!(te.font_system().db().len(), before);
    if !pen_fonts::system::catalog().is_empty() {
        te.layout(&spec("漢字 ✓"), None);
        assert!(te.font_system().db().len() > before);
    }
}
