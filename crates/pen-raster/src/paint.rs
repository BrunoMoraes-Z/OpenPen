//! Paint -> tiny-skia shader conversion, image cache (with mip levels) and conic gradients.

use crate::geom::IRect;
use pen_render::{Color, GradientStop, ImageData, ImageFit, Paint, Rect};
use std::collections::HashMap;
use std::sync::Arc;
use tiny_skia::{
    FilterQuality, IntSize, LinearGradient, Pattern, Pixmap, Point, PremultipliedColorU8,
    RadialGradient, Shader, SpreadMode, Transform,
};

/// Converts a straight sRGB color into a tiny-skia color (clamped).
pub fn ts_color(c: Color) -> tiny_skia::Color {
    let f = |v: f32| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    tiny_skia::Color::from_rgba(f(c.r), f(c.g), f(c.b), f(c.a))
        .unwrap_or(tiny_skia::Color::TRANSPARENT)
}

/// Premultiplied 8-bit color.
pub fn premul_u8(c: Color) -> [u8; 4] {
    let c = ts_color(c).premultiply().to_color_u8();
    [c.red(), c.green(), c.blue(), c.alpha()]
}

fn stops(s: &[GradientStop]) -> Vec<tiny_skia::GradientStop> {
    s.iter()
        .map(|s| tiny_skia::GradientStop::new(s.offset.clamp(0.0, 1.0), ts_color(s.color)))
        .collect()
}

fn last_color(s: &[GradientStop]) -> Option<Shader<'static>> {
    s.last().map(|s| Shader::SolidColor(ts_color(s.color)))
}

/// Samples a gradient at `t` (straight color, linear interpolation between stops).
pub fn sample_stops(s: &[GradientStop], t: f32) -> Color {
    let Some(first) = s.first() else {
        return Color::TRANSPARENT;
    };
    if t <= first.offset {
        return first.color;
    }
    for w in s.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if t <= b.offset {
            let span = b.offset - a.offset;
            let k = if span > 1e-6 {
                (t - a.offset) / span
            } else {
                1.0
            };
            let l = |x: f32, y: f32| x + (y - x) * k;
            return Color::rgba(
                l(a.color.r, b.color.r),
                l(a.color.g, b.color.g),
                l(a.color.b, b.color.b),
                l(a.color.a, b.color.a),
            );
        }
    }
    s.last().map(|s| s.color).unwrap_or_default()
}

/// Local-space mapping (scale x/y, translate) of an image of `iw x ih` into `b` for `fit`.
pub fn image_mapping(
    fit: ImageFit,
    scale: f32,
    b: &Rect,
    iw: f32,
    ih: f32,
) -> (f32, f32, f32, f32) {
    match fit {
        ImageFit::Stretch => (b.w / iw, b.h / ih, b.x, b.y),
        ImageFit::Cover | ImageFit::Contain => {
            let (sx, sy) = (b.w / iw, b.h / ih);
            let s = if fit == ImageFit::Cover {
                sx.max(sy)
            } else {
                sx.min(sy)
            };
            (s, s, b.x + (b.w - iw * s) * 0.5, b.y + (b.h - ih * s) * 0.5)
        }
        ImageFit::Tile => {
            let s = if scale > 0.0 && scale.is_finite() {
                scale
            } else {
                1.0
            };
            (s, s, b.x, b.y)
        }
    }
}

struct Level {
    raw: Pixmap,
    /// Same image with a 1px transparent border (emulates "decal" sampling with Pad spread).
    padded: Option<Pixmap>,
}

struct CachedImage {
    levels: Vec<Level>,
    last_used: u64,
}

/// CPU image cache keyed by [`ImageData::id`].
#[derive(Default)]
pub struct ImageCache {
    map: HashMap<u64, CachedImage>,
    bytes: usize,
    clock: u64,
    /// Soft memory budget; least-recently-used images are evicted beyond it.
    pub budget: usize,
}

