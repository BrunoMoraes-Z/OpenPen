//! Textures sampled by shaders: images, `pencil:object` / `pencil:background` bakes and
//! the `@sdf` distance field. Bilinear filtering, optional box-filtered mipmaps.

use std::sync::Arc;

use pen_raster::Pixmap;
use pen_render::{Rect, Vec2};

/// Behaviour outside the texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    Clamp,
    /// Transparent black (`pencil:object`).
    Transparent,
    /// Mirrored (`pencil:background`).
    Mirror,
}

/// How texture coordinates map to texels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Map {
    /// `uv` (0..1, v up) over the whole image (loaded with FLIP_Y).
    Image,
    /// `uv` over the node box (`w` x `h` units, v up); the texels cover `region`
    /// (node-local, y down) at `scale` x `scale_y` texels per unit.
    Node {
        w: f32,
        h: f32,
        rx: f32,
        ry: f32,
        scale: f32,
        scale_y: f32,
    },
}

#[derive(Debug)]
struct Level {
    w: u32,
    h: u32,
    /// Premultiplied RGBA, 0..1.
    px: Vec<[f32; 4]>,
}

/// A sampled texture (cheap to clone).
#[derive(Debug, Clone)]
pub struct Texture {
    levels: Arc<Vec<Level>>,
    wrap: Wrap,
    map: Map,
    /// Pixels per node unit used to bake node textures (for `textureSize`).
    scale_hint: f32,
    /// Values outside 0..1 (the SDF): needs a float texture on the GPU.
    hdr: bool,
}

impl Texture {
    pub fn from_premul_rgba8(
        w: u32,
        h: u32,
        data: &[u8],
        map: Map,
        wrap: Wrap,
        mipmap: bool,
    ) -> Texture {
        let px = data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                    p[3] as f32 / 255.0,
                ]
            })
            .collect();
        Self::from_levels(vec![Level { w, h, px }], map, wrap, mipmap)
    }

    fn from_levels(mut levels: Vec<Level>, map: Map, wrap: Wrap, mipmap: bool) -> Texture {
        if mipmap {
            while let Some(l) = levels.last() {
                if l.w <= 1 && l.h <= 1 {
                    break;
                }
                let (w, h) = ((l.w / 2).max(1), (l.h / 2).max(1));
                let mut px = vec![[0.0f32; 4]; (w * h) as usize];
                for y in 0..h {
                    for x in 0..w {
                        let mut acc = [0.0f32; 4];
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let sx = (x * 2 + dx).min(l.w - 1);
                            let sy = (y * 2 + dy).min(l.h - 1);
                            let p = l.px[(sy * l.w + sx) as usize];
                            for c in 0..4 {
                                acc[c] += p[c] * 0.25;
                            }
                        }
                        px[(y * w + x) as usize] = acc;
                    }
                }
                levels.push(Level { w, h, px });
            }
        }
        Texture {
            levels: Arc::new(levels),
            wrap,
            map,
            scale_hint: 1.0,
            hdr: false,
        }
    }

    /// Mip levels: (width, height, premultiplied RGBA 0..1).
    pub fn levels(&self) -> impl Iterator<Item = (u32, u32, &[[f32; 4]])> {
        self.levels.iter().map(|l| (l.w, l.h, l.px.as_slice()))
    }

    pub fn is_hdr(&self) -> bool {
        self.hdr
    }

    pub fn wrap(&self) -> Wrap {
        self.wrap
    }

    /// `(a, b, c, d)` with normalized texture coordinates (origin top-left)
    /// `t = (u * a + b, v * c + d)`, as [`Self::sample`] maps them.
    pub fn gpu_map(&self) -> [f32; 4] {
        let l = &self.levels[0];
        let (tw, th) = (l.w as f32, l.h as f32);
        match self.map {
            Map::Image => [1.0, 0.0, -1.0, 1.0],
            Map::Node {
                w,
                h,
                rx,
                ry,
                scale,
                scale_y,
            } => [
                w * scale / tw,
                -rx * scale / tw,
                -h * scale_y / th,
                (h - ry) * scale_y / th,
            ],
        }
    }

    pub fn with_scale_hint(mut self, s: f32) -> Self {
        self.scale_hint = s;
        self
    }

    /// `textureSize`: image pixels, or the node size for node textures.
    pub fn size(&self) -> [i32; 2] {
        match self.map {
            Map::Image => [self.levels[0].w as i32, self.levels[0].h as i32],
            Map::Node { w, h, .. } => [w.round().max(1.0) as i32, h.round().max(1.0) as i32],
        }
    }

    /// Texel coordinates (level 0) of `uv`.
    fn texel(&self, uv: [f32; 2]) -> (f32, f32) {
        let l = &self.levels[0];
        match self.map {
            Map::Image => (uv[0] * l.w as f32, (1.0 - uv[1]) * l.h as f32),
            Map::Node {
                w,
                h,
                rx,
                ry,
                scale,
                scale_y,
            } => {
                let x = uv[0] * w;
                let y = (1.0 - uv[1]) * h;
                ((x - rx) * scale, (y - ry) * scale_y)
            }
        }
    }

    fn fetch(&self, level: &Level, x: i32, y: i32) -> [f32; 4] {
        let (w, h) = (level.w as i32, level.h as i32);
        let (x, y) = match self.wrap {
            Wrap::Clamp => (x.clamp(0, w - 1), y.clamp(0, h - 1)),
            Wrap::Transparent => {
                if x < 0 || y < 0 || x >= w || y >= h {
                    return [0.0; 4];
                }
                (x, y)
            }
            Wrap::Mirror => (mirror(x, w), mirror(y, h)),
        };
        level.px[(y * w + x) as usize]
    }

    fn bilinear(&self, li: usize, tx: f32, ty: f32) -> [f32; 4] {
        let level = &self.levels[li];
        let k = (1u32 << li) as f32;
        let (x, y) = (tx / k - 0.5, ty / k - 0.5);
        if !(x.is_finite() && y.is_finite()) {
            return [0.0; 4];
        }
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (xi, yi) = (x0 as i32, y0 as i32);
        let a = self.fetch(level, xi, yi);
        let b = self.fetch(level, xi + 1, yi);
        let c = self.fetch(level, xi, yi + 1);
        let d = self.fetch(level, xi + 1, yi + 1);
        let mut o = [0.0; 4];
        for i in 0..4 {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bot = c[i] + (d[i] - c[i]) * fx;
            o[i] = top + (bot - top) * fy;
        }
        o
    }

    /// Samples at `uv` (v up), mip level `lod` (trilinear when mipmapped).
    pub fn sample(&self, uv: [f32; 2], lod: f32) -> [f32; 4] {
        let (tx, ty) = self.texel(uv);
        let max = (self.levels.len() - 1) as f32;
        let lod = lod.clamp(0.0, max);
        let l0 = lod.floor() as usize;
        let a = self.bilinear(l0, tx, ty);
        let f = lod - l0 as f32;
        if f <= 0.0 || l0 + 1 >= self.levels.len() {
            return a;
        }
        let b = self.bilinear(l0 + 1, tx, ty);
        [
            a[0] + (b[0] - a[0]) * f,
            a[1] + (b[1] - a[1]) * f,
            a[2] + (b[2] - a[2]) * f,
            a[3] + (b[3] - a[3]) * f,
        ]
    }
}

