//! Shader rendering: compile cache, uniform binding, textures and the parallel pixel loop.
//! [`ShaderEngine`] implements [`pen_layout::ShaderRenderer`].

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use naga::{ScalarKind, TypeInner};
use pen_doc::ParamValue;
use pen_layout::shader::{ShaderLookup, ShaderNeeds, ShaderRenderer, ShaderRequest};
use pen_render::{
    Affine2, Chunk, Color, DisplayItem, DisplayList, ImageData, Paint, Rect, Shape, Vec2,
};
use smallvec::smallvec;

use crate::eval::{EvalError, Exec, Program, Textures, V};
use crate::glsl::{self, Role, UType, Uniform};
use crate::gpu::{self, Gpu, GpuProgram};
use crate::texture::{Texture, Wrap, sdf_texture};

/// A parsed shader.
pub struct Compiled {
    pub program: Program,
    pub uniforms: Vec<Uniform>,
    rw: glsl::Rewritten,
    gpu: Mutex<Option<Result<Arc<GpuProgram>, String>>>,
}

impl Compiled {
    /// Compiles `.pen` shader source.
    pub fn new(source: &str) -> Result<Compiled, String> {
        let rw = glsl::rewrite(source);
        let module = glsl::parse(&rw).map_err(|e| e.to_string())?;
        let program = Program::new(module).map_err(|e| match e {
            EvalError::Unsupported(m) => format!("unsupported: {m}"),
            EvalError::TooLong => "too slow".into(),
        })?;
        Ok(Compiled {
            program,
            uniforms: rw.uniforms.clone(),
            rw,
            gpu: Mutex::new(None),
        })
    }

    /// The GPU pipeline of this shader (built once; `Err` = use the CPU evaluator).
    pub fn gpu_program(&self) -> Result<(&'static Gpu, Arc<GpuProgram>), String> {
        let g = gpu::gpu().ok_or("no GPU adapter")?;
        let mut slot = self.gpu.lock().unwrap_or_else(|e| e.into_inner());
        let r = slot.get_or_insert_with(|| g.program(&self.rw).map(Arc::new));
        r.clone().map(|p| (g, p))
    }

    /// Value a sampler uniform is bound to (`uniforms` first, then `@default`).
    fn sampler_source(&self, u: &Uniform, given: &[(Arc<str>, ParamValue)]) -> Option<String> {
        if u.role == Role::Sdf {
            return Some("pencil:sdf".into());
        }
        match given.iter().rev().find(|(k, _)| **k == *u.name) {
            Some((_, ParamValue::Str(s))) => Some(s.to_string()),
            _ => u.default.clone(),
        }
    }

    pub fn needs(&self, given: &[(Arc<str>, ParamValue)]) -> ShaderNeeds {
        let mut n = ShaderNeeds::default();
        for u in self.uniforms.iter().filter(|u| u.ty == UType::Sampler2D) {
            match self.sampler_source(u, given).as_deref() {
                Some("pencil:object") => n.object = true,
                Some("pencil:background") => n.backdrop = true,
                _ => {}
            }
        }
        n
    }
}

/// Rendering limits/quality.
#[derive(Debug, Clone, Copy)]
pub struct ShaderLimits {
    /// Use the GPU when an adapter is available.
    pub gpu: bool,
    /// Max output pixels of one GPU evaluation (the resolution drops below
    /// `max_scale` to stay within it).
    pub gpu_max_pixels: f32,
    /// Same for the CPU evaluator (much slower).
    pub cpu_max_pixels: f32,
    /// Max pixels per node unit.
    pub max_scale: f32,
    /// Worker threads of the CPU pixel loop (0 = all cores).
    pub threads: usize,
}

impl Default for ShaderLimits {
    fn default() -> Self {
        Self {
            gpu: true,
            gpu_max_pixels: 2_000_000.0,
            cpu_max_pixels: 40_000.0,
            max_scale: 2.0,
            threads: 0,
        }
    }
}

impl ShaderLimits {
    /// CPU only (deterministic: golden tests).
    pub fn cpu() -> Self {
        Self {
            gpu: false,
            ..Self::default()
        }
    }
}

/// Pixels per unit used for a region within a pixel budget.
pub fn pick_scale(region: &Rect, max_pixels: f32, max_scale: f32) -> f32 {
    let area = (region.w * region.h).max(1.0);
    (max_pixels / area).sqrt().min(max_scale).max(0.05)
}

static IMAGE_IDS: AtomicU64 = AtomicU64::new(1 << 62);

