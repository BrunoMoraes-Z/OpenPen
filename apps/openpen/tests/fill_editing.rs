//! Fill editing through the same path the inspector uses (`PropEdit::Paint`
//! on a `SceneDocument`): type conversions, gradient stops and geometry,
//! image fills, variable bindings, one undo step per edit, and a lossless
//! save / load round trip (unknown fields kept).

use openpen::inspector::fill::{FillType, GradKind, ImageFit, PaintOp};
use openpen::inspector::model::{PaintKind, PropEdit, PropertySource};
use openpen::scene_document::{SceneDocument, id_of};
use pen_doc::NodeKey;

const DOC: &str = r##"{
  "version": "2.22",
  "variables": { "brand": { "type": "color", "value": "#3366ff" } },
  "children": [
    { "type": "rectangle", "id": "a", "x": 0, "y": 0, "width": 100, "height": 50, "fill": "$brand", "futureNode": 1 },
    { "type": "rectangle", "id": "g", "x": 200, "y": 0, "width": 100, "height": 100,
      "fill": [
        "#000000",
        { "type": "gradient", "gradientType": "linear", "enabled": true, "rotation": 90, "future": "x",
          "size": { "height": 1 },
          "colors": [ { "color": "#ff0000", "position": 0, "note": 1 }, { "color": "$brand", "position": 1 } ] }
      ] }
  ]
}"##;

fn load(src: &str) -> SceneDocument {
    SceneDocument::from_bytes(src.as_bytes(), None, &[])
        .unwrap()
        .0
}

fn key(d: &SceneDocument, id: &str) -> NodeKey {
    d.scene.find(id).unwrap()
}

/// One inspector edit = one undo step (like `EditorState::apply`).
fn edit(d: &mut SceneDocument, k: NodeKey, e: PropEdit) {
    d.begin();
    d.apply_edit(k, &e);
    d.commit();
    d.flush();
    d.refresh_props(&[id_of(k)]);
}

