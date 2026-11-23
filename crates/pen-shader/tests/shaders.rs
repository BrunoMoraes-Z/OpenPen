//! Shader fills and effects end to end (scene -> display list -> raster), on the CPU
//! evaluator (deterministic goldens) and, when an adapter exists, on the GPU.
//!
//! Shader files are our own, written under `target/pen-shader-tests/` at test time. The
//! legacy shader library (proprietary, never committed) is used only when present.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pen_doc::SceneDoc;
use pen_layout::{NoAssets, Pipeline, TextEngine};
use pen_render::{Color, Rect};
use pen_shader::{ShaderEngine, ShaderLimits};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace")
        .to_path_buf()
}

fn dir(name: &str) -> PathBuf {
    let d = workspace().join("target/pen-shader-tests").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const CHECKER: &str = r#"/** @resolution */
uniform vec2 u_resolution;
/**
 * @label Size
 * @default 20
 */
uniform float u_size;
/**
 * @color
 * @default #ff0000
 */
uniform vec3 u_a;
/**
 * @color
 * @default #0000ff
 */
uniform vec3 u_b;
void main() {
  vec2 cell = floor(gl_FragCoord.xy / u_size);
  float k = mod(cell.x + cell.y, 2.0);
  // Bottom rows (GL y up) get a green tint.
  float g = gl_FragCoord.y < u_resolution.y * 0.25 ? 1.0 : 0.0;
  gl_FragColor = vec4(mix(u_a, u_b, k) + vec3(0.0, g, 0.0), 1.0);
}
"#;

const PIXELATE: &str = r#"/** @resolution */
uniform vec2 u_resolution;
/** @label Content */
uniform sampler2D u_content;
/**
 * @default 16
 * @range 1, 64
 */
uniform float u_pixel;
void main() {
  vec2 cell = (floor(gl_FragCoord.xy / u_pixel) + 0.5) * u_pixel;
  gl_FragColor = texture2D(u_content, cell / u_resolution);
}
"#;

const INVERT_BG: &str = r#"/** @resolution */
uniform vec2 u_resolution;
/** @default pencil:background */
uniform sampler2D u_bg;
void main() {
  vec4 c = texture2D(u_bg, gl_FragCoord.xy / u_resolution);
  gl_FragColor = vec4(vec3(1.0) - c.rgb, 1.0);
}
"#;

const SDF_RING: &str = r#"/** @resolution */
uniform vec2 u_resolution;
/** @sdf */
uniform sampler2D u_sdf;
void main() {
  float d = texture2D(u_sdf, gl_FragCoord.xy / u_resolution).r;
  // Inside: white core, an 8-unit dark border along the edge.
  vec3 c = d > 8.0 ? vec3(1.0) : vec3(0.1, 0.1, 0.3);
  gl_FragColor = d > 0.0 ? vec4(c, 1.0) : vec4(0.0);
}
"#;

fn write_shaders(d: &Path) {
    std::fs::write(d.join("checker.glsl"), CHECKER).unwrap();
    std::fs::write(d.join("pixelate.glsl"), PIXELATE).unwrap();
    std::fs::write(d.join("invert-bg.glsl"), INVERT_BG).unwrap();
    std::fs::write(d.join("sdf-ring.glsl"), SDF_RING).unwrap();
    std::fs::write(
        d.join("broken.glsl"),
        "void main() { gl_FragColor = vec4(1.0) }\n",
    )
    .unwrap();
}

const DOC: &str = r##"{ "version": "2.22", "children": [
  { "type": "frame", "id": "page", "x": 0, "y": 0, "width": 400, "height": 240, "fill": "#ffffff",
    "layout": "none", "children": [
      { "type": "rectangle", "id": "checker", "x": 10, "y": 10, "width": 120, "height": 80,
        "fill": { "type": "shader", "url": "checker.glsl", "uniforms": { "u_b": "#00ffff" } } },
      { "type": "frame", "id": "pix", "x": 150, "y": 10, "width": 100, "height": 100, "layout": "none",
        "fill": "#ffeeaa",
        "effect": { "type": "shader", "url": "pixelate.glsl", "uniforms": { "u_content": "pencil:object", "u_pixel": 20 } },
        "children": [
          { "type": "ellipse", "x": 10, "y": 10, "width": 80, "height": 80, "fill": "#2244cc" } ] },
      { "type": "rectangle", "id": "stripe", "x": 270, "y": 10, "width": 30, "height": 100, "fill": "#000000" },
      { "type": "rectangle", "id": "inv", "x": 260, "y": 40, "width": 120, "height": 40,
        "fill": { "type": "shader", "url": "invert-bg.glsl" } },
      { "type": "ellipse", "id": "sdf", "x": 10, "y": 120, "width": 160, "height": 100,
        "fill": { "type": "shader", "url": "sdf-ring.glsl" } },
      { "type": "rectangle", "id": "broken", "x": 200, "y": 140, "width": 60, "height": 60,
        "fill": { "type": "shader", "url": "broken.glsl" } },
      { "type": "rectangle", "id": "missing", "x": 280, "y": 140, "width": 60, "height": 60,
        "fill": { "type": "shader", "url": "nope.glsl" } }
    ] }
] }"##;

