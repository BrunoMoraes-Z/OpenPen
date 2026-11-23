//! Bundled icon fonts: name resolution, placeholders, corpus coverage and a
//! golden render through pen-raster.

use pen_doc::{FsLibraries, KindData, SceneDoc};
use pen_layout::icons::{IconError, icon_glyph, is_known_family};
use pen_layout::{BundledIcons, Pipeline, TextEngine, bundled_icon, resolve_icon};
use pen_render::{Color, DisplayItem, PathEl, Rect, Shape};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bounds(els: &[PathEl]) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for e in els {
        let pts: &[_] = match e {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => &[*p],
            PathEl::QuadTo(a, p) => &[*a, *p],
            PathEl::CubicTo(a, c, p) => &[*a, *c, *p],
            PathEl::Close => &[],
        };
        for p in pts {
            b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
        }
    }
    b
}

#[test]
fn known_names_resolve_to_outlines_inside_the_view_box() {
    for (family, name) in [
        ("lucide", "search"),
        ("lucide", "chevron-right"),
        ("Lucide", "house"),
        ("feather", "x"),
        ("Material Symbols Sharp", "home"),
        ("Material Symbols Rounded", "settings"),
        ("Material Symbols Outlined", "search"),
        ("phosphor", "x"),
        ("phosphor", "house-fill"),
    ] {
        let g = bundled_icon(family, name, 200.0).unwrap_or_else(|| panic!("{family}/{name}"));
        assert!(!g.stroked);
        assert!(g.path.els.len() > 4, "{family}/{name}: empty outline");
        let [x0, y0, x1, y1] = bounds(&g.path.els);
        let v = g.view_box;
        let tol = v * 0.1;
        assert!(
            x0 >= -tol && y0 >= -tol && x1 <= v + tol && y1 <= v + tol,
            "{family}/{name}: bounds {:?} outside 0..{v}",
            [x0, y0, x1, y1]
        );
        // Icons cover a good part of their box.
        assert!(x1 - x0 > v * 0.3 && y1 - y0 > v * 0.3, "{family}/{name}");
    }
}

#[test]
fn unknown_names_and_families() {
    assert_eq!(
        resolve_icon("lucide", "definitely-not-an-icon").unwrap_err(),
        IconError::UnknownName
    );
    assert_eq!(
        resolve_icon("Font Awesome", "house").unwrap_err(),
        IconError::UnknownFamily
    );
    assert!(bundled_icon("lucide", "definitely-not-an-icon", 200.0).is_none());
    assert!(!is_known_family("Font Awesome"));
    assert!(is_known_family("MATERIAL SYMBOLS SHARP"));
}

#[test]
fn material_weight_axis_changes_the_outline() {
    let r = resolve_icon("Material Symbols Sharp", "add").unwrap();
    let thin = icon_glyph(r, 100.0).unwrap();
    let bold = icon_glyph(r, 700.0).unwrap();
    let w = |g: &pen_layout::IconGlyph| {
        let b = bounds(&g.path.els);
        b[2] - b[0]
    };
    assert_ne!(thin.path.els, bold.path.els);
    assert!(w(&bold) > w(&thin));
    // Lucide is static: weight is ignored.
    let r = resolve_icon("lucide", "plus").unwrap();
    assert_eq!(
        icon_glyph(r, 100.0).unwrap().path.els,
        icon_glyph(r, 700.0).unwrap().path.els
    );
}

fn icon_doc(icons: &[(&str, &str, &str)]) -> SceneDoc {
    let children: Vec<String> = icons
        .iter()
        .enumerate()
        .map(|(i, (id, fam, name))| {
            format!(
                r##"{{"type":"icon","id":"{id}","x":{},"y":8,"width":32,"height":32,
                "library":"{fam}","icon":"{name}","fill":"#1f2937"}}"##,
                8 + i * 40
            )
        })
        .collect();
    let json = format!(
        r##"{{"version":"2.22","children":[{{"type":"frame","id":"f","x":0,"y":0,"width":{},"height":48,"layout":"none","fill":"#ffffff","children":[{}]}}]}}"##,
        8 + icons.len() * 40,
        children.join(",")
    );
    SceneDoc::new(pen_schema::load_document(json.as_bytes()).unwrap().0)
}

fn icon_items(pipe: &Pipeline, scene: &SceneDoc, id: &str) -> Vec<DisplayItem> {
    let k = scene.find(id).unwrap();
    pipe.list
        .chunks
        .iter()
        .flat_map(|c| c.items.iter())
        .filter(|i| matches!(i, DisplayItem::Shape { node, .. } if *node == k))
        .cloned()
        .collect()
}

