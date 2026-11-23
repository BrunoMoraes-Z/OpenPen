//! Streaming `execute` through the coordinator and the document the app uses,
//! without a window: growing prefixes of a realistic snippet (cut anywhere,
//! including mid-string, mid-template-literal and inside an `if` that may
//! still get an `else`) end in the same document, response and undo step as
//! a direct `execute`. On the corpus document it also measures the cost of a
//! partial (`cargo test --release -p openpen --test agent_stream -- --nocapture`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use openpen::agent::stream::{LocalDocs, Outcome, StreamCoordinator};
use openpen::scene_document::SceneDocument;
use pen_script::{ExecuteRequest, ExecuteResponse, ScriptEngine};

const SNIPPET: &str = r##"
screen = Insert(document, {type: "frame", name: "Login", x: 4000, y: 0, width: 390, height: 844,
  layout: "vertical", gap: 16, padding: 24, fill: "#FFFFFF"})
const title = Insert(screen, {type: "text", name: "Title", content: "Welcome back", fontFamily: "Inter", fontSize: 28, fill: "#111111"})
const fields = ["Email", "Password"]
for (const f of fields) {
  const row = Insert(screen, {type: "frame", name: `${f} field`, width: "fill_container", height: 48,
    cornerRadius: 8, stroke: {fill: "#D0D5DD", thickness: 1}, padding: [0, 12], layout: "horizontal", alignItems: "center"})
  Insert(row, {type: "text", name: `${f} label`, content: `Your ${f.toLowerCase()}`, fontFamily: "Inter", fontSize: 15, fill: "#667085"})
}
if (fields.length > 1) {
  Insert(screen, {type: "frame", name: "Remember", width: "fill_container", height: 24})
}
else {
  Insert(screen, {type: "frame", name: "Single", width: 10, height: 10})
}
const cta = Insert(screen, {type: "frame", name: "Sign in", width: "fill_container", height: 52, cornerRadius: 12,
  fill: "#4F46E5", layout: "horizontal", justifyContent: "center", alignItems: "center"})
Insert(cta, {type: "text", name: "CTA label", content: "Sign in", fontFamily: "Inter", fontSize: 16, fill: "#FFFFFF"})
Update(title, {fontWeight: "700"})
Print(`created ${Get(screen).children.length} rows`)
"##;

fn req(src: &str) -> ExecuteRequest {
    ExecuteRequest {
        agent_id: "chat".into(),
        description: "Create a login screen".into(),
        input: Some(src.into()),
        ..Default::default()
    }
}

fn coordinator() -> StreamCoordinator<usize> {
    let mut engine = ScriptEngine::new();
    engine.set_id_seed(Some(42));
    StreamCoordinator::new(engine)
}

fn done(o: Outcome) -> (ExecuteResponse, bool) {
    match o {
        Outcome::Done { resp, streamed } => (resp, streamed),
        o => panic!("{o:?}"),
    }
}

/// Byte offsets right inside the awkward places of `SNIPPET`.
fn awkward_cuts(src: &str) -> Vec<usize> {
    let at = |pat: &str, off: usize| src.find(pat).unwrap() + off;
    vec![
        at("Welcome back", 4),          // mid-string
        at("`${f} field`", 3),          // inside a template `${`
        at("`Your ${f.toLowerCase", 8), // mid-template expression
        at("height: 24})\n}", 13),      // `if` block closed, no `else` yet
        at("}\nelse", 2),               // `if (...) {...}\ne`
        at("}\nelse", 4),               // `...\nels`
        at("Print(`created", 9),        // inside the last template
    ]
}

/// Streams `src` in growing prefixes (plus the awkward cuts) and finalizes.
fn stream(
    c: &mut StreamCoordinator<usize>,
    h: &mut LocalDocs,
    src: &str,
    step: usize,
) -> (ExecuteResponse, bool) {
    let mut cuts: Vec<usize> = (1..src.len()).step_by(step).collect();
    cuts.extend(awkward_cuts(src));
    cuts.sort_unstable();
    let t = Instant::now();
    for (k, cut) in cuts.into_iter().enumerate() {
        let now = t + Duration::from_millis(20 * k as u64);
        c.partial(h, "chat", "toolu_1", 0, src[..cut].to_string(), now);
        c.pump(h, now);
        assert!(
            c.streams()[0].error.is_none(),
            "{:?} at {cut}",
            c.streams()[0].error
        );
    }
    done(c.execute(h, "client-7", Some("toolu_1"), &0, req(src)))
}

#[test]
fn streamed_snippet_matches_direct_execute() {
    let mut c = coordinator();
    let mut h = LocalDocs::new(vec![SceneDocument::empty(&[])]);
    let (direct, _) = done(c.execute(&mut h, "chat", None, &0, req(SNIPPET)));
    assert!(direct.ok, "{}", direct.text);
    let direct_doc = h.doc(0).save_string();

    for step in [1, 7, 61] {
        let mut c = coordinator();
        let mut h = LocalDocs::new(vec![SceneDocument::empty(&[])]);
        let before = h.doc(0).save_string();
        let (resp, streamed) = stream(&mut c, &mut h, SNIPPET, step);
        assert!(streamed);
        assert!(resp.ok, "{}", resp.text);
        assert_eq!(resp.text, direct.text);
        assert_eq!(resp.prints, ["created 5 rows"]);
        assert_eq!(h.doc(0).save_string(), direct_doc, "step {step}");
        assert_eq!(h.doc(0).undo_steps(), 1);
        assert!(h.doc(0).is_dirty());
        // Every touched entry flashed exactly once (live or at the end).
        let flashed: Vec<&str> = h.flashes.iter().map(|(_, id)| id.as_str()).collect();
        let touched: Vec<&str> = direct.touched.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(flashed, touched);
        assert!(h.doc_mut(0).undo());
        h.doc_mut(0).flush();
        assert_eq!(h.doc(0).save_string(), before);
    }
}