fn paint(index: usize, op: PaintOp) -> PropEdit {
    PropEdit::Paint {
        stroke: false,
        index,
        op,
    }
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

#[test]
fn variable_solid_becomes_gradient_and_undoes_in_one_step() {
    let mut d = load(DOC);
    let a = key(&d, "a");
    d.refresh_props(&[id_of(a)]);
    let p = d.props(id_of(a)).unwrap();
    assert_eq!(p.fills[0].var.as_deref(), Some("brand"));
    assert_eq!(p.fills[0].kind, PaintKind::Solid(0x3366ffff));

    edit(&mut d, a, paint(0, PaintOp::SetType(FillType::Linear)));
    let p = d.props(id_of(a)).unwrap();
    let g = p.fills[0].gradient.as_ref().expect("a gradient now");
    assert_eq!(g.kind, GradKind::Linear);
    // The bound color is the first stop, still bound and resolved.
    assert_eq!(g.stops[0].var.as_deref(), Some("brand"));
    assert_eq!(g.stops[0].color, 0x3366ffff);
    assert_eq!(g.stops[1].color, 0x3366ff00);
    let j = node_json(&d, "a");
    assert_eq!(j["fill"]["colors"][0]["color"], "$brand");
    assert_eq!(j["futureNode"], 1);

    assert!(d.undo());
    d.flush();
    assert_eq!(node_json(&d, "a")["fill"], "$brand");
    assert!(d.redo());
    d.flush();
    assert_eq!(node_json(&d, "a")["fill"]["type"], "gradient");
}

#[test]
fn gradient_edits_round_trip_losslessly() {
    let mut d = load(DOC);
    let g = key(&d, "g");
    d.refresh_props(&[id_of(g)]);
    edit(&mut d, g, paint(1, PaintOp::AddStop(0.5)));
    edit(&mut d, g, paint(1, PaintOp::StopColor(2, 0x00ff0080)));
    edit(&mut d, g, paint(1, PaintOp::StopPosition(0, 0.1)));
    edit(
        &mut d,
        g,
        paint(
            1,
            PaintOp::Geometry {
                center: [0.25, 0.5],
                size: [1.0, 0.5],
                rotation: 45.0,
            },
        ),
    );
    edit(&mut d, g, PropEdit::SetFillOpacity(1, 0.5));
    let p = d.props(id_of(g)).unwrap().clone();
    let info = p.fills[1].gradient.clone().unwrap();
    assert_eq!(info.stops.len(), 3);
    assert_eq!(info.stops[2].color, 0x00ff0080);
    assert_eq!(info.stops[0].position, 0.1);
    assert_eq!(info.center, [0.25, 0.5]);
    assert_eq!(info.rotation, 45.0);
    assert_eq!(p.fills[1].opacity, 0.5);
    // The solid below is untouched.
    assert_eq!(p.fills[0].kind, PaintKind::Solid(0x000000ff));

    let j = node_json(&d, "g");
    let f = &j["fill"][1];
    assert_eq!(f["future"], "x", "unknown fill field kept");
    assert_eq!(f["colors"][0]["note"], 1, "unknown stop field kept");
    assert_eq!(
        f["colors"][1]["color"], "$brand",
        "untouched stop keeps its variable"
    );
    assert_eq!(f["colors"][2]["color"], "#00FF0080");
    assert_eq!(f["opacity"], 0.5);
    // Linear gradients only store the length (like Pen).
    assert!(f["size"].get("width").is_none());

    // Save, reload: same document, same inspector snapshot.
    let saved = d.save_string();
    let mut d2 = load(&saved);
    assert_eq!(d2.save_string(), saved);
    let g2 = key(&d2, "g");
    d2.refresh_props(&[id_of(g2)]);
    assert_eq!(d2.props(id_of(g2)).unwrap().fills, p.fills);

    // Kind switches keep the stops and the geometry.
    edit(&mut d, g, paint(1, PaintOp::SetType(FillType::Radial)));
    let info2 = d.props(id_of(g)).unwrap().fills[1]
        .gradient
        .clone()
        .unwrap();
    assert_eq!(info2.kind, GradKind::Radial);
    assert_eq!(info2.stops, info.stops);
    assert_eq!(info2.center, info.center);
}

#[test]
fn a_drag_is_one_undo_step() {
    let mut d = load(DOC);
    let g = key(&d, "g");
    d.refresh_props(&[id_of(g)]);
    let before = d.save_string();
    d.begin();
    for t in [0.2, 0.3, 0.4, 0.55] {
        d.begin();
        d.apply_edit(g, &paint(1, PaintOp::StopPosition(0, t)));
        d.commit();
        d.flush();
    }
    d.commit();
    assert_eq!(node_json(&d, "g")["fill"][1]["colors"][0]["position"], 0.55);
    assert!(d.undo());
    d.flush();
    assert_eq!(d.save_string(), before);
}

#[test]
fn image_fill_edits() {
    let mut d = load(DOC);
    let a = key(&d, "a");
    d.refresh_props(&[id_of(a)]);
    edit(&mut d, a, paint(0, PaintOp::SetType(FillType::Image)));
    edit(
        &mut d,
        a,
        paint(0, PaintOp::ImageUrl("assets/logo.png".into())),
    );
    edit(&mut d, a, paint(0, PaintOp::ImageFit(ImageFit::Fit)));
    edit(&mut d, a, PropEdit::SetFillOpacity(0, 0.25));
    let j = node_json(&d, "a");
    assert_eq!(j["fill"]["type"], "image");
    assert_eq!(j["fill"]["url"], "assets/logo.png");
    assert_eq!(j["fill"]["mode"], "contain");
    assert_eq!(j["fill"]["opacity"], 0.25);
    let p = d.props(id_of(a)).unwrap();
    let img = p.fills[0].image.as_ref().unwrap();
    assert_eq!((img.fit, img.opacity), (ImageFit::Fit, 0.25));
    // Back to a solid: Pen's default color.
    edit(&mut d, a, paint(0, PaintOp::SetType(FillType::Solid)));
    assert_eq!(node_json(&d, "a")["fill"], "#D9D9D9");
}

#[test]
fn stop_removal_keeps_one_stop() {
    let mut d = load(DOC);
    let g = key(&d, "g");
    d.refresh_props(&[id_of(g)]);
    edit(&mut d, g, paint(1, PaintOp::RemoveStop(0)));
    let before = d.save_string();
    // The last stop stays: nothing changes, no undo step.
    d.begin();
    assert!(!d.apply_edit(g, &paint(1, PaintOp::RemoveStop(0))));
    d.commit();
    assert_eq!(d.save_string(), before);
    let j = node_json(&d, "g");
    assert_eq!(j["fill"][1]["colors"].as_array().unwrap().len(), 1);
}
