//! Test helpers: a tiny static HTTP server for the fixtures, rendering of an
//! imported frame through pen-layout + pen-raster, and image comparison.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn out_dir(name: &str) -> PathBuf {
    let d = crate_dir().join("../../target").join(name);
    std::fs::create_dir_all(&d).expect("create output dir");
    d
}

/// Serves `tests/fixtures`, the bundled Inter fonts under `/fonts/` and generated
/// images under `/img/`. Stops when dropped.
pub struct Server {
    pub base: String,
    stop: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect(self.addr);
    }
}

pub fn serve() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(c) = conn {
                std::thread::spawn(move || handle(c));
            }
        }
    });
    Server {
        base: format!("http://{addr}"),
        stop,
        addr,
    }
}

fn handle(mut c: TcpStream) {
    let mut reader = BufReader::new(c.try_clone().expect("clone"));
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    loop {
        let mut l = String::new();
        if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" || l == "\n" {
            break;
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
    let path = path.split('?').next().unwrap_or("/").to_string();
    let (status, ctype, body) = match route(&path) {
        Some((t, b)) => ("200 OK", t, b),
        None => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: max-age=3600\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = c.write_all(head.as_bytes());
    let _ = c.write_all(&body);
    let _ = c.flush();
}

fn route(path: &str) -> Option<(&'static str, Vec<u8>)> {
    if let Some(f) = path.strip_prefix("/fonts/") {
        let p = crate_dir().join("../../assets/fonts/inter").join(f);
        return std::fs::read(p).ok().map(|b| ("font/ttf", b));
    }
    if path == "/img/check.svg" {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M9 16.2 4.8 12l-1.4 1.4L9 19 21 7l-1.4-1.4z"/></svg>"#;
        return Some(("image/svg+xml", svg.as_bytes().to_vec()));
    }
    if let Some(name) = path.strip_prefix("/img/") {
        let name = name.trim_end_matches(".png");
        let seed: u32 = name
            .bytes()
            .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        let (w, h) = if name == "banner" {
            (1200, 400)
        } else {
            (600, 400)
        };
        return Some(("image/png", photo(w, h, seed)));
    }
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let p = crate_dir().join("tests/fixtures").join(rel);
    let ctype = if rel.ends_with(".css") {
        "text/css"
    } else {
        "text/html; charset=utf-8"
    };
    std::fs::read(p).ok().map(|b| (ctype, b))
}

/// A smooth, colorful synthetic "photo".
fn photo(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let a = (seed % 255) as f32;
    let b = ((seed / 7) % 255) as f32;
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let r = (a * (1.0 - fx) + 230.0 * fx) as u8;
        let g = (90.0 + 120.0 * fy + 30.0 * ((fx * 9.0).sin())) as u8;
        let bl = (b * fy + 200.0 * (1.0 - fy)) as u8;
        image::Rgb([r, g, bl])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

/// Wraps an imported frame into a document, assigning ids.
pub fn document(root: &Value, fonts: &[Value]) -> Value {
    let mut root = root.clone();
    let mut n = 0;
    assign_ids(&mut root, &mut n);
    let mut doc = json!({ "version": "2.22", "children": [root] });
    if !fonts.is_empty() {
        doc["fonts"] = Value::Array(fonts.to_vec());
    }
    doc
}

fn assign_ids(v: &mut Value, n: &mut usize) {
    *n += 1;
    v["id"] = Value::String(format!("n{n}"));
    if let Some(kids) = v.get_mut("children").and_then(Value::as_array_mut) {
        for k in kids {
            assign_ids(k, n);
        }
    }
}

/// Renders `doc` (saved in `dir`) at 1x; returns PNG bytes.
pub fn render(doc: &Value, dir: &Path, w: f32, h: f32) -> Vec<u8> {
    let bytes = serde_json::to_vec(doc).expect("serialize");
    let (mut scene, _) =
        pen_doc::SceneDoc::load(&bytes, Some(dir), &pen_doc::FsLibraries::default())
            .expect("load imported document");
    let mut text = pen_layout::TextEngine::new();
    // Web fonts saved next to the document (TTF/OTF only).
    let mut fonts = Vec::new();
    if let Some(defs) = doc.get("fonts").and_then(Value::as_array) {
        for d in defs {
            if let Some(url) = d["url"].as_str()
                && (url.ends_with(".ttf") || url.ends_with(".otf"))
                && let Ok(data) = std::fs::read(dir.join(url))
            {
                fonts.push(data);
            }
        }
    }
    for f in &fonts {
        text.load_font_data(f.clone());
    }
    pen_raster::with_shared_context(|c| {
        for f in &fonts {
            if !pen_layout::is_bundled_font(f) {
                c.load_font_data(f.clone());
            }
        }
    });
    let mut pipe = pen_layout::Pipeline::new(
        text,
        Box::new(pen_layout::FsAssets::new(Some(dir.to_path_buf()))),
    );
    pipe.rebuild(&mut scene);
    let list = std::mem::take(&mut pipe.list);
    pen_raster::render_to_png(
        &list,
        pen_render::Rect::new(0.0, 0.0, w, h),
        1.0,
        pen_render::Color::WHITE,
    )
    .expect("render")
}

pub struct Similarity {
    /// Share of pixels whose channels all differ by at most 32 (0..1).
    pub pixel: f64,
    /// Same on 4x4 block averages (forgives anti-aliasing and glyph shapes).
    pub block: f64,
    /// Mean absolute channel difference (0..255).
    pub mean_diff: f64,
}

pub fn compare(expected: &[u8], actual: &[u8], diff_path: Option<&Path>) -> Similarity {
    let e = image::load_from_memory(expected)
        .expect("expected png")
        .to_rgb8();
    let a = image::load_from_memory(actual)
        .expect("actual png")
        .to_rgb8();
    let w = e.width().min(a.width());
    let h = e.height().min(a.height());
    let mut ok = 0u64;
    let mut sum = 0u64;
    let mut diff = image::RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = e.get_pixel(x, y).0;
            let q = a.get_pixel(x, y).0;
            let d = (0..3).map(|i| p[i].abs_diff(q[i])).max().unwrap_or(0);
            sum += (0..3).map(|i| p[i].abs_diff(q[i]) as u64).sum::<u64>();
            if d <= 32 {
                ok += 1;
            }
            let v = 255 - d;
            diff.put_pixel(x, y, image::Rgb([255, v, v]));
        }
    }
    let total = (w as u64 * h as u64).max(1);
    let bs = 4;
    let (bw, bh) = (w / bs, h / bs);
    let mut bok = 0u64;
    let avg = |img: &image::RgbImage, bx: u32, by: u32| {
        let mut s = [0u32; 3];
        for y in 0..bs {
            for x in 0..bs {
                let p = img.get_pixel(bx * bs + x, by * bs + y).0;
                for i in 0..3 {
                    s[i] += p[i] as u32;
                }
            }
        }
        s.map(|v| v / (bs * bs))
    };
    for by in 0..bh {
        for bx in 0..bw {
            let p = avg(&e, bx, by);
            let q = avg(&a, bx, by);
            if (0..3).all(|i| p[i].abs_diff(q[i]) <= 24) {
                bok += 1;
            }
        }
    }
    if let Some(p) = diff_path {
        let _ = diff.save(p);
    }
    Similarity {
        pixel: ok as f64 / total as f64,
        block: bok as f64 / (bw as u64 * bh as u64).max(1) as f64,
        mean_diff: sum as f64 / (total * 3) as f64,
    }
}