#[test]
fn icon_nodes_become_paths_and_unknown_ones_placeholders() {
    let mut scene = icon_doc(&[("a", "lucide", "search"), ("b", "lucide", "no-such-icon")]);
    let mut pipe = Pipeline::new(
        TextEngine::with_font_system(cosmic_text::FontSystem::new_with_locale_and_db(
            "en-US".into(),
            Default::default(),
        )),
        Box::new(BundledIcons),
    );
    pipe.rebuild(&mut scene);
    let a = icon_items(&pipe, &scene, "a");
    assert!(matches!(
        &a[..],
        [DisplayItem::Shape { shape: Shape::Path(_), fills, stroke: None, .. }] if fills.len() == 1
    ));
    let b = icon_items(&pipe, &scene, "b");
    assert!(matches!(
        &b[..],
        [DisplayItem::Shape {
            shape: Shape::Rect { .. },
            stroke: Some(_),
            ..
        }]
    ));
}

/// Renders a row of icons through pen-raster and compares with
/// `tests/golden/icons.png` (`PEN_RASTER_UPDATE_GOLDEN=1` rewrites it).
#[test]
fn golden_icon_row() {
    let icons = [
        ("i0", "lucide", "search"),
        ("i1", "lucide", "settings"),
        ("i2", "lucide", "circle-check"),
        ("i3", "Material Symbols Sharp", "home"),
        ("i4", "Material Symbols Rounded", "favorite"),
        ("i5", "Material Symbols Outlined", "delete"),
        ("i6", "phosphor", "bell"),
        ("i7", "lucide", "unknown-icon-name"),
    ];
    let mut scene = icon_doc(&icons);
    let mut pipe = Pipeline::new(
        TextEngine::with_font_system(cosmic_text::FontSystem::new_with_locale_and_db(
            "en-US".into(),
            Default::default(),
        )),
        Box::new(BundledIcons),
    );
    pipe.rebuild(&mut scene);
    let w = 8.0 + icons.len() as f32 * 40.0;
    let mut ctx = pen_raster::RasterContext::new();
    let pix = ctx.render(&pipe.list, Rect::new(0.0, 0.0, w, 48.0), 2.0, Color::WHITE);
    // Every icon draws ink inside its box.
    for (i, icon) in icons.iter().enumerate() {
        let (x0, x1) = ((8 + i * 40) * 2, (40 + i * 40) * 2);
        let ink = (16..80)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| pix.pixel(x as u32, y as u32).unwrap().red() < 128)
            .count();
        assert!(ink > 40, "icon {} drew {ink} dark pixels", icon.2);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let golden = dir.join("icons.png");
    let png = pix.encode_png().unwrap();
    let update = std::env::var("PEN_RASTER_UPDATE_GOLDEN").is_ok_and(|v| v == "1");
    if update || !golden.exists() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&golden, &png).unwrap();
        return;
    }
    let want = pen_raster::Pixmap::load_png(&golden).unwrap();
    assert_eq!((want.width(), want.height()), (pix.width(), pix.height()));
    let differ = want
        .data()
        .chunks(4)
        .zip(pix.data().chunks(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 24))
        .count();
    let frac = differ as f32 / (pix.width() * pix.height()) as f32;
    if frac > 0.002 {
        std::fs::write(dir.join("icons.actual.png"), &png).unwrap();
        panic!("icons golden differs in {:.2}% of pixels", frac * 100.0);
    }
}

/// How many icon nodes of the corpus resolve to a bundled glyph.
#[test]
fn corpus_icon_coverage() {
    let mut files = Vec::new();
    for dir in ["testdata/legacy", "testdata/user"] {
        let Ok(rd) = std::fs::read_dir(root().join(dir)) else {
            continue;
        };
        files.extend(
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "pen")),
        );
    }
    if files.is_empty() {
        eprintln!("corpus not found; skipping");
        return;
    }
    files.sort();
    let (mut total, mut ok) = (0usize, 0usize);
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let Ok((scene, _)) = SceneDoc::load(&bytes, path.parent(), &FsLibraries::default()) else {
            continue;
        };
        let (mut t, mut o) = (0, 0);
        for &r in scene.roots() {
            for k in scene.descendants(r) {
                let Some(KindData::Icon(spec)) = scene.get(k).map(|n| &n.data) else {
                    continue;
                };
                t += 1;
                match resolve_icon(&spec.library, &spec.name) {
                    Ok(_) => o += 1,
                    Err(e) => {
                        *missing
                            .entry(format!("{e:?} {}/{}", spec.library, spec.name))
                            .or_default() += 1
                    }
                }
            }
        }
        if t > 0 {
            eprintln!(
                "{}: {o}/{t} icon nodes resolve",
                path.file_name().unwrap().to_string_lossy()
            );
        }
        total += t;
        ok += o;
    }
    eprintln!("corpus icons: {ok}/{total} resolve");
    for (k, n) in &missing {
        eprintln!("  unresolved x{n}: {k}");
    }
    assert!(total == 0 || ok * 100 >= total * 95, "{ok}/{total}");
}
