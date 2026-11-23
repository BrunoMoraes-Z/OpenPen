//! Real-world smoke test (network): ignored by default, run manually with
//! `cargo test -p pen-import --test smoke -- --ignored --nocapture`.
//! URLs can be replaced with `OPENPEN_SMOKE_URLS` (space separated).
//! Outputs go to `target/pen-import-smoke/<n>/` (never commit them).

mod common;

use std::time::Duration;

use pen_import::{ImportOptions, find_browser, import_url_blocking};

#[test]
#[ignore = "needs network access and a browser"]
fn real_pages() {
    if find_browser().is_none() {
        eprintln!("skipped: no Chrome/Edge/Chromium found");
        return;
    }
    let urls = std::env::var("OPENPEN_SMOKE_URLS").unwrap_or_else(|_| {
        "https://example.com https://www.rust-lang.org/learn https://docs.python.org/3/tutorial/index.html"
            .into()
    });
    let root = common::out_dir("pen-import-smoke");
    for (i, url) in urls.split_whitespace().enumerate() {
        let dir = root.join(i.to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let opts = ImportOptions {
            url: url.to_string(),
            assets_dir: Some(dir.clone()),
            capture_region: true,
            timeout: Duration::from_secs(120),
            ..Default::default()
        };
        let res = match import_url_blocking(opts, |p| eprintln!("  {p:?}")) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{url}: {e}");
                continue;
            }
        };
        let doc = common::document(&res.root, &res.fonts);
        std::fs::write(
            dir.join("page.pen"),
            serde_json::to_string_pretty(&doc).expect("json"),
        )
        .expect("write");
        let w = res.root["width"].as_f64().unwrap_or(1440.0) as f32;
        let h = res.root["height"].as_f64().unwrap_or(900.0) as f32;
        let actual = common::render(&doc, &dir, w, h);
        let expected = res.region_png.clone().unwrap_or_default();
        std::fs::write(dir.join("actual.png"), &actual).expect("write");
        std::fs::write(dir.join("expected.png"), &expected).expect("write");
        let sim = common::compare(&expected, &actual, Some(&dir.join("diff.png")));
        eprintln!(
            "{url}: pixels {:.1}% blocks {:.1}% ({} nodes, {} texts, {} images, {} svgs; {:?})",
            sim.pixel * 100.0,
            sim.block * 100.0,
            res.stats.nodes,
            res.stats.texts,
            res.stats.images,
            res.stats.svgs,
            res.stats
        );
        for w in &res.warnings {
            eprintln!("  warning: {w}");
        }
    }
}
