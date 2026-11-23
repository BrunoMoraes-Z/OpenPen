//! GPU backend: the rewritten shader runs as a real fragment shader on a headless wgpu
//! device (DX12 / Vulkan / Metal) and the result is read back.
//!
//! GPUI 0.3 has no custom-pipeline hook, so shaders cannot run inside the canvas'
//! renderer; an offscreen device renders them into images that every backend then
//! draws like image fills. The GLSL that naga already parsed for the CPU evaluator is
//! assembled differently here:
//!
//! - one uniform block `_PenU` with the view mapping, the node size, per-texture-slot
//!   mapping/info and every user uniform packed in `vec4 _pen_vals[]` (one vec4 per
//!   scalar/vector/array element/matrix column, no std140 bookkeeping);
//! - `main` is renamed and wrapped: the wrapper unpacks the uniforms into the private
//!   globals the user code reads, computes `gl_FragCoord` in node units (GL origin at
//!   the node's bottom-left) and writes `gl_FragColor` to the color target;
//! - texture slots are real `texture2D`s; `_pen_tex*` map the slot's uv like the CPU
//!   [`crate::texture::Texture`] (images flipped, node textures over their region,
//!   transparent/clamp/mirror wrap) and sample with explicit LOD.
//!
//! Any failure (no adapter, naga/wgpu validation) makes the engine fall back to the CPU
//! evaluator.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use crate::glsl::{Rewritten, UType, Uniform};
use crate::texture::{Texture, Wrap};

/// Texture slots per shader.
pub const SLOTS: usize = 8;

/// Where each uniform lives in `_pen_vals` (`None`: not packed, e.g. structs).
pub fn layout_vals(uniforms: &[Uniform]) -> (Vec<Option<(usize, usize)>>, usize) {
    let mut at = 0;
    let out = uniforms
        .iter()
        .map(|u| {
            let per = match u.ty {
                UType::Mat(n) => n as usize,
                UType::Other(_) => return None,
                _ => 1,
            };
            let count = per * u.array.unwrap_or(1).max(1) as usize;
            let r = (at, count);
            at += count;
            Some(r)
        })
        .collect();
    (out, at.max(1))
}

fn unpack(u: &Uniform, base: usize) -> String {
    let swz = |n: u8| match n {
        2 => "xy",
        3 => "xyz",
        _ => "xyzw",
    };
    let one = |k: usize| -> String {
        let v = format!("_pen_vals[{k}]");
        match &u.ty {
            UType::Float => format!("{v}.x"),
            UType::Int | UType::Sampler2D => format!("int({v}.x)"),
            UType::Bool => format!("({v}.x != 0.0)"),
            UType::Vec(n) => format!("{v}.{}", swz(*n)),
            UType::IVec(n) => format!("ivec{n}({v}.{})", swz(*n)),
            UType::BVec(n) => format!("notEqual({v}.{}, vec{n}(0.0))", swz(*n)),
            UType::Mat(n) => {
                let cols: Vec<String> = (0..*n as usize)
                    .map(|c| format!("_pen_vals[{}].{}", k + c, swz(*n)))
                    .collect();
                format!("mat{n}({})", cols.join(", "))
            }
            UType::Other(_) => String::new(),
        }
    };
    let per = match u.ty {
        UType::Mat(n) => n as usize,
        _ => 1,
    };
    match u.array {
        Some(len) => (0..len as usize)
            .map(|i| format!("{}[{i}] = {};\n", u.name, one(base + i * per)))
            .collect(),
        None => format!("{} = {};\n", u.name, one(base)),
    }
}