fn mirror(i: i32, n: i32) -> i32 {
    if n <= 1 {
        return 0;
    }
    let period = 2 * n;
    let m = i.rem_euclid(period);
    if m < n { m } else { period - 1 - m }
}

/// 1D squared Euclidean distance transform (Felzenszwalb & Huttenlocher).
fn edt_1d(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        let fq = f[q] + (q * q) as f32;
        let mut s;
        loop {
            let p = v[k];
            s = (fq - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 {
                k -= 1;
            } else {
                break;
            }
        }
        if s <= z[k] {
            // k == 0 and the new parabola dominates from the start.
            v[0] = q;
            z[0] = f32::NEG_INFINITY;
            z[1] = f32::INFINITY;
            continue;
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let d = q as f32 - p as f32;
        *o = d * d + f[p];
    }
}

/// Squared distance from every texel to the nearest texel where `seed` is true.
fn edt_2d(seed: &[bool], w: usize, h: usize) -> Vec<f32> {
    const INF: f32 = 1e20;
    let mut g: Vec<f32> = seed.iter().map(|s| if *s { 0.0 } else { INF }).collect();
    let n = w.max(h);
    let mut f = vec![0.0f32; n];
    let mut out = vec![0.0f32; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f32; n + 1];
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        edt_1d(&f[..h], &mut out[..h], &mut v, &mut z);
        for y in 0..h {
            g[y * w + x] = out[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..(y + 1) * w]);
        edt_1d(&f[..w], &mut out[..w], &mut v, &mut z);
        g[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
    }
    g
}

/// `@sdf` texture of a coverage mask (alpha, edge at 50%) baked over `region` at `scale`:
/// `r` = signed distance in node units (positive inside), `gb` = unit gradient of the
/// distance (direction of increasing distance, texel space with y up like `uv`).
pub fn sdf_texture(mask: &Pixmap, region: Rect, size: Vec2, scale: f32) -> Texture {
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let alpha: Vec<f32> = mask
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[3] as f32 / 255.0)
        .collect();
    let inside: Vec<bool> = alpha.iter().map(|a| *a >= 0.5).collect();
    // Distances on a grid padded by one texel of "outside" on every side: a shape that
    // touches the texture border (a fill covering the node box) has its edge there.
    let (pw, ph) = (w + 2, h + 2);
    let mut outside_p = vec![true; pw * ph];
    for y in 0..h {
        for x in 0..w {
            outside_p[(y + 1) * pw + x + 1] = !inside[y * w + x];
        }
    }
    let inside_p: Vec<bool> = outside_p.iter().map(|o| !*o).collect();
    let unpad = |d: Vec<f32>| -> Vec<f32> {
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            out.extend_from_slice(&d[(y + 1) * pw + 1..(y + 1) * pw + 1 + w]);
        }
        out
    };
    let d_out = unpad(edt_2d(&outside_p, pw, ph));
    let d_in = unpad(edt_2d(&inside_p, pw, ph));
    let sx = w as f32 / region.w.max(1e-3);
    let mut sd = vec![0.0f32; w * h];
    for i in 0..w * h {
        // Sub-texel edge from the coverage of edge texels.
        let a = alpha[i];
        let d = if inside[i] {
            d_out[i].sqrt() - 0.5
        } else {
            -(d_in[i].sqrt() - 0.5)
        };
        let d = if a > 0.0 && a < 1.0 && d.abs() <= 1.0 {
            a - 0.5
        } else {
            d
        };
        sd[i] = d / sx;
    }
    let _ = scale;
    let at = |x: isize, y: isize| -> f32 {
        let x = x.clamp(0, w as isize - 1) as usize;
        let y = y.clamp(0, h as isize - 1) as usize;
        sd[y * w + x]
    };
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h as isize {
        for x in 0..w as isize {
            let gx = at(x + 1, y) - at(x - 1, y);
            // Rows go down; the gradient is reported with y up.
            let gy = -(at(x, y + 1) - at(x, y - 1));
            let l = (gx * gx + gy * gy).sqrt().max(1e-6);
            px.push([at(x, y), gx / l, gy / l, 1.0]);
        }
    }
    let map = Map::Node {
        w: size.x,
        h: size.y,
        rx: region.x,
        ry: region.y,
        scale: sx,
        scale_y: h as f32 / region.h.max(1e-3),
    };
    let mut t = Texture::from_levels(
        vec![Level {
            w: w as u32,
            h: h as u32,
            px,
        }],
        map,
        Wrap::Clamp,
        false,
    );
    t.hdr = true;
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edt_matches_brute_force() {
        let (w, h) = (9, 7);
        let seed: Vec<bool> = (0..w * h).map(|i| i == 3 * w + 4 || i == 10).collect();
        let d = edt_2d(&seed, w, h);
        for y in 0..h {
            for x in 0..w {
                let mut best = f32::MAX;
                for (i, s) in seed.iter().enumerate() {
                    if *s {
                        let (sx, sy) = ((i % w) as f32, (i / w) as f32);
                        best = best.min((x as f32 - sx).powi(2) + (y as f32 - sy).powi(2));
                    }
                }
                assert_eq!(d[y * w + x], best, "{x},{y}");
            }
        }
    }

    #[test]
    fn edt_of_a_dense_mask_matches_brute_force() {
        let (w, h) = (61, 47);
        let seed: Vec<bool> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 - 30.0, (i / w) as f32 - 23.0);
                (x / 28.0).powi(2) + (y / 19.0).powi(2) > 1.0
            })
            .collect();
        let d = edt_2d(&seed, w, h);
        let pts: Vec<(f32, f32)> = (0..w * h)
            .filter(|i| seed[*i])
            .map(|i| ((i % w) as f32, (i / w) as f32))
            .collect();
        for y in 0..h {
            for x in 0..w {
                let best = pts
                    .iter()
                    .map(|(sx, sy)| (x as f32 - sx).powi(2) + (y as f32 - sy).powi(2))
                    .fold(f32::MAX, f32::min);
                assert_eq!(d[y * w + x], best, "{x},{y}");
            }
        }
    }

    #[test]
    fn image_uv_is_flipped_and_mirror_wraps() {
        // 1x2 image: top red, bottom blue.
        let data = [255, 0, 0, 255, 0, 0, 255, 255];
        let t = Texture::from_premul_rgba8(1, 2, &data, Map::Image, Wrap::Clamp, false);
        assert_eq!(t.sample([0.5, 0.0], 0.0), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(t.sample([0.5, 1.0], 0.0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(mirror(-1, 4), 0);
        assert_eq!(mirror(4, 4), 3);
        assert_eq!(mirror(9, 4), 1);
    }
}
