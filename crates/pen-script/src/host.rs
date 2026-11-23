//! Rendering services used by `TakeScreenshot` and `Export`.
//!
//! The engine records screenshot/export requests while the snippet runs and
//! hands them to a [`ScriptHost`] once the snippet finished (before the
//! transaction is committed), with the scene and pipeline up to date. The
//! default methods render on the CPU with pen-raster, so a GUI host only needs
//! to override what it renders differently.

use std::path::{Path, PathBuf};

use pen_doc::{NodeKey, SceneDoc};
use pen_layout::Pipeline;
use pen_render::{Color, DisplayList, Rect};

/// What to render.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderTarget {
    /// Every top-level node.
    Document,
    /// One node, by id path and current scene key.
    Node { id: String, key: NodeKey },
}

impl RenderTarget {
    /// File stem for exports (`document`, or the id path with `/` replaced).
    pub fn file_stem(&self) -> String {
        match self {
            RenderTarget::Document => "document".into(),
            RenderTarget::Node { id, .. } => id.replace(['/', '\\', ':'], "-"),
        }
    }
}

/// Image formats of `Export`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Jpeg,
    Webp,
    Pdf,
    Svg,
    HtmlTailwind,
    HtmlCss,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<ExportFormat> {
        Some(match s {
            "png" => ExportFormat::Png,
            "jpeg" | "jpg" => ExportFormat::Jpeg,
            "webp" => ExportFormat::Webp,
            "pdf" => ExportFormat::Pdf,
            "svg" => ExportFormat::Svg,
            "html-tailwind" => ExportFormat::HtmlTailwind,
            "html-css" => ExportFormat::HtmlCss,
            _ => return None,
        })
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Png => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Webp => "webp",
            ExportFormat::Pdf => "pdf",
            ExportFormat::Svg => "svg",
            ExportFormat::HtmlTailwind | ExportFormat::HtmlCss => "html",
        }
    }
}

/// One `Export(...)` call.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportRequest {
    pub targets: Vec<RenderTarget>,
    pub format: ExportFormat,
    /// Absolute output path: a directory for image formats, a file for HTML.
    pub output: PathBuf,
    /// Image scale factor (default 2).
    pub scale: f32,
    /// JPEG/WEBP quality 1-100.
    pub quality: Option<u8>,
}

/// Rendering services of the application hosting the engine.
pub trait ScriptHost {
    /// Renders `target` as PNG bytes for the agent to look at.
    fn screenshot(
        &mut self,
        scene: &SceneDoc,
        pipe: &mut Pipeline,
        target: &RenderTarget,
    ) -> Result<Vec<u8>, String> {
        screenshot_png(scene, pipe, target, SCREENSHOT_MAX_SIDE)
    }

    /// Writes the files of an `Export` call; returns their paths.
    fn export(
        &mut self,
        scene: &SceneDoc,
        pipe: &mut Pipeline,
        req: &ExportRequest,
    ) -> Result<Vec<PathBuf>, String> {
        export_images(scene, pipe, req)
    }
}

/// CPU host: everything through pen-raster.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeadlessHost;

impl ScriptHost for HeadlessHost {}

/// Longest side of a screenshot, in pixels (images cost tokens).
pub const SCREENSHOT_MAX_SIDE: u32 = 1568;

fn target_list(
    scene: &SceneDoc,
    pipe: &mut Pipeline,
    target: &RenderTarget,
) -> Result<(DisplayList, Rect), String> {
    match target {
        RenderTarget::Document => {
            let list = pipe.list.clone();
            let r = list.bounds().unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
            Ok((list, r))
        }
        RenderTarget::Node { id, key } => {
            let r = pipe
                .node_rect(scene, *key)
                .ok_or_else(|| format!("{id} has no layout (disabled?)"))?;
            Ok((pipe.node_list(scene, *key), r))
        }
    }
}

fn clamp_viewport(r: Rect) -> Rect {
    Rect::new(r.x, r.y, r.w.max(1.0), r.h.max(1.0))
}

/// Renders a target on white at 1x, scaled down to fit `max_side`.
pub fn screenshot_png(
    scene: &SceneDoc,
    pipe: &mut Pipeline,
    target: &RenderTarget,
    max_side: u32,
) -> Result<Vec<u8>, String> {
    let (list, r) = target_list(scene, pipe, target)?;
    let r = clamp_viewport(r);
    let longest = r.w.max(r.h);
    let scale = (max_side as f32 / longest).min(1.0);
    pen_raster::render_to_png(&list, r, scale, Color::WHITE).map_err(|e| e.to_string())
}

/// Renders a target to straight-alpha RGBA pixels at `scale`.
pub fn render_rgba(
    scene: &SceneDoc,
    pipe: &mut Pipeline,
    target: &RenderTarget,
    scale: f32,
) -> Result<image::RgbaImage, String> {
    let (list, r) = target_list(scene, pipe, target)?;
    let pix = pen_raster::render(&list, clamp_viewport(r), scale, Color::TRANSPARENT);
    let (w, h) = (pix.width(), pix.height());
    let mut out = image::RgbaImage::new(w, h);
    for (dst, src) in out.pixels_mut().zip(pix.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(out)
}

/// Exports PNG / JPEG / WEBP files (`<output>/<id>.<ext>`).
pub fn export_images(
    scene: &SceneDoc,
    pipe: &mut Pipeline,
    req: &ExportRequest,
) -> Result<Vec<PathBuf>, String> {
    let fmt = req.format;
    if !matches!(
        fmt,
        ExportFormat::Png | ExportFormat::Jpeg | ExportFormat::Webp
    ) {
        return Err(format!(
            "export format '{}' is not supported yet (use png, jpeg or webp)",
            fmt.extension()
        ));
    }
    std::fs::create_dir_all(&req.output)
        .map_err(|e| format!("cannot create {}: {e}", req.output.display()))?;
    let mut out = Vec::new();
    for t in &req.targets {
        let img = render_rgba(scene, pipe, t, req.scale)?;
        let path = req
            .output
            .join(format!("{}.{}", t.file_stem(), fmt.extension()));
        write_image(&img, fmt, req.quality, &path)?;
        out.push(path);
    }
    Ok(out)
}

fn write_image(
    img: &image::RgbaImage,
    fmt: ExportFormat,
    quality: Option<u8>,
    path: &Path,
) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("cannot write {}: {e}", path.display());
    let file = std::fs::File::create(path).map_err(|e| err(&e))?;
    let mut w = std::io::BufWriter::new(file);
    match fmt {
        ExportFormat::Png => img
            .write_to(&mut w, image::ImageFormat::Png)
            .map_err(|e| err(&e)),
        ExportFormat::Jpeg => {
            // JPEG has no alpha: flatten on white.
            let rgb = image::RgbImage::from_fn(img.width(), img.height(), |x, y| {
                let p = img.get_pixel(x, y).0;
                let a = p[3] as u32;
                let mix = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
                image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
            });
            let q = quality.unwrap_or(90).clamp(1, 100);
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, q);
            rgb.write_with_encoder(enc).map_err(|e| err(&e))
        }
        ExportFormat::Webp => {
            let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut w);
            img.write_with_encoder(enc).map_err(|e| err(&e))
        }
        _ => Err(err(&"unsupported format")),
    }
}