/// Inputs of one evaluation.
pub struct RenderInput<'a> {
    pub compiled: &'a Compiled,
    pub uniforms: &'a [(Arc<str>, ParamValue)],
    pub size: Vec2,
    pub region: Rect,
    pub scale: f32,
    pub base_dir: Option<&'a Path>,
    pub shape: Option<&'a Shape>,
    pub object: Option<&'a DisplayList>,
    pub backdrop: Option<&'a DisplayList>,
    /// CPU threads (0 = all cores).
    pub threads: usize,
    /// Run on the GPU (falls back to the CPU on failure).
    pub gpu: bool,
}

struct TexSet<'a> {
    list: &'a [Texture],
}

impl Textures for TexSet<'_> {
    fn sample(&self, slot: i32, uv: [f32; 2], lod: f32) -> [f32; 4] {
        match usize::try_from(slot).ok().and_then(|s| self.list.get(s)) {
            Some(t) => t.sample(uv, lod),
            None => [0.0; 4],
        }
    }
    fn size(&self, slot: i32) -> [i32; 2] {
        match usize::try_from(slot).ok().and_then(|s| self.list.get(s)) {
            Some(t) => t.size(),
            None => [1, 1],
        }
    }
}

fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    let v = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    let one = |i: usize| {
        u8::from_str_radix(h.get(i..i + 1)?, 16)
            .ok()
            .map(|x| x * 17)
    };
    let px = match h.len() {
        3 => [one(0)?, one(1)?, one(2)?, 255],
        4 => [one(0)?, one(1)?, one(2)?, one(3)?],
        6 => [v(0)?, v(2)?, v(4)?, 255],
        8 => [v(0)?, v(2)?, v(4)?, v(6)?],
        _ => return None,
    };
    Some(px.map(|c| c as f32 / 255.0))
}

/// Numbers of a uniform value (`3`, `[1, 2]`, `"1, 2"`, `"#ff0000"`, `true`).
fn numbers(v: &ParamValue) -> Vec<f32> {
    match v {
        ParamValue::Num(n) => vec![*n as f32],
        ParamValue::Bool(b) => vec![*b as u8 as f32],
        ParamValue::Vec(v) => v.iter().map(|x| *x as f32).collect(),
        ParamValue::Str(s) => text_numbers(s),
    }
}

fn text_numbers(s: &str) -> Vec<f32> {
    let s = s.trim();
    if let Some(c) = parse_hex(s) {
        return c.to_vec();
    }
    match s {
        "true" => return vec![1.0],
        "false" => return vec![0.0],
        _ => {}
    }
    s.trim_start_matches(['[', '('])
        .trim_end_matches([']', ')'])
        .split([',', ' '])
        .filter_map(|x| x.trim().parse::<f32>().ok())
        .collect()
}

/// Converts numbers into a value of the global's naga type.
fn value_for(m: &naga::Module, ty: naga::Handle<naga::Type>, nums: &[f32]) -> Option<V> {
    let at = |i: usize| nums.get(i).or(nums.first()).copied().unwrap_or(0.0);
    Some(match &m.types[ty].inner {
        TypeInner::Scalar(s) => match s.kind {
            ScalarKind::Float | ScalarKind::AbstractFloat => V::F(at(0)),
            ScalarKind::Sint | ScalarKind::AbstractInt => V::I(at(0) as i32),
            ScalarKind::Uint => V::U(at(0) as u32),
            ScalarKind::Bool => V::B(at(0) != 0.0),
        },
        TypeInner::Vector { size, scalar } => {
            let n = *size as usize;
            let mut o = [0.0f32; 4];
            for (i, slot) in o.iter_mut().enumerate().take(n) {
                *slot = if nums.len() == 1 {
                    nums[0]
                } else {
                    nums.get(i)
                        .copied()
                        .unwrap_or(if i == 3 { 1.0 } else { 0.0 })
                };
            }
            match scalar.kind {
                ScalarKind::Float | ScalarKind::AbstractFloat => V::Fv(o, n as u8),
                ScalarKind::Sint | ScalarKind::AbstractInt => V::Iv(o.map(|x| x as i32), n as u8),
                ScalarKind::Uint => V::Uv(o.map(|x| x as u32), n as u8),
                ScalarKind::Bool => V::Bv(o.map(|x| x != 0.0), n as u8),
            }
        }
        TypeInner::Matrix { columns, rows, .. } => {
            let (c, r) = (*columns as usize, *rows as usize);
            let mut o = [0.0; 16];
            for col in 0..c {
                for row in 0..r {
                    o[col * 4 + row] = nums.get(col * r + row).copied().unwrap_or(0.0);
                }
            }
            V::M(o, c as u8, r as u8)
        }
        TypeInner::Array { base, size, .. } => {
            let n = match size {
                naga::ArraySize::Constant(n) => n.get() as usize,
                _ => 0,
            };
            let per = match &m.types[*base].inner {
                TypeInner::Vector { size, .. } => *size as usize,
                _ => 1,
            };
            V::A(
                (0..n)
                    .map(|i| {
                        let part: Vec<f32> = nums.iter().skip(i * per).take(per).copied().collect();
                        value_for(m, *base, &part).unwrap_or(V::Void)
                    })
                    .collect(),
            )
        }
        _ => return None,
    })
}

