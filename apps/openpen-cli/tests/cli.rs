use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_openpen-cli"))
}

fn tmp(name: &str, content: &str) -> std::path::PathBuf {
    // Under target/ (reused across runs) instead of a new %TEMP% folder per run.
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("openpen-cli-test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

#[test]
fn validate_exit_codes() {
    let good = tmp(
        "good.pen",
        r#"{"version": "2.22", "children": [{"type": "rectangle", "id": "a"}]}"#,
    );
    let bad = tmp(
        "bad.pen",
        r#"{"version": "2.22", "children": [{"type": "ref", "id": "a", "ref": "zz"}]}"#,
    );
    assert!(bin().arg("validate").arg(&good).status().unwrap().success());
    let out = bin().arg("validate").arg(&good).arg(&bad).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("dangling-ref"));
}

#[test]
fn migrate_to_file() {
    let old = tmp(
        "old.pen",
        r#"{"version": "2.11", "children": [{"type": "icon_font", "id": "i", "iconFontName": "x"}]}"#,
    );
    let out = old.with_file_name("new.pen");
    assert!(
        bin()
            .arg("migrate")
            .arg(&old)
            .arg("-o")
            .arg(&out)
            .status()
            .unwrap()
            .success()
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(v["version"], "2.22");
    assert_eq!(v["children"][0]["type"], "icon");
}

#[test]
fn render_node_to_png() {
    let doc = tmp(
        "render.pen",
        r##"{"version": "2.22",
        "themes": {"Mode": ["Light", "Dark"]},
        "variables": {"bg": {"type": "color", "value": [{"value": "#ffffff"}, {"value": "#000000", "theme": {"Mode": "Dark"}}]}},
        "children": [
          {"type": "frame", "id": "f", "x": 0, "y": 0, "width": 40, "height": 20, "fill": "$bg",
           "children": [{"type": "rectangle", "id": "r", "width": 10, "height": 10, "fill": "#ff0000"}]}
        ]}"##,
    );
    let out = doc.with_file_name("render.png");
    let status = bin()
        .args(["render"])
        .arg(&doc)
        .arg("-o")
        .arg(&out)
        .args(["--node", "f", "--scale", "2", "--theme", "Mode=Dark"])
        .status()
        .unwrap();
    assert!(status.success());
    let png = std::fs::read(&out).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    // IHDR width/height (big endian) at bytes 16..24.
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((w, h), (80, 40));
    let missing = bin()
        .arg("render")
        .arg(&doc)
        .arg("-o")
        .arg(&out)
        .args(["--node", "zz"])
        .status()
        .unwrap();
    assert!(!missing.success());
}

#[test]
fn render_svg_and_pdf() {
    let doc = tmp(
        "vector.pen",
        r##"{"version": "2.22", "children": [
          {"type": "frame", "id": "f", "x": 0, "y": 0, "width": 40, "height": 20, "fill": "#ffffff",
           "children": [{"type": "text", "id": "t", "content": "Hi", "fill": "#000000", "fontSize": 12}]}
        ]}"##,
    );
    let svg = doc.with_file_name("vector.svg");
    let status = bin()
        .arg("render")
        .arg(&doc)
        .arg("-o")
        .arg(&svg)
        .status()
        .unwrap();
    assert!(status.success());
    let text = std::fs::read_to_string(&svg).unwrap();
    assert!(text.starts_with("<svg") || text.starts_with("<?xml"));
    assert!(text.contains("<text"));

    let outlined = doc.with_file_name("vector-outline.svg");
    let status = bin()
        .arg("render")
        .arg(&doc)
        .arg("-o")
        .arg(&outlined)
        .arg("--outline")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        !std::fs::read_to_string(&outlined)
            .unwrap()
            .contains("<text")
    );

    let pdf = doc.with_file_name("vector.pdf");
    let status = bin()
        .arg("render")
        .arg(&doc)
        .arg("-o")
        .arg(&pdf)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(&std::fs::read(&pdf).unwrap()[..5], b"%PDF-");

    let bad = doc.with_file_name("vector.bmp");
    let status = bin()
        .arg("render")
        .arg(&doc)
        .arg("-o")
        .arg(&bad)
        .status()
        .unwrap();
    assert!(!status.success());
}
