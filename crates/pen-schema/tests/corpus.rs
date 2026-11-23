//! Corpus round-trip tests over `testdata/legacy/*.pen` and `testdata/user/*.pen`.
//! Skips silently when the (git-ignored) corpus is not present.

use std::path::{Path, PathBuf};

use pen_schema::json::{json_diff, json_eq};
use pen_schema::{
    DiagCode, LoadOptions, Severity, load_document, load_document_with, save_document,
    save_document_with, validate,
};
use serde_json::Value;

fn corpus_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
    let mut files = Vec::new();
    for dir in ["legacy", "user"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            eprintln!("testdata/{dir} not found; skipping");
            continue;
        };
        let mut list: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "pen"))
            .collect();
        list.sort();
        files.extend(list);
    }
    files
}

fn short(p: &Path) -> String {
    let dir = p
        .parent()
        .and_then(|d| d.file_name())
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "{dir}/{}",
        p.file_name().unwrap_or_default().to_string_lossy()
    )
}

#[test]
fn corpus_roundtrip() {
    let files = corpus_files();
    if files.is_empty() {
        return;
    }
    let mut failures = Vec::new();
    println!(
        "{:<34} {:>5} {:>6} {:>5} {:>5} {:>5} {:>6}  round-trip",
        "file", "from", "nodes", "migr", "lwarn", "verr", "vwarn"
    );
    for path in &files {
        let name = short(path);
        let bytes = std::fs::read(path).unwrap();
        let (doc, report) = match load_document(&bytes) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{name}: load failed: {e}"));
                continue;
            }
        };
        // (a) no hard errors while loading
        let load_errors: Vec<_> = report
            .warnings
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        if !load_errors.is_empty() {
            failures.push(format!(
                "{name}: {} load errors, first: {}",
                load_errors.len(),
                load_errors[0]
            ));
        }
        let diags = validate(&doc);
        let verr = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let vwarn = diags
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count();
        // (d) no duplicate ids
        if let Some(d) = diags
            .iter()
            .find(|d| d.code == DiagCode::DuplicateId && d.severity == Severity::Error)
        {
            failures.push(format!("{name}: {d}"));
        }
        // PEN_VERBOSE=1 lists every validation warning/error.
        let shown = if std::env::var_os("PEN_VERBOSE").is_some() {
            usize::MAX
        } else {
            5
        };
        let min = if shown == usize::MAX {
            Severity::Warning
        } else {
            Severity::Error
        };
        for d in diags.iter().filter(|d| d.severity >= min).take(shown) {
            println!("    {d}");
        }

        let saved = save_document(&doc);
        let status = if report.migrations.is_empty() {
            // (b) already current: save(load(x)) == x
            let (pure, _) = load_document_with(
                &bytes,
                &LoadOptions {
                    repair: false,
                    id_seed: None,
                },
            )
            .unwrap();
            let pure_saved = save_document(&pure);
            let original: Value = serde_json::from_slice(&bytes).unwrap();
            let again: Value = serde_json::from_str(&pure_saved).unwrap();
            if !json_eq(&original, &again) {
                failures.push(format!(
                    "{name}: round-trip differs: {:?}",
                    json_diff(&original, &again)
                ));
                "DIFF"
            } else {
                let normalized = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
                let formatted = save_document_with(&pure, &report.save_options);
                if formatted.as_bytes() == bytes.as_slice() {
                    "json-equal, byte-identical"
                } else {
                    // Line endings / trailing newline must never be the only difference.
                    if normalized.trim_end_matches('\n') == pure_saved {
                        failures.push(format!(
                            "{name}: only the line endings differ ({:?})",
                            report.save_options
                        ));
                    }
                    "json-equal"
                }
            }
        } else {
            // (c) migrate -> save -> load -> save is a fixed point
            let (doc2, report2) = load_document(saved.as_bytes()).unwrap();
            let saved2 = save_document(&doc2);
            if !report2.migrations.is_empty() {
                failures.push(format!("{name}: saved file still needs migration"));
            }
            if saved2 != saved {
                let a: Value = serde_json::from_str(&saved).unwrap();
                let b: Value = serde_json::from_str(&saved2).unwrap();
                failures.push(format!(
                    "{name}: not a fixed point: {:?}",
                    json_diff(&a, &b)
                ));
                "NOT FIXED"
            } else {
                "fixed point"
            }
        };
        println!(
            "{:<34} {:>5} {:>6} {:>5} {:>5} {:>5} {:>6}  {status}",
            name,
            report.source_version,
            doc.node_count(),
            report.migrations.len(),
            report.warnings.len(),
            verr,
            vwarn
        );
    }
    assert!(
        failures.is_empty(),
        "corpus failures:\n{}",
        failures.join("\n")
    );
}
