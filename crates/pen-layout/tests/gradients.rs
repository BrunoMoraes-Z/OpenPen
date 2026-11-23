//! Gradient geometry as handed to the backends (Pen's semantics: the ellipse is built in
//! the normalized box, rotated about the center, then scaled by the node size).

use pen_doc::SceneDoc;
use pen_layout::{FsAssets, Pipeline, TextEngine};
use pen_render::{DisplayItem, Paint, Vec2};

fn first_fill(fill: &str, w: f32, h: f32) -> Paint {
    let json = format!(
        r#"{{"version":"2.22","children":[{{"type":"rectangle","id":"r","width":{w},"height":{h},"fill":{fill}}}]}}"#
    );
    let mut scene = SceneDoc::new(pen_schema::load_document(json.as_bytes()).unwrap().0);
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(FsAssets::new(None)));
    pipe.rebuild(&mut scene);
    pipe.list
        .chunks
        .iter()
        .flat_map(|c| c.items.iter())
        .find_map(|i| match i {
            DisplayItem::Shape { fills, .. } => fills.first().cloned(),
            _ => None,
        })
        .expect("a filled shape")
}

fn close(a: Vec2, b: Vec2) -> bool {
    (a - b).length() < 1e-3
}

#[test]
fn radial_without_rotation_is_axis_aligned() {
    let p = first_fill(
        r##"{"type":"gradient","gradientType":"radial","center":{"x":0.5,"y":0.5},"size":{"width":1,"height":0.5},"colors":[{"color":"#000000","position":0},{"color":"#ffffff","position":1}]}"##,
        200.0,
        100.0,
    );
    let Paint::Radial {
        center,
        axis_x,
        axis_y,
        ..
    } = p
    else {
        panic!("radial expected, got {p:?}")
    };
    assert!(close(center, Vec2::new(100.0, 50.0)));
    assert!(close(axis_x, Vec2::new(100.0, 0.0)));
    // `size.height` lies along the rotation, which points up at 0 deg.
    assert!(close(axis_y, Vec2::new(0.0, -25.0)));
}

#[test]
fn rotated_radial_keeps_its_rotation() {
    // 45 deg counter-clockwise in a non-square box: the axes are rotated, then scaled.
    let p = first_fill(
        r##"{"type":"gradient","gradientType":"radial","rotation":45,"center":{"x":0.5,"y":0.5},"size":{"width":1,"height":0.5},"colors":[{"color":"#000000","position":0},{"color":"#ffffff","position":1}]}"##,
        200.0,
        100.0,
    );
    let Paint::Radial { axis_x, axis_y, .. } = p else {
        panic!("radial expected, got {p:?}")
    };
    let k = std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(axis_x, Vec2::new(k * 0.5 * 200.0, -k * 0.5 * 100.0)));
    assert!(close(
        axis_y,
        Vec2::new(-k * 0.25 * 200.0, -k * 0.25 * 100.0)
    ));
    // Pen maps the unit circle through these vectors: a point on the long axis is t = 1.
    let m = pen_render::radial_to_unit(Vec2::new(100.0, 50.0), axis_x, axis_y).unwrap();
    let t = m.transform_point2(Vec2::new(100.0, 50.0) + axis_x).length();
    assert!((t - 1.0).abs() < 1e-4);
}
