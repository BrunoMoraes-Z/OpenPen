//! DisplayList -> PDF.
//!
//! The page content is the SVG of [`crate::svg`] converted by `svg2pdf` (usvg tree -> PDF
//! operators): paths, gradients, clips, masks, blend modes and images stay vector/lossless;
//! text is real text with the fonts embedded as subsets (or outlines with
//! [`TextMode::Outlines`]); only groups with SVG filters (shadows, layer blur) are rasterized,
//! at [`ExportOptions::raster_scale`]. Pages are assembled with `pdf-writer` (one Form XObject
//! per page), so one document can hold several exports (e.g. one page per top-level frame).

use crate::{ExportError, ExportOptions, TextMode};
use pdf_writer::{Content, Finish, Name, Pdf, Rect as PdfRect, Ref, TextStr};
use pen_fonts::fontdb::Database;
use pen_raster::RasterContext;
use pen_render::{DisplayItem, DisplayList};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use svg2pdf::usvg;

/// Exports one page (see [`ExportOptions`]); 1 output unit = 1 pt.
pub fn export(list: &DisplayList, opts: &ExportOptions) -> Result<Vec<u8>, ExportError> {
    export_pages(list, std::slice::from_ref(opts))
}

/// Exports one page per entry of `pages` (each with its own selection, region and scale).
pub fn export_pages(list: &DisplayList, pages: &[ExportOptions]) -> Result<Vec<u8>, ExportError> {
    pen_raster::with_shared_context(|ctx| export_pages_with(ctx, list, pages))
}

/// Like [`export_pages`] with a caller-owned raster context.
pub fn export_pages_with(
    ctx: &mut RasterContext,
    list: &DisplayList,
    pages: &[ExportOptions],
) -> Result<Vec<u8>, ExportError> {
    if pages.is_empty() {
        return Err(ExportError::NoPages);
    }
    let fonts = FontSet::collect(list, pages);
    let mut usvg_opts = usvg::Options {
        fontdb: fonts.db.clone(),
        font_family: pen_fonts::DEFAULT_SANS.to_string(),
        ..usvg::Options::default()
    };
    usvg_opts.resources_dir = None;

    let mut alloc = Ref::new(1);
    let catalog_id = alloc.bump();
    let tree_id = alloc.bump();
    let mut pdf = Pdf::new();
    let mut page_ids = Vec::with_capacity(pages.len());
    for opts in pages {
        let svg = crate::svg::export_impl(ctx, list, opts, Some(&fonts.names));
        let tree =
            usvg::Tree::from_str(&svg, &usvg_opts).map_err(|e| ExportError::Svg(e.to_string()))?;
        let conv = svg2pdf::ConversionOptions {
            compress: true,
            raster_scale: if opts.raster_scale.is_finite() {
                opts.raster_scale.clamp(0.25, 8.0)
            } else {
                2.0
            },
            embed_text: opts.text == TextMode::Text,
            pdfa: false,
        };
        let (chunk, root) =
            svg2pdf::to_chunk(&tree, conv).map_err(|e| ExportError::Pdf(e.to_string()))?;
        let mut map = HashMap::new();
        let chunk = chunk.renumber(|old| *map.entry(old).or_insert_with(|| alloc.bump()));
        let root = *map
            .get(&root)
            .ok_or_else(|| ExportError::Pdf("missing page XObject".into()))?;

        let (w, h) = (tree.size().width(), tree.size().height());
        let page_id = alloc.bump();
        let content_id = alloc.bump();
        let mut content = Content::new();
        content.transform([w, 0.0, 0.0, h, 0.0, 0.0]);
        content.x_object(Name(b"S0"));
        pdf.stream(content_id, &content.finish());

        let mut page = pdf.page(page_id);
        page.media_box(PdfRect::new(0.0, 0.0, w, h));
        page.parent(tree_id);
        page.contents(content_id);
        page.resources().x_objects().pair(Name(b"S0"), root);
        page.group().transparency().isolated(true).knockout(false);
        page.finish();
        pdf.extend(&chunk);
        page_ids.push(page_id);
    }
    pdf.catalog(catalog_id).pages(tree_id);
    pdf.pages(tree_id)
        .kids(page_ids.iter().copied())
        .count(page_ids.len() as i32);
    pdf.document_info(alloc.bump()).producer(TextStr("OpenPen"));
    Ok(pdf.finish())
}

/// Fonts the exported text needs: the bundled fonts, the installed families the document
/// names (copied from pen-fonts' system catalog) and platform fallbacks for characters none
/// of them covers.
struct FontSet {
    db: Arc<Database>,
    /// Document family name -> canonical family name in `db`.
    names: HashMap<String, String>,
}

impl FontSet {
    fn collect(list: &DisplayList, pages: &[ExportOptions]) -> Self {
        use pen_fonts::system::{
            Generic, fallback_families, load_families, load_family, load_generic,
        };
        let mut db = pen_fonts::system::bundled_db();
        let mut names = HashMap::new();
        if pages.iter().all(|p| p.text == TextMode::Outlines) {
            return Self {
                db: Arc::new(db),
                names,
            };
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut chars: HashSet<char> = HashSet::new();
        let mut generics = [false; 2];
        for chunk in list
            .chunks
            .iter()
            .filter(|c| pages.iter().any(|p| p.selects(c.root)))
        {
            for item in &chunk.items {
                let DisplayItem::Text { block, .. } = item else {
                    continue;
                };
                for run in &block.runs {
                    let fam = run.style.family.trim();
                    if !seen.insert(fam) {
                        continue;
                    }
                    match fam.to_ascii_lowercase().as_str() {
                        "" | "sans-serif" | "system-ui" | "ui-sans-serif" => {}
                        "serif" | "ui-serif" => generics[0] = true,
                        "monospace" | "ui-monospace" => generics[1] = true,
                        _ => {
                            if let Some(canon) = load_family(&mut db, fam) {
                                names.insert(fam.to_string(), canon);
                            }
                        }
                    }
                }
                chars.extend(
                    block
                        .text
                        .chars()
                        .filter(|c| !c.is_whitespace() && !c.is_control()),
                );
            }
        }
        if generics[0] {
            load_generic(&mut db, Generic::Serif);
        }
        if generics[1] {
            load_generic(&mut db, Generic::Monospace);
        }
        if !chars.is_empty() {
            let ids: Vec<_> = db.faces().map(|f| f.id).collect();
            for id in ids {
                db.with_face_data(id, |data, index| {
                    if let Ok(face) = ttf_parser::Face::parse(data, index) {
                        chars.retain(|c| face.glyph_index(*c).is_none());
                    }
                });
                if chars.is_empty() {
                    break;
                }
            }
            if !chars.is_empty() {
                load_families(&mut db, fallback_families());
            }
        }
        Self {
            db: Arc::new(db),
            names,
        }
    }
}