/// Fragment shader source for the GPU (see the module docs).
pub fn gpu_source(rw: &Rewritten) -> String {
    let (slots, n_vals) = layout_vals(&rw.uniforms);
    let mut s = String::with_capacity(rw.gpu_body.len() + 4096);
    s.push_str("#version 450\n");
    s.push_str(&format!(
        "layout(set = 0, binding = 0) uniform _PenU {{ vec4 _pen_view; vec4 _pen_size; \
         vec4 _pen_map[{SLOTS}]; vec4 _pen_info[{SLOTS}]; vec4 _pen_vals[{n_vals}]; }};\n"
    ));
    s.push_str("layout(set = 0, binding = 1) uniform sampler _pen_s_clamp;\n");
    s.push_str("layout(set = 0, binding = 2) uniform sampler _pen_s_mirror;\n");
    for i in 0..SLOTS {
        s.push_str(&format!(
            "layout(set = 0, binding = {}) uniform texture2D _pen_t{i};\n",
            3 + i
        ));
    }
    s.push_str("layout(location = 0) out vec4 _pen_out;\n");
    s.push_str("vec4 _pen_frag_coord; vec4 _pen_frag_color;\n");
    s.push_str("vec4 _pen_fetch(int s, vec2 t, float lod) {\n");
    for i in 0..SLOTS {
        s.push_str(&format!(
            "  if (s == {i}) {{ if (_pen_info[{i}].z > 1.5) return textureLod(sampler2D(_pen_t{i}, _pen_s_mirror), t, lod); \
             return textureLod(sampler2D(_pen_t{i}, _pen_s_clamp), t, lod); }}\n"
        ));
    }
    s.push_str("  return vec4(0.0);\n}\n");
    s.push_str(&format!(
        "vec4 _pen_tex_lod(int s, vec2 p, float lod) {{\n\
         if (s < 0 || s >= {SLOTS}) return vec4(0.0);\n\
         vec4 info = _pen_info[s];\n\
         if (info.w < 0.5) return vec4(0.0);\n\
         vec4 m = _pen_map[s];\n\
         vec2 t = vec2(p.x * m.x + m.y, p.y * m.z + m.w);\n\
         if (info.z > 0.5 && info.z < 1.5 && (t.x < 0.0 || t.y < 0.0 || t.x > 1.0 || t.y > 1.0)) return vec4(0.0);\n\
         return _pen_fetch(s, t, max(lod, 0.0));\n}}\n\
         vec4 _pen_tex(int s, vec2 p) {{ return _pen_tex_lod(s, p, 0.0); }}\n\
         vec4 _pen_tex(int s, vec2 p, float bias) {{ return _pen_tex_lod(s, p, bias); }}\n\
         ivec2 _pen_tex_size(int s, int lod) {{ if (s < 0 || s >= {SLOTS}) return ivec2(1); return ivec2(_pen_info[s].xy); }}\n"
    ));
    s.push_str(&rw.gpu_body);
    s.push_str("\nvoid main() {\n");
    for (u, slot) in rw.uniforms.iter().zip(&slots) {
        if let Some((base, _)) = slot {
            s.push_str(&unpack(u, *base));
        }
    }
    s.push_str(
        "_pen_frag_coord = vec4(_pen_view.x + gl_FragCoord.x * _pen_view.z, \
         _pen_size.y - (_pen_view.y + gl_FragCoord.y * _pen_view.w), 0.0, 1.0);\n\
         _pen_frag_color = vec4(0.0);\n_pen_user_main();\n_pen_out = _pen_frag_color;\n}\n",
    );
    s
}

const VERTEX: &str = "#version 450\n\
void main() {\n\
  vec2 p = vec2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2));\n\
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);\n\
}\n";

/// The process-wide headless device.
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    bgl: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    vs: wgpu::ShaderModule,
    clamp: wgpu::Sampler,
    mirror: wgpu::Sampler,
    dummy: wgpu::TextureView,
    max_side: u32,
    pub adapter: String,
}

/// A compiled GPU pipeline for one shader.
pub struct GpuProgram {
    pipeline: wgpu::RenderPipeline,
    n_vals: usize,
}

/// The shared device (`None` when there is no adapter, or with `PEN_SHADER_GPU=0`).
pub fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        if std::env::var("PEN_SHADER_GPU").is_ok_and(|v| v == "0") {
            return None;
        }
        init()
    })
    .as_ref()
}

fn init() -> Option<Gpu> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        ..Default::default()
    }))
    .ok()?;
    let info = adapter.get_info();
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("pen-shader"),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .ok()?;
    // Validation errors are caught with error scopes; anything else must not panic.
    device.on_uncaptured_error(Arc::new(|e| {
        eprintln!("pen-shader: wgpu error: {e}");
    }));
    let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let mut entries = vec![
        wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
    ];
    entries.extend((0..SLOTS as u32).map(|i| tex_entry(3 + i)));
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pen-shader"),
        entries: &entries,
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pen-shader"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let vs_module = naga::front::glsl::Frontend::default()
        .parse(
            &naga::front::glsl::Options::from(naga::ShaderStage::Vertex),
            VERTEX,
        )
        .ok()?;
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pen-shader-vs"),
        source: wgpu::ShaderSource::Naga(Cow::Owned(vs_module)),
    });
    let sampler = |mode: wgpu::AddressMode| {
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pen-shader"),
            address_mode_u: mode,
            address_mode_v: mode,
            address_mode_w: mode,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        })
    };
    let clamp = sampler(wgpu::AddressMode::ClampToEdge);
    let mirror = sampler(wgpu::AddressMode::MirrorRepeat);
    let dummy = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("pen-shader-dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default());
    Some(Gpu {
        device,
        queue,
        bgl,
        layout,
        vs,
        clamp,
        mirror,
        dummy,
        max_side: limits.max_texture_dimension_2d,
        adapter: format!("{} ({:?})", info.name, info.backend),
    })
}

impl Gpu {
    /// Builds the pipeline of a shader (naga parse of the GPU source + wgpu validation).
    pub fn program(&self, rw: &Rewritten) -> Result<GpuProgram, String> {
        let src = gpu_source(rw);
        let module = naga::front::glsl::Frontend::default()
            .parse(
                &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
                &src,
            )
            .map_err(|e| {
                e.errors
                    .first()
                    .map_or_else(|| "GLSL error".to_string(), |er| er.kind.to_string())
            })?;
        // Validate with naga first: wgpu would only report it through the error scope.
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .map_err(|e| format!("validation: {}", e.as_inner()))?;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let fs = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("pen-shader-fs"),
                source: wgpu::ShaderSource::Naga(Cow::Owned(module)),
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("pen-shader"),
                layout: Some(&self.layout),
                vertex: wgpu::VertexState {
                    module: &self.vs,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &fs,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(e.to_string());
        }
        let (_, n_vals) = layout_vals(&rw.uniforms);
        Ok(GpuProgram { pipeline, n_vals })
    }