fn build(d: &Path, engine: &ShaderEngine) -> (SceneDoc, Pipeline) {
    let (mut scene, _) =
        SceneDoc::load(DOC.as_bytes(), Some(d), &pen_doc::NoLibraries).expect("load");
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.set_shader_renderer(Arc::new(engine.clone()));
    pipe.rebuild(&mut scene);
    (scene, pipe)
}

fn render(pipe: &Pipeline) -> pen_raster::Pixmap {
    pen_raster::render(
        &pipe.list,
        Rect::new(0.0, 0.0, 400.0, 240.0),
        1.0,
        Color::WHITE,
    )
}

fn px(p: &pen_raster::Pixmap, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * p.width() + x) * 4) as usize;
    let d = p.data();
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    a.iter()
        .zip(b)
        .all(|(x, y)| (*x as i32 - y as i32).abs() <= tol)
}

fn check_scene(p: &pen_raster::Pixmap, tag: &str) {
    // Checker: cell (0,0) at the bottom-left (GL origin) is u_a = red + green tint.
    assert!(
        near(px(p, 15, 85), [255, 255, 0, 255], 8),
        "{tag} {:?}",
        px(p, 15, 85)
    );
    // Above the tint: (x 0..20, GL y 40..60 -> doc y 30..50 from the top+10).
    assert!(
        near(px(p, 15, 45), [255, 0, 0, 255], 8),
        "{tag} {:?}",
        px(p, 15, 45)
    );
    assert!(
        near(px(p, 35, 45), [0, 255, 255, 255], 8),
        "{tag} {:?}",
        px(p, 35, 45)
    );
    // Pixelate: every 20x20 block is uniform (sampled at the block center).
    let a = px(p, 151, 11);
    for (x, y) in [(158, 18), (165, 25), (169, 29)] {
        assert!(near(px(p, x, y), a, 6), "{tag} block not uniform");
    }
    // Center block is the ellipse color.
    assert!(
        near(px(p, 200, 60), [0x22, 0x44, 0xcc, 255], 10),
        "{tag} {:?}",
        px(p, 200, 60)
    );
    // Inverted background: over the black stripe -> white; over the page -> black.
    assert!(
        near(px(p, 285, 60), [255, 255, 255, 255], 10),
        "{tag} {:?}",
        px(p, 285, 60)
    );
    assert!(
        near(px(p, 350, 60), [0, 0, 0, 255], 10),
        "{tag} {:?}",
        px(p, 350, 60)
    );
    // SDF: center white, near the edge dark, outside the page color.
    assert!(near(px(p, 90, 170), [255, 255, 255, 255], 10), "{tag}");
    assert!(
        near(px(p, 14, 170), [26, 26, 77, 255], 12),
        "{tag} {:?}",
        px(p, 14, 170)
    );
    assert!(
        near(px(p, 12, 123), [255, 255, 255, 255], 4),
        "{tag} corner outside"
    );
}

