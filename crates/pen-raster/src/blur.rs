//! Fast gaussian approximation: three successive box blurs per axis (sliding window sums).

use crate::geom::{IRect, box_sizes};

/// What the blur sees beyond the region edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Outside pixels are zero (shadows, layer blur).
    Transparent,
    /// Outside pixels repeat the edge pixel (backdrop blur).
    Clamp,
}

/// Reusable buffers so repeated blurs do not allocate.
#[derive(Default)]
pub struct BlurScratch {
    plane: Vec<u8>,
    plane_t: Vec<u8>,
    line: Vec<u8>,
}

/// Blurs channels `ch0..ch0 + nch` of the pixels inside `rect` in place.
///
/// `data` is row-major with `stride` bytes per row and `bpp` bytes per pixel. `rect` must lie inside
/// the image.
#[allow(clippy::too_many_arguments)]
pub fn blur_region(
    data: &mut [u8],
    stride: usize,
    bpp: usize,
    ch0: usize,
    nch: usize,
    rect: IRect,
    sigma: f32,
    edge: Edge,
    scratch: &mut BlurScratch,
) {
    if sigma < 0.2 || rect.is_empty() {
        return;
    }
    let mut boxes = [0usize; 3];
    box_sizes(sigma, &mut boxes);
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let (x0, y0) = (rect.x0 as usize, rect.y0 as usize);
    scratch.plane.resize(w * h, 0);
    scratch.plane_t.resize(w * h, 0);
    scratch.line.resize(w.max(h), 0);
    for ch in ch0..ch0 + nch {
        // Gather the channel into a contiguous plane.
        for y in 0..h {
            let row = &data[(y0 + y) * stride + x0 * bpp..];
            let dst = &mut scratch.plane[y * w..(y + 1) * w];
            for (x, d) in dst.iter_mut().enumerate() {
                *d = row[x * bpp + ch];
            }
        }
        // Horizontal passes.
        for y in 0..h {
            let row = &mut scratch.plane[y * w..(y + 1) * w];
            for b in boxes {
                box_line(row, &mut scratch.line[..w], b / 2, edge);
            }
        }
        transpose(&scratch.plane, &mut scratch.plane_t, w, h);
        // Vertical passes (rows of the transposed plane).
        for x in 0..w {
            let col = &mut scratch.plane_t[x * h..(x + 1) * h];
            for b in boxes {
                box_line(col, &mut scratch.line[..h], b / 2, edge);
            }
        }
        transpose(&scratch.plane_t, &mut scratch.plane, h, w);
        for y in 0..h {
            let row = &mut data[(y0 + y) * stride + x0 * bpp..];
            let src = &scratch.plane[y * w..(y + 1) * w];
            for (x, s) in src.iter().enumerate() {
                row[x * bpp + ch] = *s;
            }
        }
    }
}

fn transpose(src: &[u8], dst: &mut [u8], w: usize, h: usize) {
    const B: usize = 32;
    for by in (0..h).step_by(B) {
        for bx in (0..w).step_by(B) {
            for y in by..(by + B).min(h) {
                for x in bx..(bx + B).min(w) {
                    dst[x * h + y] = src[y * w + x];
                }
            }
        }
    }
}

/// One box blur of radius `r` over `line`, using `tmp` (same length) as scratch.
fn box_line(line: &mut [u8], tmp: &mut [u8], r: usize, edge: Edge) {
    let n = line.len();
    if r == 0 || n == 0 {
        return;
    }
    tmp.copy_from_slice(line);
    let d = (2 * r + 1) as u32;
    // Fixed point reciprocal: sum <= 255 * d, so sum * inv fits in u32 with 16 fractional bits only
    // when d is small; use u64 for the product to stay exact for any radius.
    let inv = (1u64 << 32) / d as u64;
    let (first, last) = (tmp[0] as u32, tmp[n - 1] as u32);
    let (lo, hi) = if edge == Edge::Clamp {
        (first, last)
    } else {
        (0, 0)
    };
    let at = |i: isize| -> u32 {
        if i < 0 {
            lo
        } else if i as usize >= n {
            hi
        } else {
            tmp[i as usize] as u32
        }
    };
    let ri = r as isize;
    let mut sum: u32 = (-ri..=ri).map(at).sum();
    let out = |sum: u32| ((sum as u64 * inv + (1u64 << 31)) >> 32).min(255) as u8;
    // Head and tail need bounds checks; the middle does not.
    let head = (r + 1).min(n);
    let tail_start = n.saturating_sub(r + 1).max(head);
    #[allow(clippy::needless_range_loop)]
    for i in 0..head {
        line[i] = out(sum);
        sum = sum + at(i as isize + ri + 1) - at(i as isize - ri);
    }
    for i in head..tail_start {
        line[i] = out(sum);
        sum = sum + tmp[i + r + 1] as u32 - tmp[i - r] as u32;
    }
    #[allow(clippy::needless_range_loop)]
    for i in tail_start..n {
        line[i] = out(sum);
        sum = sum + at(i as isize + ri + 1) - at(i as isize - ri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_preserves_mass_and_spreads() {
        let (w, h) = (41usize, 41usize);
        let mut img = vec![0u8; w * h];
        for y in 15..26 {
            for x in 15..26 {
                img[y * w + x] = 255;
            }
        }
        let before: u64 = img.iter().map(|v| *v as u64).sum();
        let mut s = BlurScratch::default();
        blur_region(
            &mut img,
            w,
            1,
            0,
            1,
            IRect::from_size(w as u32, h as u32),
            3.0,
            Edge::Transparent,
            &mut s,
        );
        let after: u64 = img.iter().map(|v| *v as u64).sum();
        let err = (before as f64 - after as f64).abs() / before as f64;
        assert!(err < 0.02, "mass changed by {err}");
        assert!(
            img[20 * w + 12] > 0,
            "blur should spread outside the square"
        );
        assert!(img[20 * w + 20] < 255 && img[20 * w + 20] > 200);
        // Symmetric.
        assert_eq!(img[20 * w + 10], img[20 * w + 30]);
        assert_eq!(img[10 * w + 20], img[30 * w + 20]);
    }

    #[test]
    fn clamp_edge_keeps_flat_image_flat() {
        let mut img = vec![128u8; 16 * 16];
        let mut s = BlurScratch::default();
        blur_region(
            &mut img,
            16,
            1,
            0,
            1,
            IRect::from_size(16, 16),
            4.0,
            Edge::Clamp,
            &mut s,
        );
        assert!(img.iter().all(|v| *v == 128));
    }

    #[test]
    fn premultiplied_invariant_kept() {
        // Color channel never exceeds alpha after blurring premultiplied RGBA.
        let (w, h) = (20usize, 20usize);
        let mut img = vec![0u8; w * h * 4];
        for y in 5..15 {
            for x in 5..15 {
                let p = &mut img[(y * w + x) * 4..];
                p[..4].copy_from_slice(&[200, 30, 100, 200]);
            }
        }
        let mut s = BlurScratch::default();
        blur_region(
            &mut img,
            w * 4,
            4,
            0,
            4,
            IRect::new(2, 2, 18, 18),
            2.5,
            Edge::Transparent,
            &mut s,
        );
        for p in img.chunks(4) {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]);
        }
    }
}