/// Bakes the display list `list` (local space) into a texture covering `region`.
fn raster_texture(
    ctx: &mut pen_raster::RasterContext,
    list: &DisplayList,
    region: &Rect,
    scale: f32,
    size: Vec2,
    wrap: Wrap,
    mipmap: bool,
) -> Texture {
    let pm = ctx.render(list, *region, scale, Color::TRANSPARENT);
    Texture::from_premul_rgba8(
        pm.width(),
        pm.height(),
        pm.data(),
        crate::texture::Map::Node {
            w: size.x,
            h: size.y,
            rx: region.x,
            ry: region.y,
            scale: pm.width() as f32 / region.w.max(1e-3),
            scale_y: pm.height() as f32 / region.h.max(1e-3),
        },
        wrap,
        mipmap,
    )
    .with_scale_hint(scale)
}

fn load_image(base: Option<&Path>, url: &str, mipmap: bool) -> Option<Texture> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return None;
    }
    let path = pen_layout::script::resolve_path(base, url);
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut px = img.into_raw();
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
    Some(Texture::from_premul_rgba8(
        w,
        h,
        &px,
        crate::texture::Map::Image,
        Wrap::Clamp,
        mipmap,
    ))
}

/// Evaluates a shader over `input.region`. Returns premultiplied RGBA8.
pub fn render(
    input: &RenderInput<'_>,
    raster: &mut pen_raster::RasterContext,
) -> Result<ImageData, String> {
    let c = input.compiled;
    let p = &c.program;
    let m = &p.module;
    let region = input.region;
    let scale = input.scale;
    let pw = ((region.w * scale).ceil() as u32).clamp(1, 8192);
    let ph = ((region.h * scale).ceil() as u32).clamp(1, 8192);
    let (sx, sy) = (
        pw as f32 / region.w.max(1e-3),
        ph as f32 / region.h.max(1e-3),
    );
    let mut textures: Vec<Texture> = Vec::new();
    let mut object_tex: Option<usize> = None;
    // Numbers bound to each uniform (samplers: their texture slot).
    let mut bound: Vec<Option<Vec<f32>>> = Vec::with_capacity(c.uniforms.len());
    for u in &c.uniforms {
        let given = input
            .uniforms
            .iter()
            .rev()
            .find(|(k, _)| **k == *u.name)
            .map(|(_, v)| v);
        let v = if u.ty == UType::Sampler2D {
            let src = c.sampler_source(u, input.uniforms);
            let tex = match src.as_deref() {
                Some("pencil:object") => {
                    if let Some(i) = object_tex {
                        Some(textures[i].clone())
                    } else {
                        input.object.map(|l| {
                            raster_texture(
                                raster,
                                l,
                                &region,
                                scale,
                                input.size,
                                Wrap::Transparent,
                                u.mipmap,
                            )
                        })
                    }
                }
                Some("pencil:background") => input.backdrop.map(|l| {
                    raster_texture(
                        raster,
                        l,
                        &region,
                        scale,
                        input.size,
                        Wrap::Mirror,
                        u.mipmap,
                    )
                }),
                Some("pencil:sdf") => {
                    // Fill: the node shape; effect: the content's alpha.
                    let mask = match (input.shape, input.object) {
                        (Some(shape), _) => {
                            let list = shape_list(shape, &region);
                            Some(raster.render(&list, region, scale, Color::TRANSPARENT))
                        }
                        (None, Some(obj)) => {
                            Some(raster.render(obj, region, scale, Color::TRANSPARENT))
                        }
                        _ => None,
                    };
                    mask.map(|pm| sdf_texture(&pm, region, input.size, scale))
                }
                Some(url) if !url.is_empty() => load_image(input.base_dir, url, u.mipmap),
                _ => None,
            };
            match tex {
                Some(t) => {
                    textures.push(t);
                    if src.as_deref() == Some("pencil:object") {
                        object_tex = Some(textures.len() - 1);
                    }
                    Some(vec![textures.len() as f32 - 1.0])
                }
                None => Some(vec![-1.0]),
            }
        } else {
            let nums: Vec<f32> = match u.role {
                Role::Resolution => vec![input.size.x, input.size.y],
                Role::Time => vec![0.0],
                Role::Mouse => match given {
                    Some(v) => numbers(v),
                    None => match &u.default {
                        Some(d) => text_numbers(d),
                        None => vec![input.size.x * 0.5, input.size.y * 0.5],
                    },
                },
                _ => match given {
                    Some(v) => numbers(v),
                    None => u.default.as_deref().map(text_numbers).unwrap_or_default(),
                },
            };
            (!nums.is_empty()).then_some(nums)
        };
        bound.push(v);
    }

    if input.gpu
        && let Ok((g, prog)) = c.gpu_program()
    {
        let (slots, n_vals) = gpu::layout_vals(&c.uniforms);
        let mut vals = vec![[0.0f32; 4]; n_vals];
        for ((u, slot), nums) in c.uniforms.iter().zip(&slots).zip(&bound) {
            let (Some((base, count)), Some(nums)) = (slot, nums) else {
                continue;
            };
            let per = match u.ty {
                UType::Vec(n) | UType::IVec(n) | UType::BVec(n) | UType::Mat(n) => n as usize,
                _ => 1,
            };
            let splat = nums.len() == 1;
            for k in 0..*count {
                for (lane, slot) in vals[base + k].iter_mut().enumerate().take(per) {
                    let i = k * per + lane;
                    let x = if splat {
                        nums[0]
                    } else {
                        nums.get(i)
                            .copied()
                            .unwrap_or(if lane == 3 { 1.0 } else { 0.0 })
                    };
                    *slot = x;
                }
            }
        }
        let view = [region.x, region.y, 1.0 / sx, 1.0 / sy];
        if let Ok(px) = g.render(
            &prog,
            view,
            [input.size.x, input.size.y],
            &vals,
            &textures,
            pw,
            ph,
        ) {
            return Ok(ImageData {
                id: IMAGE_IDS.fetch_add(1, Ordering::Relaxed),
                width: pw,
                height: ph,
                rgba_premul: px,
            });
        }
    }

    let mut init = p.globals_init().to_vec();
    for (u, nums) in c.uniforms.iter().zip(&bound) {
        let (Some((gi, ty)), Some(nums)) = (p.global(&u.name), nums) else {
            continue;
        };
        if let Some(v) = value_for(m, ty, nums) {
            init[gi as usize] = v;
        }
    }

    let threads = match input.threads {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        n => n,
    }
    .min(ph as usize)
    .max(1);
    let tex = TexSet { list: &textures };
    let mut out = vec![0u8; (pw * ph * 4) as usize];
    let rows_per = (ph as usize).div_ceil(threads);
    let error: Mutex<Option<EvalError>> = Mutex::new(None);
    let init = &init;
    let tex = &tex;
    std::thread::scope(|s| {
        for (band, chunk) in out.chunks_mut(rows_per * pw as usize * 4).enumerate() {
            let error = &error;
            s.spawn(move || {
                let mut ex = Exec::with_globals(p, tex, init);
                let y0 = band * rows_per;
                for (r, row) in chunk.chunks_mut(pw as usize * 4).enumerate() {
                    let py = (y0 + r) as f32 + 0.5;
                    // Node-local y (down) -> GL y (up from the node's bottom edge).
                    let ly = region.y + py / sy;
                    let gy = input.size.y - ly;
                    for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let gx = region.x + (x as f32 + 0.5) / sx;
                        match ex.run([gx, gy, 0.0, 1.0]) {
                            Ok(Some(c)) => {
                                let a = c[3].clamp(0.0, 1.0);
                                let q = |v: f32| (v.clamp(0.0, a) * 255.0 + 0.5) as u8;
                                px.copy_from_slice(&[q(c[0]), q(c[1]), q(c[2]), q(a)]);
                            }
                            Ok(None) => px.copy_from_slice(&[0; 4]),
                            Err(e) => {
                                *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                                return;
                            }
                        }
                    }
                    if error.lock().map(|e| e.is_some()).unwrap_or(true) {
                        return;
                    }
                }
            });
        }
    });
    if let Some(e) = error.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(match e {
            EvalError::TooLong => "shader loop ran too long".into(),
            EvalError::Unsupported(m) => format!("unsupported: {m}"),
        });
    }
    Ok(ImageData {
        id: IMAGE_IDS.fetch_add(1, Ordering::Relaxed),
        width: pw,
        height: ph,
        rgba_premul: out,
    })
}

