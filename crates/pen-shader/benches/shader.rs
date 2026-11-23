//! Shader rendering benchmarks: one fill over a 300x200 node at 1 px per unit
//! (60k pixels), on the GPU (when an adapter exists) and on the CPU evaluator.
//! `noise` is an fbm of value noise (5 octaves, typical of the shader library).

use criterion::{Criterion, criterion_group, criterion_main};
use pen_render::{Rect, Vec2};
use pen_shader::{Compiled, RenderInput};

const NOISE: &str = r#"/** @resolution */
uniform vec2 u_resolution;
/** @default 6 */
uniform float u_scale;
float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float vnoise(vec2 p) {
  vec2 i = floor(p); vec2 f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
             mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) {
  float s = 0.0; float a = 0.5;
  for (int i = 0; i < 5; i++) { s += a * vnoise(p); p *= 2.0; a *= 0.5; }
  return s;
}
void main() {
  vec2 uv = gl_FragCoord.xy / u_resolution;
  float n = fbm(uv * u_scale);
  gl_FragColor = vec4(mix(vec3(0.1, 0.2, 0.5), vec3(1.0, 0.8, 0.4), n), 1.0);
}
"#;

fn run(c: &Compiled, gpu: bool, raster: &mut pen_raster::RasterContext) {
    let r = pen_shader::render(
        &RenderInput {
            compiled: c,
            uniforms: &[],
            size: Vec2::new(300.0, 200.0),
            region: Rect::new(0.0, 0.0, 300.0, 200.0),
            scale: 1.0,
            base_dir: None,
            shape: None,
            object: None,
            backdrop: None,
            threads: 0,
            gpu,
        },
        raster,
    );
    assert!(r.is_ok());
}

fn benches(cr: &mut Criterion) {
    let c = Compiled::new(NOISE).unwrap();
    let mut raster = pen_raster::RasterContext::new();
    cr.bench_function("compile/noise", |b| {
        b.iter(|| Compiled::new(NOISE).unwrap())
    });
    if c.gpu_program().is_ok() {
        cr.bench_function("render/noise_300x200_gpu", |b| {
            b.iter(|| run(&c, true, &mut raster))
        });
    }
    let mut g = cr.benchmark_group("cpu");
    g.sample_size(10);
    g.bench_function("render/noise_300x200_cpu", |b| {
        b.iter(|| run(&c, false, &mut raster))
    });
    g.finish();
}

criterion_group!(group, benches);
criterion_main!(group);