    fn upload(&self, t: &Texture) -> wgpu::TextureView {
        let levels: Vec<_> = t.levels().collect();
        let (w, h) = (levels[0].0, levels[0].1);
        let hdr = t.is_hdr();
        let format = if hdr {
            wgpu::TextureFormat::Rgba16Float
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pen-shader-tex"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (mip, (lw, lh, px)) in levels.iter().enumerate() {
            let (bytes, bpp): (Vec<u8>, u32) = if hdr {
                (
                    px.iter()
                        .flat_map(|p| p.map(|c| half::f16::from_f32(c).to_bits()))
                        .flat_map(u16::to_le_bytes)
                        .collect(),
                    8,
                )
            } else {
                (
                    px.iter()
                        .flat_map(|p| p.map(|c| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8))
                        .collect(),
                    4,
                )
            };
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: mip as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(lw * bpp),
                    rows_per_image: Some(*lh),
                },
                wgpu::Extent3d {
                    width: *lw,
                    height: *lh,
                    depth_or_array_layers: 1,
                },
            );
        }
        tex.create_view(&Default::default())
    }

    /// Renders `pw` x `ph` pixels. `view` = (region x, region y, units per pixel x, y);
    /// `vals` = packed uniform values. Returns premultiplied RGBA8.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        prog: &GpuProgram,
        view: [f32; 4],
        size: [f32; 2],
        vals: &[[f32; 4]],
        textures: &[Texture],
        pw: u32,
        ph: u32,
    ) -> Result<Vec<u8>, String> {
        if pw > self.max_side || ph > self.max_side {
            return Err("output too large for the GPU".into());
        }
        if textures.len() > SLOTS {
            return Err(format!("more than {SLOTS} textures"));
        }
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut ubo: Vec<f32> = Vec::with_capacity(8 + SLOTS * 8 + prog.n_vals * 4);
        ubo.extend_from_slice(&view);
        ubo.extend_from_slice(&[size[0], size[1], 0.0, 0.0]);
        for i in 0..SLOTS {
            ubo.extend_from_slice(&textures.get(i).map_or([0.0; 4], Texture::gpu_map));
        }
        for i in 0..SLOTS {
            ubo.extend_from_slice(&textures.get(i).map_or([1.0, 1.0, 0.0, 0.0], |t| {
                let s = t.size();
                let wrap = match t.wrap() {
                    Wrap::Clamp => 0.0,
                    Wrap::Transparent => 1.0,
                    Wrap::Mirror => 2.0,
                };
                [s[0] as f32, s[1] as f32, wrap, 1.0]
            }));
        }
        for i in 0..prog.n_vals {
            ubo.extend_from_slice(&vals.get(i).copied().unwrap_or([0.0; 4]));
        }
        let bytes: Vec<u8> = ubo.iter().flat_map(|f| f.to_le_bytes()).collect();
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pen-shader-ubo"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buf, 0, &bytes);
        let views: Vec<wgpu::TextureView> = textures.iter().map(|t| self.upload(t)).collect();
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&self.clamp),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&self.mirror),
            },
        ];
        for i in 0..SLOTS {
            entries.push(wgpu::BindGroupEntry {
                binding: 3 + i as u32,
                resource: wgpu::BindingResource::TextureView(views.get(i).unwrap_or(&self.dummy)),
            });
        }
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pen-shader"),
            layout: &self.bgl,
            entries: &entries,
        });
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pen-shader-target"),
            size: wgpu::Extent3d {
                width: pw,
                height: ph,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let tview = target.create_view(&Default::default());
        let row = (pw * 4).div_ceil(256) * 256;
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pen-shader-readback"),
            size: (row * ph) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pen-shader"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pen-shader"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tview,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&prog.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &out,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(ph),
                },
            },
            wgpu::Extent3d {
                width: pw,
                height: ph,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([enc.finish()]);
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(e.to_string());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        out.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let data = out.get_mapped_range(..).map_err(|e| format!("{e:?}"))?;
        let mut px = Vec::with_capacity((pw * ph * 4) as usize);
        for y in 0..ph as usize {
            let r = &data[y * row as usize..y * row as usize + pw as usize * 4];
            px.extend_from_slice(r);
        }
        drop(data);
        out.unmap();
        // Premultiplied output: color cannot exceed alpha.
        for p in px.as_chunks_mut::<4>().0 {
            let a = p[3];
            p[0] = p[0].min(a);
            p[1] = p[1].min(a);
            p[2] = p[2].min(a);
        }
        Ok(px)
    }
}