/// A display list with `shape` filled opaque white (local space).
fn shape_list(shape: &Shape, region: &Rect) -> DisplayList {
    DisplayList {
        chunks: vec![Arc::new(Chunk {
            root: pen_render::NodeKey(0),
            bounds: *region,
            version: 0,
            items: vec![DisplayItem::Shape {
                node: pen_render::NodeKey(0),
                transform: Affine2::IDENTITY,
                shape: shape.clone(),
                fills: smallvec![Paint::Solid(Color::WHITE)],
                stroke: None,
                shadows: smallvec![],
            }],
        })],
    }
}

// --------------------------------------------------------------------- engine

type Prog = Arc<Result<Compiled, String>>;

struct ProgEntry {
    modified: Option<SystemTime>,
    len: u64,
    prog: Prog,
}

enum Entry {
    Pending,
    Ready(Arc<ImageData>),
    Failed(Arc<str>),
}

struct Inner {
    limits: ShaderLimits,
    deferred: bool,
    programs: Mutex<HashMap<PathBuf, ProgEntry>>,
    cache: Mutex<(HashMap<u64, Entry>, VecDeque<u64>)>,
    ready: AtomicBool,
    queue: Mutex<Option<Sender<ShaderRequest>>>,
    raster: Mutex<pen_raster::RasterContext>,
    stats: Mutex<ShaderStats>,
}