impl ImageCache {
    pub fn new() -> Self {
        Self {
            budget: 512 << 20,
            ..Default::default()
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    /// Makes sure level `level` (and its padded copy if `padded`) exists; returns the level actually available.
    pub fn prepare(&mut self, img: &Arc<ImageData>, level: usize, padded: bool) -> Option<usize> {
        self.clock += 1;
        if !self.map.contains_key(&img.id) {
            let size = IntSize::from_wh(img.width, img.height)?;
            if img.rgba_premul.len() != (img.width as usize) * (img.height as usize) * 4 {
                return None;
            }
            let raw = Pixmap::from_vec(img.rgba_premul.clone(), size)?;
            self.bytes += raw.data().len();
            self.map.insert(
                img.id,
                CachedImage {
                    levels: vec![Level { raw, padded: None }],
                    last_used: 0,
                },
            );
            self.evict(img.id);
        }
        let entry = self.map.get_mut(&img.id)?;
        entry.last_used = self.clock;
        while entry.levels.len() <= level {
            let prev = &entry.levels[entry.levels.len() - 1].raw;
            if prev.width() <= 1 && prev.height() <= 1 {
                break;
            }
            let next = downsample(prev)?;
            self.bytes += next.data().len();
            entry.levels.push(Level {
                raw: next,
                padded: None,
            });
        }
        let level = level.min(entry.levels.len() - 1);
        let l = &mut entry.levels[level];
        if padded && l.padded.is_none() {
            let p = pad(&l.raw)?;
            self.bytes += p.data().len();
            l.padded = Some(p);
        }
        Some(level)
    }

    fn evict(&mut self, keep: u64) {
        while self.bytes > self.budget && self.map.len() > 1 {
            let Some((&id, _)) = self
                .map
                .iter()
                .filter(|(id, _)| **id != keep)
                .min_by_key(|(_, e)| e.last_used)
            else {
                break;
            };
            if let Some(e) = self.map.remove(&id) {
                for l in e.levels {
                    self.bytes -= l.raw.data().len();
                    if let Some(p) = l.padded {
                        self.bytes -= p.data().len();
                    }
                }
            }
        }
    }

    fn get(&self, id: u64, level: usize, padded: bool) -> Option<&Pixmap> {
        let l = self.map.get(&id)?.levels.get(level)?;
        if padded {
            l.padded.as_ref()
        } else {
            Some(&l.raw)
        }
    }
}

/// 2x2 box downsample (rounding dimensions up).
fn downsample(src: &Pixmap) -> Option<Pixmap> {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (dw, dh) = (sw.div_ceil(2).max(1), sh.div_ceil(2).max(1));
    let mut dst = Pixmap::new(dw as u32, dh as u32)?;
    let s = src.data();
    let d = dst.data_mut();
    for y in 0..dh {
        for x in 0..dw {
            let mut acc = [0u32; 4];
            let mut n = 0;
            for (yy, xx) in [
                (2 * y, 2 * x),
                (2 * y, 2 * x + 1),
                (2 * y + 1, 2 * x),
                (2 * y + 1, 2 * x + 1),
            ] {
                if yy < sh && xx < sw {
                    let i = (yy * sw + xx) * 4;
                    for c in 0..4 {
                        acc[c] += s[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let o = (y * dw + x) * 4;
            for c in 0..4 {
                d[o + c] = ((acc[c] + n / 2) / n) as u8;
            }
        }
    }
    Some(dst)
}

fn pad(src: &Pixmap) -> Option<Pixmap> {
    let (w, h) = (src.width() as usize, src.height() as usize);
    let mut dst = Pixmap::new(w as u32 + 2, h as u32 + 2)?;
    let dw = (w + 2) * 4;
    let s = src.data();
    let d = dst.data_mut();
    for y in 0..h {
        d[(y + 1) * dw + 4..(y + 1) * dw + 4 + w * 4]
            .copy_from_slice(&s[y * w * 4..(y + 1) * w * 4]);
    }
    Some(dst)
}

/// Picks the mip level for an image drawn at `device_scale` (device px per image px).
pub fn mip_level(device_scale: f32) -> usize {
    if device_scale.is_nan() || device_scale <= 0.0 || device_scale >= 0.5 {
        return 0;
    }
    ((1.0 / device_scale).log2().floor() as usize).min(16)
}

/// Prepares any cached data `paint` needs (images). Call before [`shader`].
pub fn prepare(
    paint: &Paint,
    bounds: &Rect,
    device_scale: f32,
    images: &mut ImageCache,
) -> Option<usize> {
    if let Paint::Image {
        image, fit, scale, ..
    } = paint
    {
        let (iw, ih) = (image.width as f32, image.height as f32);
        if iw <= 0.0 || ih <= 0.0 {
            return None;
        }
        let (sx, sy, _, _) = image_mapping(*fit, *scale, bounds, iw, ih);
        let level = mip_level(sx.abs().min(sy.abs()) * device_scale);
        return images.prepare(image, level, *fit != ImageFit::Tile);
    }
    Some(0)
}

/// Builds a shader for `paint` in the item's local space. `bounds` is the shape's local box.
/// Returns `None` when nothing should be drawn. Angular gradients are handled by the caller.
pub fn shader<'a>(
    paint: &Paint,
    bounds: &Rect,
    level: usize,
    images: &'a ImageCache,
) -> Option<Shader<'a>> {
    match paint {
        Paint::Solid(c) => (c.a > 0.0).then(|| Shader::SolidColor(ts_color(*c))),
        Paint::Linear {
            start,
            end,
            stops: s,
        } => {
            if s.is_empty() {
                return None;
            }
            LinearGradient::new(
                Point::from_xy(start.x, start.y),
                Point::from_xy(end.x, end.y),
                stops(s),
                SpreadMode::Pad,
                Transform::identity(),
            )
            .or_else(|| last_color(s))
        }
        Paint::Radial {
            center,
            radius,
            stops: s,
        } => {
            if s.is_empty() {
                return None;
            }
            if radius.x.abs() < 1e-6 || radius.y.abs() < 1e-6 {
                return last_color(s);
            }
            let ts = Transform::from_row(radius.x, 0.0, 0.0, radius.y, center.x, center.y);
            RadialGradient::new(
                Point::zero(),
                Point::zero(),
                1.0,
                stops(s),
                SpreadMode::Pad,
                ts,
            )
            .or_else(|| last_color(s))
        }
        Paint::Angular { .. } => None,
        Paint::Image {
            image,
            fit,
            opacity,
            scale,
        } => {
            let (iw, ih) = (image.width as f32, image.height as f32);
            if iw <= 0.0 || ih <= 0.0 || *opacity <= 0.0 {
                return None;
            }
            let tile = *fit == ImageFit::Tile;
            let pix = images.get(image.id, level, !tile)?;
            let (sx, sy, tx, ty) = image_mapping(*fit, *scale, bounds, iw, ih);
            // Level pixmaps are smaller: scale back up to the original image size.
            let (pw, ph) = if tile {
                (pix.width() as f32, pix.height() as f32)
            } else {
                (pix.width() as f32 - 2.0, pix.height() as f32 - 2.0)
            };
            let (lx, ly) = (iw / pw, ih / ph);
            let mut ts = Transform::from_row(sx * lx, 0.0, 0.0, sy * ly, tx, ty);
            if !tile {
                ts = ts.pre_translate(-1.0, -1.0);
            }
            Some(Pattern::new(
                pix.as_ref(),
                if tile {
                    SpreadMode::Repeat
                } else {
                    SpreadMode::Pad
                },
                FilterQuality::Bilinear,
                opacity.clamp(0.0, 1.0),
                ts,
            ))
        }
    }
}

/// Renders a conic gradient into `out` covering device region `region` (target-local pixels).
/// `inv` maps device -> item local space.
pub fn render_conic(
    out: &mut Pixmap,
    region: IRect,
    inv: &Transform,
    center: pen_render::Vec2,
    start_angle: f32,
    s: &[GradientStop],
) {
    const N: usize = 1024;
    let mut lut = [PremultipliedColorU8::TRANSPARENT; N];
    for (i, e) in lut.iter_mut().enumerate() {
        let c = sample_stops(s, i as f32 / (N - 1) as f32);
        let [r, g, b, a] = premul_u8(c);
        *e = PremultipliedColorU8::from_rgba(r, g, b, a)
            .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
    let w = out.width() as usize;
    let tau = std::f32::consts::TAU;
    let pixels = out.pixels_mut();
    for y in 0..region.height() as usize {
        for x in 0..region.width() as usize {
            let dx = region.x0 as f32 + x as f32 + 0.5;
            let dy = region.y0 as f32 + y as f32 + 0.5;
            let lx = inv.sx * dx + inv.kx * dy + inv.tx;
            let ly = inv.ky * dx + inv.sy * dy + inv.ty;
            let a = (ly - center.y).atan2(lx - center.x) - start_angle;
            let t = (a / tau).rem_euclid(1.0);
            pixels[y * w + x] = lut[((t * (N - 1) as f32) as usize).min(N - 1)];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_and_contain_mapping() {
        let b = Rect::new(0.0, 0.0, 100.0, 50.0);
        let (sx, sy, tx, ty) = image_mapping(ImageFit::Cover, 1.0, &b, 10.0, 10.0);
        assert_eq!((sx, sy, tx, ty), (10.0, 10.0, 0.0, -25.0));
        let (sx, _, tx, ty) = image_mapping(ImageFit::Contain, 1.0, &b, 10.0, 10.0);
        assert_eq!((sx, tx, ty), (5.0, 25.0, 0.0));
        let (sx, sy, ..) = image_mapping(ImageFit::Stretch, 1.0, &b, 10.0, 10.0);
        assert_eq!((sx, sy), (10.0, 5.0));
        let (sx, ..) = image_mapping(ImageFit::Tile, 0.5, &b, 10.0, 10.0);
        assert_eq!(sx, 0.5);
    }

    #[test]
    fn stop_sampling() {
        let s = [
            GradientStop {
                offset: 0.0,
                color: Color::BLACK,
            },
            GradientStop {
                offset: 1.0,
                color: Color::WHITE,
            },
        ];
        let c = sample_stops(&s, 0.5);
        assert!((c.r - 0.5).abs() < 1e-6 && (c.a - 1.0).abs() < 1e-6);
        assert_eq!(sample_stops(&s, -1.0), Color::BLACK);
        assert_eq!(sample_stops(&s, 2.0), Color::WHITE);
    }

    #[test]
    fn mip_levels() {
        assert_eq!(mip_level(1.0), 0);
        assert_eq!(mip_level(0.5), 0);
        assert_eq!(mip_level(0.3), 1);
        assert_eq!(mip_level(0.1), 3);
    }
}