#[test]
fn cpu_fill_effect_background_sdf() {
    let d = dir("cpu");
    write_shaders(&d);
    let engine = ShaderEngine::with(ShaderLimits::cpu(), false);
    let (_scene, pipe) = build(&d, &engine);
    let p = render(&pipe);
    p.save_png(d.join("scene.png")).unwrap();
    check_scene(&p, "cpu");
    // Errors: tinted, reported.
    let err = pen_layout::display::SHADER_ERROR;
    let tinted = px(&p, 230, 170);
    assert!(tinted[1] < 240 && tinted[0] > 200, "{tinted:?} vs {err:?}");
    assert!(near(px(&p, 310, 170), tinted, 2));
    let stats = engine.stats();
    assert!(
        stats.errors.iter().any(|e| e.starts_with("broken.glsl")),
        "{:?}",
        stats.errors
    );
    assert!(stats.errors.iter().any(|e| e.contains("not found")));
    compare_golden("shader_scene_cpu", &p);
}

#[test]
fn gpu_matches_cpu() {
    let Some(g) = pen_shader::gpu::gpu() else {
        eprintln!("skip: no GPU adapter");
        return;
    };
    eprintln!("adapter: {}", g.adapter);
    let d = dir("gpu");
    write_shaders(&d);
    let engine = ShaderEngine::new();
    let (_scene, pipe) = build(&d, &engine);
    let p = render(&pipe);
    p.save_png(d.join("scene.png")).unwrap();
    check_scene(&p, "gpu");
}

#[test]
fn deferred_results_arrive_through_refresh() {
    let d = dir("deferred");
    write_shaders(&d);
    let engine = ShaderEngine::with(ShaderLimits::cpu(), true);
    let (scene, mut pipe) = build(&d, &engine);
    assert!(pipe.has_pending_assets());
    let t0 = Instant::now();
    while pipe.has_pending_assets() && t0.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(20));
        pipe.refresh_assets(&scene);
    }
    assert!(!pipe.has_pending_assets());
    check_scene(&render(&pipe), "deferred");
}

#[test]
fn edits_reuse_cached_results() {
    let d = dir("cache");
    write_shaders(&d);
    let engine = ShaderEngine::with(ShaderLimits::cpu(), false);
    let (mut scene, mut pipe) = build(&d, &engine);
    let renders = engine.stats().renders;
    // Moving the page translates the chunk: nothing is rendered again.
    let page = scene.find("page").unwrap();
    let (node, _) = pen_schema::node_from_value(serde_json::json!({ "x": 50 }), "t");
    scene
        .apply(pen_doc::DocOp::set(scene.node_ref(page).unwrap(), node))
        .unwrap();
    pipe.update(&mut scene);
    assert_eq!(engine.stats().renders, renders);
    // Changing a uniform renders that shader, plus the background shader drawn above it
    // (its `pencil:background` changed); the others come from the cache.
    let k = scene.find("checker").unwrap();
    let (node, _) = pen_schema::node_from_value(
        serde_json::json!({ "fill": { "type": "shader", "url": "checker.glsl", "uniforms": { "u_size": 10 } } }),
        "t",
    );
    scene
        .apply(pen_doc::DocOp::set(scene.node_ref(k).unwrap(), node))
        .unwrap();
    pipe.update(&mut scene);
    assert_eq!(engine.stats().renders, renders + 2);
}

/// Golden comparison (bootstrap when missing, `PEN_RASTER_UPDATE_GOLDEN=1` rewrites,
/// mismatches write `*.actual.png`).
fn compare_golden(name: &str, actual: &pen_raster::Pixmap) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    let actual_path = dir.join(format!("{name}.actual.png"));
    if std::env::var("PEN_RASTER_UPDATE_GOLDEN").is_ok_and(|v| v == "1") || !path.exists() {
        actual.save_png(&path).unwrap();
        let _ = std::fs::remove_file(&actual_path);
        return;
    }
    let expected = pen_raster::Pixmap::load_png(&path).unwrap();
    let actual_rt = pen_raster::Pixmap::decode_png(&actual.encode_png().unwrap()).unwrap();
    assert_eq!(
        (expected.width(), expected.height()),
        (actual.width(), actual.height())
    );
    let bad = actual_rt
        .data()
        .chunks(4)
        .zip(expected.data().chunks(4))
        .filter(|(a, e)| {
            a.iter()
                .zip(*e)
                .any(|(x, y)| (*x as i32 - *y as i32).abs() > 8)
        })
        .count();
    if bad as f64 > (expected.width() * expected.height()) as f64 * 0.002 {
        actual.save_png(&actual_path).unwrap();
        panic!(
            "{name}: {bad} pixels differ (wrote {})",
            actual_path.display()
        );
    }
    let _ = std::fs::remove_file(&actual_path);
}