/// Counters (for tests, benches and the docs).
#[derive(Debug, Default, Clone)]
pub struct ShaderStats {
    pub renders: u64,
    pub pixels: u64,
    pub time: Duration,
    /// Last errors (`url: message`).
    pub errors: Vec<String>,
}

/// Max cached results.
const CACHE_CAP: usize = 256;

/// Shader engine plugged into the display-list builder (see the crate docs).
/// Cloning shares the caches.
#[derive(Clone)]
pub struct ShaderEngine {
    inner: Arc<Inner>,
}

impl ShaderEngine {
    /// Renders synchronously when asked (CLI, export, tests).
    pub fn new() -> Self {
        Self::with(ShaderLimits::default(), false)
    }

    /// Renders on a background thread; results arrive through `take_ready` (app).
    pub fn deferred() -> Self {
        Self::with(ShaderLimits::default(), true)
    }

    pub fn with(limits: ShaderLimits, deferred: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                limits,
                deferred,
                programs: Mutex::new(HashMap::new()),
                cache: Mutex::new((HashMap::new(), VecDeque::new())),
                ready: AtomicBool::new(false),
                queue: Mutex::new(None),
                raster: Mutex::new(pen_raster::RasterContext::new()),
                stats: Mutex::new(ShaderStats::default()),
            }),
        }
    }

    pub fn stats(&self) -> ShaderStats {
        self.inner
            .stats
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Forgets compiled shaders and rendered results (files changed on disk).
    pub fn clear(&self) {
        if let Ok(mut p) = self.inner.programs.lock() {
            p.clear();
        }
        if let Ok(mut c) = self.inner.cache.lock() {
            c.0.clear();
            c.1.clear();
        }
    }

    /// The compiled shader at `url` (re-read when the file changes).
    pub fn program(&self, url: &str, base: Option<&Path>) -> Prog {
        self.inner.program(url, base)
    }
}

