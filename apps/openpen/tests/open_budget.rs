//! Open / edit budgets of the real document path (`SceneDocument`) on the
//! largest corpus file. Skips when `testdata/user/t-process-recorder.pen` is
//! absent. Budgets are for release builds (`cargo test --release`); debug
//! builds only check the structure (loose limits).

use std::path::Path;
use std::time::{Duration, Instant};

use openpen::scene_document::SceneDocument;

/// `release_us` in release builds, 20x that in debug builds.
fn budget(release_us: u64) -> Duration {
    Duration::from_micros(if cfg!(debug_assertions) {
        release_us * 20
    } else {
        release_us
    })
}

#[test]
fn open_and_drag_budget() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/user/t-process-recorder.pen");
    if !path.exists() {
        eprintln!("corpus not found; skipping");
        return;
    }
    // Fresh process: includes the one-time system font scan.
    let t = Instant::now();
    let (mut doc, st) = SceneDocument::load(&path, &[]).unwrap();
    let open = t.elapsed();
    eprintln!("open {open:?} ({st:?})");
    assert!(open < budget(150_000), "open took {open:?}");

    // Drag the largest top-level frame by 1 px steps (op + update + list).
    let root = *doc
        .scene
        .roots()
        .iter()
        .max_by_key(|&&r| doc.scene.descendants(r).count())
        .unwrap();
    let mut worst = Duration::ZERO;
    let mut total = Duration::ZERO;
    let steps = 50;
    doc.begin();
    for i in 0..steps {
        let old = doc.box_geom(root).unwrap();
        let mut new = old;
        new.x += 1.0 + (i % 2) as f32;
        let t = Instant::now();
        doc.set_box(root, &old, &new);
        doc.flush();
        let list = doc.display_list().clone();
        let d = t.elapsed();
        std::hint::black_box(list);
        worst = worst.max(d);
        total += d;
    }
    doc.commit();
    let avg = total / steps;
    eprintln!("drag step avg {avg:?}, worst {worst:?}");
    assert!(avg < budget(300), "drag step {avg:?}");
}