/// The legacy shader-library examples (when present): every scene builds and renders
/// without panics; PNGs land in `target/pen-shader-tests/library/`.
#[test]
fn legacy_library_examples_render() {
    let goodies = workspace().join("legacy/extracted/editor/goodies");
    let examples = goodies.join("shader-library/examples");
    let Ok(rd) = std::fs::read_dir(&examples) else {
        eprintln!("skip: {} not found", examples.display());
        return;
    };
    let d = dir("library");
    // Same layout as the legacy app: `goodies/...` urls relative to the document.
    let assets = d.join("goodies/shader-library/assets");
    std::fs::create_dir_all(&assets).unwrap();
    for e in std::fs::read_dir(goodies.join("shader-library/assets"))
        .unwrap()
        .flatten()
    {
        std::fs::copy(e.path(), assets.join(e.file_name())).unwrap();
    }
    let gpu = pen_shader::gpu::gpu().is_some();
    let limit: usize = std::env::var("PEN_SHADER_EXAMPLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if gpu { usize::MAX } else { 3 });
    let engine = ShaderEngine::new();
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    files.sort();
    for f in files.into_iter().take(limit) {
        let nodes: serde_json::Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        let doc = serde_json::json!({ "version": "2.22", "children": nodes });
        let (mut scene, _) =
            SceneDoc::load(doc.to_string().as_bytes(), Some(&d), &pen_doc::NoLibraries).unwrap();
        let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
        pipe.set_shader_renderer(Arc::new(engine.clone()));
        pipe.rebuild(&mut scene);
        let Some(&root) = scene.roots().first() else {
            continue;
        };
        let r = pipe.node_rect(&scene, root).unwrap();
        let p = pen_raster::render(&pipe.list, r, 0.5, Color::WHITE);
        let name = f
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".pen.json", ".png");
        p.save_png(d.join(name)).unwrap();
    }
    let s = engine.stats();
    eprintln!(
        "{} renders, {} Mpx, {:?}; errors: {:?}",
        s.renders,
        s.pixels / 1_000_000,
        s.time,
        s.errors
    );
    assert!(
        s.errors.iter().all(|e| e.contains("not found")),
        "{:?}",
        s.errors
    );
}

const ANIMATED: &str = r#"/** @resolution */
uniform vec2 u_resolution;
/** @time */
uniform float u_time;
/** @mouse */
uniform vec2 u_mouse;
void main() {
  gl_FragColor = vec4(fract(u_time), u_mouse / u_resolution, 1.0);
}
"#;

const ANIM_DOC: &str = r##"{ "version": "2.22", "children": [
  { "type": "frame", "id": "page", "x": 0, "y": 0, "width": 200, "height": 100, "fill": "#ffffff",
    "layout": "none", "children": [
      { "type": "rectangle", "id": "anim", "x": 0, "y": 0, "width": 200, "height": 100,
        "fill": { "type": "shader", "url": "anim.glsl" } } ] },
  { "type": "rectangle", "id": "other", "x": 300, "y": 0, "width": 50, "height": 50,
    "fill": { "type": "shader", "url": "checker.glsl" } }
] }"##;

fn build_anim(d: &Path, engine: &ShaderEngine) -> (SceneDoc, Pipeline) {
    std::fs::write(d.join("anim.glsl"), ANIMATED).unwrap();
    let (mut scene, _) =
        SceneDoc::load(ANIM_DOC.as_bytes(), Some(d), &pen_doc::NoLibraries).expect("load");
    let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
    pipe.set_shader_renderer(Arc::new(engine.clone()));
    pipe.rebuild(&mut scene);
    (scene, pipe)
}

