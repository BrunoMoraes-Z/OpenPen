//! One frame of an animated-shader scene in the editor: a 1200x800 page with 4 fbm fills
//! of 300x200 that use `@time` (rendered at 2 px per unit = 240k px each) among 400 plain
//! rectangles, on the deferred engine (GPU when an adapter exists, like the app).
//!
//! - `anim/frame_build`: advancing `@time` and rebuilding the page chunk (the UI-thread
//!   cost of a tick; the renders are queued).
//! - `anim/frame_full`: the whole frame: build, wait for the 4 renders, swap them in.

use std::fmt::Write;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use pen_doc::SceneDoc;
use pen_layout::{NoAssets, Pipeline, ShaderAnim, TextEngine};
use pen_shader::ShaderEngine;

const NOISE_T: &str = r#"/** @resolution */
uniform vec2 u_resolution;
/** @time */
uniform float u_time;
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
  float n = fbm(uv * 6.0 + vec2(u_time * 0.5, u_time * 0.2));
  gl_FragColor = vec4(mix(vec3(0.1, 0.2, 0.5), vec3(1.0, 0.8, 0.4), n), 1.0);
}
"#;

fn scene(dir: &std::path::Path) -> String {
    std::fs::write(dir.join("noise-t.glsl"), NOISE_T).unwrap();
    let mut kids = String::new();
    for i in 0..4 {
        let _ = write!(
            kids,
            r#"{{ "type": "rectangle", "id": "s{i}", "x": {}, "y": 20, "width": 300, "height": 200,
               "fill": {{ "type": "shader", "url": "noise-t.glsl" }} }},"#,
            20 + i * 300
        );
    }
    for i in 0..400 {
        let _ = write!(
            kids,
            r##"{{ "type": "rectangle", "x": {}, "y": {}, "width": 40, "height": 20, "cornerRadius": 4, "fill": "#88aacc" }},"##,
            (i % 20) * 55 + 20,
            (i / 20) * 26 + 260
        );
    }
    kids.pop();
    format!(
        r##"{{ "version": "2.22", "children": [ {{ "type": "frame", "id": "page", "x": 0, "y": 0,
            "width": 1240, "height": 800, "fill": "#ffffff", "layout": "none", "children": [ {kids} ] }} ] }}"##
    )
}

fn benches(cr: &mut Criterion) {
    let dir = std::env::temp_dir().join("pen-shader-anim-bench");
    let _ = std::fs::create_dir_all(&dir);
    let doc = scene(&dir);
    let (mut scene, _) =
        SceneDoc::load(doc.as_bytes(), Some(&dir), &pen_doc::NoLibraries).expect("load");
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.set_shader_renderer(Arc::new(ShaderEngine::deferred()));
    pipe.rebuild(&mut scene);
    let keys: Vec<_> = (0..4)
        .map(|i| scene.find(&format!("s{i}")).unwrap())
        .collect();
    let wait = |pipe: &Pipeline| {
        while pipe.shaders_busy() {
            std::hint::spin_loop();
        }
    };
    let mut t = 0.0f32;
    let mut frame = |pipe: &mut Pipeline, full: bool| {
        t += 1.0 / 60.0;
        let frames: Vec<_> = keys
            .iter()
            .map(|&k| {
                (
                    k,
                    ShaderAnim {
                        time: t,
                        mouse: None,
                    },
                )
            })
            .collect();
        let n = pipe.animate_shaders(&scene, &frames).len();
        if full {
            wait(pipe);
            pipe.animate_shaders(&scene, &[]);
        }
        n
    };
    // Warm up: pipeline compile, first frames.
    for _ in 0..5 {
        frame(&mut pipe, true);
    }
    cr.bench_function("anim/frame_full", |b| b.iter(|| frame(&mut pipe, true)));
    cr.bench_function("anim/frame_build", |b| {
        b.iter_custom(|iters| {
            let mut total = std::time::Duration::ZERO;
            for _ in 0..iters {
                let t0 = std::time::Instant::now();
                std::hint::black_box(frame(&mut pipe, false));
                total += t0.elapsed();
                // The renders run on the worker while the UI thread is idle (not timed).
                wait(&pipe);
            }
            total
        })
    });
}

criterion_group!(group, benches);
criterion_main!(group);