#[test]
fn awkward_cuts_never_evaluate_half_a_statement() {
    let mut c = coordinator();
    let mut h = LocalDocs::new(vec![SceneDocument::empty(&[])]);
    let t = Instant::now();
    for (k, cut) in awkward_cuts(SNIPPET).into_iter().enumerate() {
        let now = t + Duration::from_millis(100 * k as u64);
        c.partial(
            &mut h,
            "chat",
            "toolu_1",
            0,
            SNIPPET[..cut].to_string(),
            now,
        );
        c.pump(&mut h, now);
        let s = &c.streams()[0];
        assert!(s.error.is_none(), "{:?}", s.error);
        // The evaluated part always ends at a statement boundary before the cut.
        assert!(s.evaluated() <= cut);
        let ev = &SNIPPET[..s.evaluated()];
        assert!(
            ev.is_empty() || ev.ends_with('\n') || ev.ends_with('}'),
            "{ev:?}"
        );
    }
    // Inside the `if` without its `else`, the `if` is not run yet.
    let cut = SNIPPET.find("}\nelse").unwrap() + 4;
    c.partial(
        &mut h,
        "chat",
        "toolu_1",
        0,
        SNIPPET[..cut].to_string(),
        t + Duration::from_secs(5),
    );
    c.pump(&mut h, t + Duration::from_secs(5));
    assert!(!h.doc(0).save_string().contains("Remember"));
    let (resp, _) = done(c.execute(&mut h, "chat", Some("toolu_1"), &0, req(SNIPPET)));
    assert!(resp.ok, "{}", resp.text);
    assert!(h.doc(0).save_string().contains("Remember"));
    assert!(!h.doc(0).save_string().contains("Single"));
}

#[test]
fn error_in_the_last_statement_rolls_back_the_streamed_part() {
    let bad = format!("{SNIPPET}Update(\"missing-node\", {{name: \"x\"}})\nPrint(1)\n");
    let mut c = coordinator();
    let mut h = LocalDocs::new(vec![SceneDocument::empty(&[])]);
    let before = h.doc(0).save_string();
    let t = Instant::now();
    for (k, cut) in (1..bad.len()).step_by(40).enumerate() {
        let now = t + Duration::from_millis(20 * k as u64);
        c.partial(&mut h, "chat", "toolu_1", 0, bad[..cut].to_string(), now);
        c.pump(&mut h, now);
    }
    let (resp, _) = done(c.execute(&mut h, "chat", Some("toolu_1"), &0, req(&bad)));
    assert!(!resp.ok);
    assert!(resp.text.contains("rolled back"), "{}", resp.text);
    assert_eq!(h.doc(0).save_string(), before);
    assert_eq!(h.doc(0).undo_steps(), 0);
    assert_eq!(h.doc(0).stream_lock(), None);
}

/// Per-partial cost on the big corpus document (copied to `target/`).
#[test]
fn corpus_partial_cost() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let src = root.join("testdata/user/t-process-recorder.pen");
    if !src.exists() {
        eprintln!("skip: {} not found", src.display());
        return;
    }
    let dir = root.join("target/stream-bench");
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("t-process-recorder.pen");
    std::fs::copy(&src, &copy).unwrap();
    let (doc, _) = SceneDocument::load(&copy, &[]).unwrap();
    let original = doc.save_string();
    let mut c = coordinator();
    c.config.cost_factor = 0; // evaluate every prefix that completes a statement
    let mut h = LocalDocs::new(vec![doc]);
    let t = Instant::now();
    let mut costs = Vec::new();
    for (k, cut) in (1..SNIPPET.len()).step_by(12).enumerate() {
        let now = t + Duration::from_millis(20 * k as u64);
        c.partial(
            &mut h,
            "chat",
            "toolu_1",
            0,
            SNIPPET[..cut].to_string(),
            now,
        );
        let evals = c.streams()[0].evals;
        let t0 = Instant::now();
        c.pump(&mut h, now);
        if c.streams()[0].evals > evals {
            costs.push(t0.elapsed());
        }
    }
    let t0 = Instant::now();
    let (resp, streamed) = done(c.execute(&mut h, "chat", Some("toolu_1"), &0, req(SNIPPET)));
    let fin = t0.elapsed();
    assert!(resp.ok && streamed, "{}", resp.text);
    costs.sort();
    let total: Duration = costs.iter().sum();
    eprintln!(
        "corpus partials: {} evaluations, median {:.2?}, max {:.2?}, mean {:.2?}; finalize {:.2?}; nodes {}",
        costs.len(),
        costs[costs.len() / 2],
        costs[costs.len() - 1],
        total / costs.len() as u32,
        fin,
        h.doc(0).scene.document().node_count(),
    );
    assert!(h.doc_mut(0).undo());
    h.doc_mut(0).flush();
    assert_eq!(h.doc(0).save_string(), original);
}
