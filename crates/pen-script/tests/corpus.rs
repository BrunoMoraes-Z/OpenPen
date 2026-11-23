//! Corpus: for every `testdata/user/*.pen`, a read-only `Get` sweep leaves the
//! document untouched, and an edit snippet followed by one undo restores a
//! byte-identical save. Skips when the folder is absent (it is not committed).

use std::path::{Path, PathBuf};
use std::time::Instant;

use pen_doc::{FsLibraries, SceneDoc, UndoStack};
use pen_layout::{NoAssets, Pipeline, TextEngine};
use pen_script::{ExecuteRequest, HeadlessHost, ScriptEngine};

fn files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/user");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pen"))
        .collect();
    out.sort();
    out
}

fn req(src: &str) -> ExecuteRequest {
    ExecuteRequest {
        agent_id: "corpus".into(),
        description: "corpus".into(),
        input: Some(src.into()),
        ..Default::default()
    }
}

const READ: &str = r#"
let count = 0, instances = 0, clipped = 0
Get((n, c) => { count++; if (c.problems) clipped++ })
const refs = Get(n => n.type === "ref" ? n.id : undefined)
for (const id of refs.slice(0, 20)) {
  const full = Get(id, {resolveInstances: true, depth: 2})
  instances++
  if (full.children && Array.isArray(full.children) && full.children.length) {
    Get(full.children[0].id)
  }
}
const vars = GetVariables()
Print(count, refs.length, instances, clipped, Object.keys(vars.variables).length)
"#;

const EDIT: &str = r##"
const first = Get(n => n.id)[0]
const pos = FindEmptySpace({width: 400, height: 300, padding: 40})
s = Insert(document, {type: "frame", name: "Agent screen", x: pos.x, y: pos.y, width: 400, layout: "vertical", gap: 8, padding: 16, children: [
  {type: "text", name: "Title", content: "Hello", fontFamily: "Inter", fontSize: 24, fill: "#111111"},
  {type: "rectangle", name: "Bar", width: "fill_container", height: 8, fill: "#3366ff"}
]})
if (first) {
  Update(first, {name: "Renamed by agent"})
  Copy(first, document, {name: "Copied", x: pos.x, y: pos.y + 400})
}
SetVariables({"agent-accent": {type: "color", value: "#ff6600"}})
const b = Get(s, (n, c) => c.bounds)[0]
Print(b.width, b.height > 0)
"##;

#[test]
fn corpus_read_and_edit_undo() {
    let files = files();
    if files.is_empty() {
        eprintln!("testdata/user not found; skipping");
        return;
    }
    let mut eng = ScriptEngine::new();
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let dir = path.parent().unwrap();
        let (mut scene, _) = SceneDoc::load(&bytes, Some(dir), &FsLibraries::default()).unwrap();
        let mut pipe = Pipeline::new(TextEngine::new(), Box::new(NoAssets));
        pipe.rebuild(&mut scene);
        let mut undo = UndoStack::new();
        let original = scene.save();
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        let t = Instant::now();
        let r = eng.execute(
            &mut scene,
            &mut pipe,
            &mut undo,
            &mut HeadlessHost,
            req(READ),
        );
        assert!(r.ok, "{name}: read failed:\n{}", r.text);
        assert_eq!(r.ops, 0, "{name}");
        assert_eq!(undo.undo_len(), 0, "{name}");
        assert!(
            scene.save() == original,
            "{name}: read-only snippet changed the document"
        );
        let read_ms = t.elapsed().as_secs_f64() * 1000.0;
        let summary = r.prints.join(" ");

        let t = Instant::now();
        let r = eng.execute(
            &mut scene,
            &mut pipe,
            &mut undo,
            &mut HeadlessHost,
            req(EDIT),
        );
        assert!(r.ok, "{name}: edit failed:\n{}", r.text);
        assert_eq!(
            r.prints.last().map(String::as_str),
            Some("400 true"),
            "{name}"
        );
        assert_eq!(undo.undo_len(), 1, "{name}");
        let edit_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(undo.undo(&mut scene).unwrap());
        pipe.update(&mut scene);
        assert!(
            scene.save() == original,
            "{name}: undo did not restore the file"
        );
        eprintln!(
            "{name}: read {read_ms:.1} ms (nodes, refs, expanded, clipped, vars = {summary}), edit {edit_ms:.1} ms, {} warning(s)",
            r.warnings.len()
        );
        for w in r.warnings.iter().take(3) {
            eprintln!("    warning: {w}");
        }
    }
}
