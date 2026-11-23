//! Undo round-trips on the real corpus (skipped when testdata/ is absent):
//! open, edit through the same paths the UI uses, undo everything, and the
//! saved bytes must equal the original file.

use std::path::PathBuf;
use std::time::Instant;

use openpen::geom::{BoxGeom, Handle, ResizeMods, resize};
use openpen::inspector::model::{LayoutMode, PropEdit, PropertySource};
use openpen::inspector::value::SizeValue;
use openpen::scene_document::{NewKind, SceneDocument, id_of};
use pen_render::Rect;

fn corpus() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/user");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "pen"))
        .collect();
    v.sort();
    v
}

#[test]
fn edit_and_undo_all_restores_bytes() {
    let files = corpus();
    if files.is_empty() {
        eprintln!("testdata/user missing: skipped");
        return;
    }
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let t0 = Instant::now();
        let (mut d, st) = SceneDocument::load(&path, &[]).unwrap();
        let load = t0.elapsed();
        let first = d.save_string();
        let same_bytes = first.as_bytes() == bytes.as_slice();
        let roots = d.scene.roots().to_vec();
        if roots.is_empty() {
            continue;
        }
        // 1. Move gesture (many incremental updates, one block).
        let r0 = roots[0];
        let g0 = d.box_geom(r0).unwrap();
        let t1 = Instant::now();
        d.begin();
        for i in 1..=30 {
            let g = BoxGeom {
                x: g0.x + i as f32,
                y: g0.y + i as f32 * 0.5,
                ..g0
            };
            d.set_box(r0, &g0, &g);
            d.flush();
        }
        d.commit();
        let drag = t1.elapsed() / 30;
        // 2. Resize with a handle.
        let g1 = d.box_geom(r0).unwrap();
        let p = g1.to_parent(pen_render::Vec2::new(g1.w + 40.0, g1.h + 20.0));
        let g2 = resize(&g1, Handle::BottomRight, p, ResizeMods::default());
        d.begin();
        d.set_box(r0, &g1, &g2);
        d.commit();
        d.flush();
        // 3. Inspector edits on a nested node (or the root).
        let target = d
            .scene
            .descendants(r0)
            .find(|&k| {
                k != r0
                    && d.get(k)
                        .is_some_and(|n| matches!(n.origin, pen_doc::Origin::Doc))
            })
            .unwrap_or(r0);
        let tid = id_of(target);
        d.refresh_props(&[tid]);
        for e in [
            PropEdit::Opacity(0.5),
            PropEdit::SetFillColor(0, 0x336699ff),
            PropEdit::AddFill,
            PropEdit::Width(SizeValue::Fixed(77.0)),
            PropEdit::Rotation(15.0),
        ] {
            d.refresh_props(&[tid]);
            d.begin();
            d.apply(tid, &e);
            d.commit();
            d.flush();
        }
        d.begin();
        d.apply(id_of(r0), &PropEdit::LayoutMode(LayoutMode::Vertical));
        d.commit();
        d.flush();
        // 4. Structure: create, duplicate, rename, hide, delete.
        d.create(
            NewKind::Rectangle,
            Rect::new(-500.0, -500.0, 40.0, 40.0),
            None,
            "",
        );
        d.flush();
        let dup = d.duplicate(&[r0]);
        d.flush();
        d.begin();
        d.rename_key(r0, "Renamed by test");
        d.set_visible_key(target, false);
        d.commit();
        d.flush();
        d.delete(&dup);
        d.flush();
        let steps = d.undo_steps();
        let t2 = Instant::now();
        while d.undo() {}
        d.flush();
        let undo_t = t2.elapsed();
        let after = d.save_string();
        assert!(!d.is_dirty(), "{}", path.display());
        if same_bytes {
            assert!(
                after.as_bytes() == bytes.as_slice(),
                "{}: bytes differ after undo",
                path.display()
            );
        } else {
            assert_eq!(after, first, "{}", path.display());
        }
        eprintln!(
            "{:<28} {:>8} B nodes {:>5} load {:>7.1?} (parse {:>6.1?} layout {:>6.1?}) drag-step {:>7.1?} steps {steps} undo-all {:>6.1?} byte-identical-save={same_bytes}",
            path.file_name().unwrap().to_string_lossy(),
            bytes.len(),
            st.nodes,
            load,
            st.parse,
            st.layout,
            drag,
            undo_t,
        );
    }
}