impl Default for ShaderEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    fn program(&self, url: &str, base: Option<&Path>) -> Prog {
        let path = pen_layout::script::resolve_path(base, url);
        let meta = std::fs::metadata(&path).ok();
        let (modified, len) = (
            meta.as_ref().and_then(|m| m.modified().ok()),
            meta.as_ref().map_or(0, |m| m.len()),
        );
        let mut progs = self.programs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = progs.get(&path)
            && e.modified == modified
            && e.len == len
        {
            return e.prog.clone();
        }
        let prog: Prog = Arc::new(match std::fs::read_to_string(&path) {
            Ok(src) => Compiled::new(&src),
            Err(_) => Err(format!("shader not found: {}", path.display())),
        });
        progs.insert(
            path,
            ProgEntry {
                modified,
                len,
                prog: prog.clone(),
            },
        );
        prog
    }

    fn run(&self, req: &ShaderRequest) -> Entry {
        let prog = self.program(&req.url, req.base_dir.as_deref());
        let compiled = match &*prog {
            Ok(c) => c,
            Err(e) => return self.fail(req, e),
        };
        let use_gpu = self.limits.gpu && compiled.gpu_program().is_ok();
        let budget = if use_gpu {
            self.limits.gpu_max_pixels
        } else {
            self.limits.cpu_max_pixels
        };
        let scale = pick_scale(&req.region, budget, self.limits.max_scale);
        let t0 = Instant::now();
        let mut raster = self.raster.lock().unwrap_or_else(|e| e.into_inner());
        let r = render(
            &RenderInput {
                compiled,
                uniforms: &req.uniforms,
                size: req.size,
                region: req.region,
                scale,
                base_dir: req.base_dir.as_deref(),
                shape: req.shape.as_ref(),
                object: req.object.as_deref(),
                backdrop: req.backdrop.as_deref(),
                threads: self.limits.threads,
                gpu: use_gpu,
            },
            &mut raster,
        );
        drop(raster);
        match r {
            Ok(img) => {
                if let Ok(mut s) = self.stats.lock() {
                    s.renders += 1;
                    s.pixels += img.width as u64 * img.height as u64;
                    s.time += t0.elapsed();
                }
                Entry::Ready(Arc::new(img))
            }
            Err(e) => self.fail(req, &e),
        }
    }

    fn fail(&self, req: &ShaderRequest, e: &str) -> Entry {
        if let Ok(mut s) = self.stats.lock() {
            let msg = format!("{}: {e}", req.url);
            if !s.errors.contains(&msg) {
                s.errors.push(msg);
                if s.errors.len() > 32 {
                    s.errors.remove(0);
                }
            }
        }
        Entry::Failed(e.into())
    }

    fn store(&self, key: u64, e: Entry) {
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.0.insert(key, e).is_none() {
            c.1.push_back(key);
        }
        while c.1.len() > CACHE_CAP {
            if let Some(old) = c.1.pop_front() {
                c.0.remove(&old);
            }
        }
    }
}

fn lookup(e: &Entry) -> ShaderLookup {
    match e {
        Entry::Pending => ShaderLookup::Pending,
        Entry::Ready(i) => ShaderLookup::Ready(i.clone()),
        Entry::Failed(m) => ShaderLookup::Failed(m.clone()),
    }
}

impl ShaderRenderer for ShaderEngine {
    fn needs(&self, url: &str, base_dir: Option<&Path>) -> ShaderNeeds {
        match &*self.inner.program(url, base_dir) {
            Ok(c) => c.needs(&[]),
            Err(_) => ShaderNeeds::default(),
        }
    }

    fn render(&self, req: &ShaderRequest) -> ShaderLookup {
        // The key covers the request; the file identity is added here.
        let prog = self.inner.program(&req.url, req.base_dir.as_deref());
        let mut h = DefaultHasher::new();
        req.key.hash(&mut h);
        (Arc::as_ptr(&prog) as usize).hash(&mut h);
        let key = h.finish();
        if let Some(e) = self
            .inner
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .get(&key)
        {
            return lookup(e);
        }
        if !self.inner.deferred {
            let e = self.inner.run(req);
            let l = lookup(&e);
            self.inner.store(key, e);
            return l;
        }
        self.inner.store(key, Entry::Pending);
        let mut q = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.is_none() {
            let (tx, rx) = channel::<ShaderRequest>();
            let inner = self.inner.clone();
            let spawned = std::thread::Builder::new()
                .name("pen-shader".into())
                .spawn(move || {
                    while let Ok(req) = rx.recv() {
                        let prog = inner.program(&req.url, req.base_dir.as_deref());
                        let mut h = DefaultHasher::new();
                        req.key.hash(&mut h);
                        (Arc::as_ptr(&prog) as usize).hash(&mut h);
                        let key = h.finish();
                        let e = inner.run(&req);
                        inner.store(key, e);
                        inner.ready.store(true, Ordering::Release);
                    }
                });
            if spawned.is_ok() {
                *q = Some(tx);
            }
        }
        match q.as_ref() {
            Some(tx) if tx.send(req.clone()).is_ok() => ShaderLookup::Pending,
            _ => {
                drop(q);
                let e = self.inner.run(req);
                let l = lookup(&e);
                self.inner.store(key, e);
                l
            }
        }
    }

    fn take_ready(&self) -> bool {
        self.inner.ready.swap(false, Ordering::AcqRel)
    }
}
