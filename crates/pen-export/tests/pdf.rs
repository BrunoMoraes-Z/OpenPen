//! PDF structure checks: parsed back with lopdf (page count and size, embedded fonts,
//! non-empty content).

use lopdf::{Document, Object};
use pen_export::{ExportOptions, TextMode};
use pen_render::*;
use smallvec::smallvec;
use std::sync::Arc;

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

fn rect_item(x: f32, y: f32, w: f32, h: f32, paint: Paint) -> DisplayItem {
    DisplayItem::Shape {
        node: NodeKey(0),
        transform: Affine2::from_translation(Vec2::new(x, y)),
        shape: Shape::Rect {
            size: Vec2::new(w, h),
            radii: CornerRadii::uniform(6.0),
        },
        fills: smallvec![paint],
        stroke: None,
        shadows: smallvec![Shadow::Drop {
            offset: Vec2::new(0.0, 2.0),
            blur: 6.0,
            spread: 0.0,
            color: Color::rgba(0.0, 0.0, 0.0, 0.3),
        }],
    }
}

fn text_item(x: f32, y: f32, s: &str) -> DisplayItem {
    DisplayItem::Text {
        node: NodeKey(1),
        transform: Affine2::from_translation(Vec2::new(x, y)),
        block: Arc::new(TextBlock {
            text: s.into(),
            runs: smallvec![TextRun {
                range: 0..s.len(),
                style: TextStyle {
                    family: "Inter".into(),
                    size: 16.0,
                    weight: 600,
                    italic: false,
                    letter_spacing: 0.0,
                    underline: false,
                    strikethrough: false,
                    paint: Paint::Solid(rgb(20, 20, 40)),
                },
            }],
            lines: vec![TextLine {
                range: 0..s.len(),
                x: 0.0,
                baseline: 16.0,
                width: 0.0,
            }],
            align: TextAlign::Left,
            size: Vec2::new(200.0, 20.0),
            clip: false,
        }),
    }
}

/// Two top-level frames: (0,0,300,200) and (400,0,120,80).
fn scene() -> DisplayList {
    DisplayList::from_chunks([
        Chunk {
            root: NodeKey(1),
            bounds: Rect::new(0.0, 0.0, 300.0, 200.0),
            version: 0,
            items: vec![
                rect_item(0.0, 0.0, 300.0, 200.0, Paint::Solid(rgb(245, 245, 250))),
                rect_item(
                    20.0,
                    60.0,
                    120.0,
                    80.0,
                    Paint::Linear {
                        start: Vec2::ZERO,
                        end: Vec2::new(120.0, 0.0),
                        stops: smallvec![
                            GradientStop {
                                offset: 0.0,
                                color: rgb(255, 90, 0)
                            },
                            GradientStop {
                                offset: 1.0,
                                color: rgb(60, 0, 200)
                            },
                        ],
                    },
                ),
                text_item(20.0, 20.0, "Hello PDF"),
            ],
        },
        Chunk {
            root: NodeKey(2),
            bounds: Rect::new(400.0, 0.0, 120.0, 80.0),
            version: 0,
            items: vec![rect_item(
                400.0,
                0.0,
                120.0,
                80.0,
                Paint::Solid(rgb(0, 160, 90)),
            )],
        },
    ])
}

fn pages(doc: &Document) -> Vec<lopdf::ObjectId> {
    doc.get_pages().into_values().collect()
}

fn media_box(doc: &Document, page: lopdf::ObjectId) -> Vec<f32> {
    let dict = doc.get_dictionary(page).unwrap();
    dict.get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| match o {
            Object::Integer(i) => *i as f32,
            Object::Real(r) => *r,
            other => panic!("bad MediaBox entry {other:?}"),
        })
        .collect()
}

/// Every font dictionary reachable in the file, with whether its descriptor embeds a font file.
fn fonts(doc: &Document) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for obj in doc.objects.values() {
        let Ok(dict) = obj.as_dict() else { continue };
        if dict.get(b"Type").and_then(|t| t.as_name()).ok() != Some(b"FontDescriptor".as_slice()) {
            continue;
        }
        let name = dict
            .get(b"FontName")
            .and_then(|n| n.as_name())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let embedded = [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"]
            .iter()
            .any(|k| dict.get(k).is_ok());
        out.push((name, embedded));
    }
    out
}

#[test]
fn single_page_structure_and_fonts() {
    let list = scene();
    let opts = ExportOptions {
        roots: Some(vec![NodeKey(1)]),
        ..ExportOptions::default()
    };
    let bytes = pen_export::pdf::export(&list, &opts).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    std::fs::create_dir_all(target_dir()).unwrap();
    std::fs::write(target_dir().join("single.pdf"), &bytes).unwrap();

    let doc = Document::load_mem(&bytes).expect("valid PDF");
    let pages = pages(&doc);
    assert_eq!(pages.len(), 1);
    assert_eq!(media_box(&doc, pages[0]), vec![0.0, 0.0, 300.0, 200.0]);
    let content = doc.get_page_content(pages[0]).unwrap();
    assert!(!content.is_empty(), "empty page content stream");

    let fonts = fonts(&doc);
    assert!(!fonts.is_empty(), "no font in the PDF");
    assert!(
        fonts.iter().all(|(_, embedded)| *embedded),
        "font not embedded: {fonts:?}"
    );
    assert!(
        fonts.iter().any(|(n, _)| n.contains("Inter")),
        "Inter not embedded: {fonts:?}"
    );
    // The text is real text: some content stream (the page's Form XObject) shows glyphs.
    let shows_text = doc.objects.values().any(|o| {
        let Ok(s) = o.as_stream() else { return false };
        let data = s
            .decompressed_content()
            .unwrap_or_else(|_| s.content.clone());
        data.windows(3).any(|w| w == b" Tj" || w == b" TJ")
    });
    assert!(shows_text, "no text-showing operator in any content stream");
}

#[test]
fn multi_page_and_scale() {
    let list = scene();
    let pages_opts = [
        ExportOptions {
            roots: Some(vec![NodeKey(1)]),
            ..ExportOptions::default()
        },
        ExportOptions {
            roots: Some(vec![NodeKey(2)]),
            scale: 2.0,
            background: Some(Color::WHITE),
            ..ExportOptions::default()
        },
    ];
    let bytes = pen_export::pdf::export_pages(&list, &pages_opts).unwrap();
    let doc = Document::load_mem(&bytes).expect("valid PDF");
    let pages = pages(&doc);
    assert_eq!(pages.len(), 2);
    assert_eq!(media_box(&doc, pages[0]), vec![0.0, 0.0, 300.0, 200.0]);
    assert_eq!(media_box(&doc, pages[1]), vec![0.0, 0.0, 240.0, 160.0]);
    for p in pages {
        assert!(!doc.get_page_content(p).unwrap().is_empty());
    }
}

#[test]
fn outlined_text_has_no_fonts() {
    let list = scene();
    let opts = ExportOptions {
        text: TextMode::Outlines,
        ..ExportOptions::default()
    };
    let bytes = pen_export::pdf::export(&list, &opts).unwrap();
    let doc = Document::load_mem(&bytes).expect("valid PDF");
    assert!(fonts(&doc).is_empty(), "outlined text still embeds fonts");
    assert_eq!(
        media_box(&doc, pages(&doc)[0]),
        vec![0.0, 0.0, 520.0, 200.0]
    );
}

#[test]
fn no_pages_is_an_error() {
    assert!(pen_export::pdf::export_pages(&scene(), &[]).is_err());
}

fn target_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pen-export")
}
