//! Fidelity of the web import: each fixture page is imported through a real
//! headless browser, the resulting `.pen` frame is rendered with pen-layout +
//! pen-raster at 1x and compared with the browser's own screenshot.
//!
//! Skipped when no Chrome/Edge/Chromium is installed. Outputs (the `.pen`,
//! assets, both PNGs and a diff) go to `target/pen-import-fidelity/<fixture>/`.

mod common;

use std::time::Duration;

use pen_import::{ImportOptions, find_browser, import_url_blocking};

struct Case {
    name: &'static str,
    /// Minimum share of matching 4x4 blocks.
    min_block: f64,
    /// Minimum share of matching pixels.
    min_pixel: f64,
}

const CASES: &[Case] = &[
    Case {
        name: "landing",
        min_block: 0.97,
        min_pixel: 0.96,
    },
    Case {
        name: "cards",
        min_block: 0.97,
        min_pixel: 0.96,
    },
    Case {
        name: "nav",
        min_block: 0.97,
        min_pixel: 0.96,
    },
    Case {
        name: "form",
        min_block: 0.97,
        min_pixel: 0.96,
    },
    Case {
        name: "lazy",
        min_block: 0.97,
        min_pixel: 0.96,
    },
];

#[test]
fn fidelity() {
    if find_browser().is_none() {
        eprintln!("skipped: no Chrome/Edge/Chromium found");
        return;
    }
    let server = common::serve();
    let root = common::out_dir("pen-import-fidelity");
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for case in CASES {
        let dir = root.join(case.name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("case dir");
        let opts = ImportOptions {
            url: format!("{}/{}.html", server.base, case.name),
            viewport_width: 1280,
            viewport_height: 800,
            wait_ms: 200,
            assets_dir: Some(dir.clone()),
            timeout: Duration::from_secs(60),
            capture_region: true,
            ..Default::default()
        };
        let res = import_url_blocking(opts, |_| {}).expect("import");
        let expected = res.region_png.clone().expect("region screenshot");
        let doc = common::document(&res.root, &res.fonts);
        std::fs::write(
            dir.join(format!("{}.pen", case.name)),
            serde_json::to_string_pretty(&doc).expect("json"),
        )
        .expect("write pen");
        let w = res.root["width"].as_f64().unwrap_or(1280.0) as f32;
        let h = res.root["height"].as_f64().unwrap_or(800.0) as f32;
        let actual = common::render(&doc, &dir, w, h);
        std::fs::write(dir.join("expected.png"), &expected).expect("write");
        std::fs::write(dir.join("actual.png"), &actual).expect("write");
        let sim = common::compare(&expected, &actual, Some(&dir.join("diff.png")));
        let line = format!(
            "{:8} pixels {:5.1}%  blocks {:5.1}%  mean diff {:4.1}  ({} nodes, {} texts, {} images, {} svgs, {} ms)",
            case.name,
            sim.pixel * 100.0,
            sim.block * 100.0,
            sim.mean_diff,
            res.stats.nodes,
            res.stats.texts,
            res.stats.images,
            res.stats.svgs,
            res.stats.total_ms,
        );
        eprintln!("{line}");
        for wmsg in &res.warnings {
            eprintln!("         warning: {wmsg}");
        }
        report.push(line);
        if sim.pixel < case.min_pixel || sim.block < case.min_block {
            failures.push(format!(
                "{}: pixels {:.3} (min {}), blocks {:.3} (min {}); see {}",
                case.name,
                sim.pixel,
                case.min_pixel,
                sim.block,
                case.min_block,
                dir.display()
            ));
        }
    }
    let _ = std::fs::write(root.join("report.txt"), report.join("\n"));
    assert!(
        failures.is_empty(),
        "fidelity below threshold:\n{}",
        failures.join("\n")
    );
}

/// Viewport-only import with a selector and flex inference: structure checks.
#[test]
fn selector_and_layout_inference() {
    if find_browser().is_none() {
        eprintln!("skipped: no Chrome/Edge/Chromium found");
        return;
    }
    let server = common::serve();
    let opts = ImportOptions {
        url: format!("{}/nav.html", server.base),
        viewport_width: 1280,
        viewport_height: 800,
        full_page: false,
        selector: Some("nav".into()),
        wait_ms: 0,
        infer_layout: true,
        timeout: Duration::from_secs(60),
        ..Default::default()
    };
    let res = import_url_blocking(opts, |_| {}).expect("import");
    let root = &res.root;
    assert_eq!(root["type"], "frame");
    assert_eq!(root["width"].as_f64(), Some(1280.0));
    assert_eq!(root["height"].as_f64(), Some(72.0));
    let text = serde_json::to_string(root).expect("json");
    assert!(text.contains("\"Acme\""), "logo text imported");
    assert!(text.contains("\"Changelog\""));
    assert!(text.contains("\"NEW\""), "::after content imported");
    assert!(
        text.contains("\"geometry\""),
        "inline SVG imported as paths"
    );
    assert!(
        text.contains("\"layout\":\"horizontal\""),
        "a flex row was turned into auto layout"
    );
    assert!(
        !text.contains("\"id\""),
        "no ids are assigned by the importer"
    );
    assert!(res.screenshot_png.is_some());
}
