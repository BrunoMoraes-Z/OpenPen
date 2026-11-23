//! Image fills follow their file on disk: a rewritten image gets a new id
//! (so GPU / raster textures keyed by it are not reused) once
//! `Pipeline::reload_changed_assets` runs.

use pen_doc::SceneDoc;
use pen_layout::{FsAssets, Pipeline, TextEngine};
use pen_render::{DisplayItem, Paint};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(path: &Path, w: u32, h: u32, rgba: [u8; 4]) {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
    img.save(path).unwrap();
}

/// (id, width) of the first image paint in the display list.
fn image_paint(pipe: &Pipeline) -> Option<(u64, u32)> {
    pipe.list
        .chunks
        .iter()
        .flat_map(|c| c.items.iter())
        .find_map(|i| match i {
            DisplayItem::Shape { fills, .. } => fills.iter().find_map(|p| match p {
                Paint::Image { image, .. } => Some((image.id, image.width)),
                _ => None,
            }),
            _ => None,
        })
}

fn doc() -> SceneDoc {
    let json = r#"{"version":"2.22","children":[{"type":"rectangle","id":"r","width":40,"height":40,
        "fill":{"type":"image","enabled":true,"url":"pic.png","mode":"cover"}}]}"#;
    SceneDoc::new(pen_schema::load_document(json.as_bytes()).unwrap().0)
}

#[test]
fn rewritten_image_file_gets_a_new_id() {
    let dir = scratch("image-reload-sync");
    write_png(&dir.join("pic.png"), 4, 4, [255, 0, 0, 255]);
    let mut scene = doc();
    let mut pipe = Pipeline::new(
        TextEngine::new(),
        Box::new(FsAssets::new(Some(dir.clone()))),
    );
    pipe.rebuild(&mut scene);
    let (id1, w1) = image_paint(&pipe).expect("image painted");
    assert_eq!(w1, 4);
    // Nothing changed: nothing to reload.
    assert!(!pipe.reload_changed_assets(&scene));
    write_png(&dir.join("pic.png"), 8, 8, [0, 0, 255, 255]);
    assert!(pipe.reload_changed_assets(&scene));
    let (id2, w2) = image_paint(&pipe).expect("image painted");
    assert_eq!(w2, 8);
    assert_ne!(id1, id2, "new content, new texture key");
    assert!(!pipe.reload_changed_assets(&scene));
}

#[test]
fn deferred_resolver_reloads_in_the_background() {
    let dir = scratch("image-reload-deferred");
    write_png(&dir.join("pic.png"), 4, 4, [255, 0, 0, 255]);
    let mut scene = doc();
    let mut pipe = Pipeline::new(
        TextEngine::new(),
        Box::new(FsAssets::deferred(Some(dir.clone()))),
    );
    pipe.rebuild(&mut scene);
    let wait = |pipe: &mut Pipeline, scene: &SceneDoc| {
        for _ in 0..400 {
            pipe.refresh_assets(scene);
            if !pipe.has_pending_assets() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("image never loaded");
    };
    wait(&mut pipe, &scene);
    let (id1, _) = image_paint(&pipe).expect("image painted");
    write_png(&dir.join("pic.png"), 6, 6, [0, 255, 0, 255]);
    assert!(pipe.reload_changed_assets(&scene));
    wait(&mut pipe, &scene);
    let (id2, w2) = image_paint(&pipe).expect("image painted");
    assert_eq!(w2, 6);
    assert_ne!(id1, id2);
}