fn anim_px(list: &pen_render::DisplayList) -> [u8; 4] {
    let p = pen_raster::render(list, Rect::new(0.0, 0.0, 200.0, 100.0), 1.0, Color::WHITE);
    px(&p, 100, 50)
}

#[test]
fn animated_time_and_mouse() {
    let d = dir("animated");
    write_shaders(&d);
    let engine = ShaderEngine::with(ShaderLimits::cpu(), false);
    let (scene, mut pipe) = build_anim(&d, &engine);
    let k = scene.find("anim").unwrap();
    let page = scene.find("page").unwrap();
    // Only the animated node is reported, with the root to rebuild.
    let animated: Vec<_> = pipe.animated_shaders().collect();
    assert_eq!(animated.len(), 1);
    assert_eq!(animated[0].0, k);
    assert_eq!(animated[0].1.root, page);
    assert!(animated[0].1.time && animated[0].1.mouse);
    // Static output: t = 0, mouse at the center.
    assert!(near(anim_px(&pipe.list), [0, 128, 128, 255], 3));
    let other = pipe.list.chunks[1].clone();
    // A frame: t = 0.5, mouse at a quarter of the node (y down -> GL y up).
    let a = pen_layout::ShaderAnim {
        time: 0.5,
        mouse: Some(pen_render::Vec2::new(50.0, 25.0)),
    };
    let rebuilt = pipe.animate_shaders(&scene, &[(k, a)]);
    assert_eq!(rebuilt, vec![page]);
    assert!(
        Arc::ptr_eq(&other, &pipe.list.chunks[1]),
        "other chunk kept"
    );
    assert!(near(anim_px(&pipe.list), [128, 64, 191, 255], 3));
    // Same state again: nothing to rebuild.
    assert!(pipe.animate_shaders(&scene, &[(k, a)]).is_empty());
    // Exports stay deterministic (t = 0, center).
    let export = pipe.node_list(&scene, page);
    assert!(near(anim_px(&export), [0, 128, 128, 255], 3));
    // `render --time`: exports at another instant.
    pipe.set_export_shader_time(0.25);
    let export = pipe.node_list(&scene, page);
    assert!(near(anim_px(&export), [64, 128, 128, 255], 3));
}

#[test]
fn deferred_animation_keeps_previous_frame() {
    let d = dir("animated-deferred");
    write_shaders(&d);
    let engine = ShaderEngine::with(ShaderLimits::cpu(), true);
    let (scene, mut pipe) = build_anim(&d, &engine);
    let k = scene.find("anim").unwrap();
    let wait = |pipe: &mut Pipeline| {
        let t0 = Instant::now();
        while pipe.shaders_busy() && t0.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(5));
        }
        pipe.animate_shaders(&scene, &[])
    };
    wait(&mut pipe);
    assert!(!pipe.has_pending_assets());
    assert!(near(anim_px(&pipe.list), [0, 128, 128, 255], 3));
    let a = pen_layout::ShaderAnim {
        time: 0.5,
        mouse: None,
    };
    pipe.animate_shaders(&scene, &[(k, a)]);
    // While t = 0.5 renders, the t = 0 frame stays on screen (no flicker).
    assert!(pipe.shaders_busy() || pipe.has_pending_assets());
    if pipe.shaders_busy() {
        assert!(near(anim_px(&pipe.list), [0, 128, 128, 255], 3));
    }
    wait(&mut pipe);
    assert!(!pipe.has_pending_assets());
    assert!(near(anim_px(&pipe.list), [128, 128, 128, 255], 3));
    // Animation frames are marked transient (backends drop them once replaced).
    let transient = pipe.list.chunks[0].items.iter().any(|it| match it {
        pen_render::DisplayItem::Shape { fills, .. } => fills.iter().any(|f| {
            matches!(f, pen_render::Paint::Image { image, .. } if image.id & pen_render::TRANSIENT_IMAGE != 0)
        }),
        _ => false,
    });
    assert!(transient);
}
