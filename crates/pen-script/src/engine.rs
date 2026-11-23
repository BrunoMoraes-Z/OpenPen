//! The QuickJS side: runtime limits, the host function, chunked evaluation,
//! commit/rollback and the response.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use pen_doc::{SceneDoc, UndoStack};
use pen_layout::{NoAssets, Pipeline, TextEngine};
use pen_schema::{Document, IdGenerator};
use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, CaughtError, Context, Function, Runtime};
use serde_json::{Value, json};

use crate::errors::{SNIPPET_FILE, call_text, snippet_location, truncate};
use crate::host::{ExportRequest, RenderTarget, ScriptHost};
use crate::nodes::is_root;
use crate::stream::complete_prefix;
use crate::tx::{Touch, Tx};

const PRELUDE: &str = include_str!("prelude.js");

/// Resource limits of one `execute` call.
#[derive(Debug, Clone)]
pub struct Limits {
    /// QuickJS heap limit (bytes).
    pub memory_bytes: usize,
    /// QuickJS native stack limit (bytes).
    pub stack_bytes: usize,
    /// CPU budget per evaluated chunk (time spent in host calls is not counted).
    pub cpu_per_chunk: Duration,
    /// Wall-clock limit of the whole call (including host calls).
    pub total: Duration,
    /// Max total size (JSON bytes) of the globals persisted per agent.
    pub max_global_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            memory_bytes: 256 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            cpu_per_chunk: Duration::from_secs(2),
            total: Duration::from_secs(55),
            max_global_bytes: 1024 * 1024,
        }
    }
}

/// One find/replace patch of a failed snippet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edit {
    pub find: String,
    pub replace: String,
    /// Replace every occurrence (otherwise `find` must match exactly once).
    pub all: bool,
}

/// An `execute` request.
#[derive(Debug, Clone, Default)]
pub struct ExecuteRequest {
    /// Agent / conversation id: globals persist per agent.
    pub agent_id: String,
    /// Short label of the call (shown to the user by the host).
    pub description: String,
    /// The snippet. Ignored when `edit_id` is set.
    pub input: Option<String>,
    /// Patch and rerun the failed snippet stored under this id.
    pub edit_id: Option<String>,
    pub edits: Vec<Edit>,
}

/// Where and why a snippet failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    pub message: String,
    /// 1-based position in the snippet, when known.
    pub line: Option<usize>,
    pub column: Option<usize>,
    /// Source text of the offending call.
    pub call: Option<String>,
}

/// Result of an `execute` call.
#[derive(Debug, Clone, Default)]
pub struct ExecuteResponse {
    pub ok: bool,
    /// Full message for the agent (created ids, prints, warnings or the error).
    pub text: String,
    /// Screenshots (PNG).
    pub images: Vec<Vec<u8>>,
    /// `(name, id)` of the created nodes.
    pub created: Vec<(String, String)>,
    /// On failure: id to patch the snippet with `edits`.
    pub edit_id: Option<String>,
    /// Nodes written/read (for the agent flash on the canvas).
    pub touched: Vec<Touch>,
    pub prints: Vec<String>,
    pub warnings: Vec<String>,
    pub exported: Vec<PathBuf>,
    pub error: Option<ScriptError>,
    /// Number of document operations applied (0 on failure).
    pub ops: usize,
    pub duration: Duration,
}

/// Progress of a streamed snippet (see [`ScriptEngine::execute_partial`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartialStatus {
    /// Bytes of the input evaluated so far.
    pub evaluated: usize,
    /// First error (evaluation stops; the final call reports it).
    pub error: Option<String>,
    /// Nodes touched since the previous partial call of this stream (for the
    /// live flash); the final response still lists every touched node.
    pub touched: Vec<Touch>,
    /// Document operations applied so far by the stream (uncommitted).
    pub ops: usize,
}

// ---------------------------------------------------------------------- budget

struct BudgetState {
    cpu: Duration,
    chunk_deadline: Instant,
    total_deadline: Instant,
    total: Duration,
    reason: Option<String>,
}

struct Budget(Mutex<BudgetState>);

impl Budget {
    fn new(l: &Limits) -> Budget {
        let now = Instant::now();
        Budget(Mutex::new(BudgetState {
            cpu: l.cpu_per_chunk,
            chunk_deadline: now + l.cpu_per_chunk,
            total_deadline: now + l.total,
            total: l.total,
            reason: None,
        }))
    }

    fn st(&self) -> MutexGuard<'_, BudgetState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start_chunk(&self) {
        let mut s = self.st();
        s.chunk_deadline = Instant::now() + s.cpu;
        s.reason = None;
    }

    /// Host time is not charged to the CPU budget.
    fn credit(&self, d: Duration) {
        self.st().chunk_deadline += d;
    }

    fn interrupt(&self) -> bool {
        let mut s = self.st();
        let now = Instant::now();
        if now > s.total_deadline {
            s.reason = Some(format!(
                "execute timed out after {} s",
                s.total.as_secs_f32()
            ));
            return true;
        }
        if now > s.chunk_deadline {
            s.reason = Some(format!(
                "script interrupted: CPU budget of {} s exceeded (infinite loop?)",
                s.cpu.as_secs_f32()
            ));
            return true;
        }
        false
    }

    fn timed_out(&self) -> bool {
        Instant::now() > self.st().total_deadline
    }

    fn reason(&self) -> Option<String> {
        self.st().reason.clone()
    }
}

fn lock(tx: &Mutex<Tx>) -> MutexGuard<'_, Tx> {
    tx.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------- run

/// One transaction: a QuickJS context plus the transaction state. Lives for one
/// `execute` call, or across the partial calls of a streamed snippet.
struct Run {
    ctx: Context,
    #[allow(dead_code)]
    rt: Runtime,
    tx: Arc<Mutex<Tx>>,
    budget: Arc<Budget>,
    source: String,
    done: usize,
    error: Option<ScriptError>,
    /// `tx.out.touched` entries already reported by `execute_partial`.
    reported: usize,
}

fn eval_options() -> EvalOptions {
    let mut o = EvalOptions::default();
    o.global = true;
    o.strict = false;
    o.filename = Some(SNIPPET_FILE.to_string());
    o
}

fn describe(e: CaughtError<'_>) -> (String, Option<String>) {
    match e {
        CaughtError::Exception(ex) => {
            let name: Option<String> = ex.get::<_, Option<String>>("name").ok().flatten();
            let msg = ex.message().unwrap_or_default();
            let msg = match name.as_deref() {
                // Errors thrown by the API carry their message only.
                Some("Error") | None => msg,
                Some(n) => format!("{n}: {msg}"),
            };
            (msg, ex.stack())
        }
        CaughtError::Value(v) => {
            let s = if let Some(s) = v.as_string() {
                s.to_string().unwrap_or_default()
            } else if v.is_null() {
                // QuickJS reports memory exhaustion as a null exception.
                "out of memory".to_string()
            } else {
                format!("{v:?}")
            };
            (format!("uncaught exception: {s}"), None)
        }
        CaughtError::Error(e) => (e.to_string(), None),
    }
}

/// 0-based (line, column in chars) of byte offset `at`.
fn position(src: &str, at: usize) -> (usize, usize) {
    let before = &src[..at];
    let line = before.matches('\n').count();
    let col = before
        .rfind('\n')
        .map_or(before, |i| &before[i + 1..])
        .chars()
        .count();
    (line, col)
}

impl Run {
    fn new(
        limits: &Limits,
        scene: SceneDoc,
        pipe: Pipeline,
        globals: Option<&IndexMap<String, String>>,
        id_seed: Option<u64>,
    ) -> Result<Run, String> {
        let rt = Runtime::new().map_err(|e| format!("cannot start QuickJS: {e}"))?;
        rt.set_memory_limit(limits.memory_bytes);
        rt.set_max_stack_size(limits.stack_bytes);
        let budget = Arc::new(Budget::new(limits));
        let b = budget.clone();
        rt.set_interrupt_handler(Some(Box::new(move || b.interrupt())));
        let ctx = Context::full(&rt).map_err(|e| format!("cannot create JS context: {e}"))?;
        let mut tx = Tx::new(scene, pipe);
        if let Some(seed) = id_seed {
            tx.seed_ids(seed);
        }
        let tx = Arc::new(Mutex::new(tx));
        let globals_json = globals.map(|g| serde_json::to_string(g).unwrap_or_default());
        let setup = ctx.with(|ctx| -> Result<(), String> {
            let tx = tx.clone();
            let budget = budget.clone();
            let f = Function::new(ctx.clone(), move |name: String, args: String| -> String {
                let t0 = Instant::now();
                let res: Result<Value, String> = if budget.timed_out() {
                    Err("execute timed out".to_string())
                } else {
                    match serde_json::from_str::<Value>(&args) {
                        Ok(args) => {
                            catch_unwind(AssertUnwindSafe(|| lock(&tx).dispatch(&name, &args)))
                                .unwrap_or_else(|_| Err(format!("internal error in {name}")))
                        }
                        Err(e) => Err(format!("invalid arguments: {e}")),
                    }
                };
                budget.credit(t0.elapsed());
                match res {
                    Ok(v) => json!({ "v": v }).to_string(),
                    Err(e) => json!({ "e": e }).to_string(),
                }
            })
            .map_err(|e| e.to_string())?;
            ctx.globals().set("__host", f).map_err(|e| e.to_string())?;
            let mut o = EvalOptions::default();
            o.filename = Some("prelude.js".into());
            ctx.eval_with_options::<rquickjs::Value, _>(PRELUDE, o)
                .catch(&ctx)
                .map_err(|e| format!("prelude failed: {}", describe(e).0))?;
            if let Some(g) = globals_json {
                let import: Function = ctx
                    .globals()
                    .get("__importGlobals")
                    .map_err(|e| e.to_string())?;
                import
                    .call::<_, rquickjs::Value>((g,))
                    .catch(&ctx)
                    .map_err(|e| format!("restoring globals failed: {}", describe(e).0))?;
            }
            Ok(())
        });
        setup?;
        Ok(Run {
            ctx,
            rt,
            tx,
            budget,
            source: String::new(),
            done: 0,
            error: None,
            reported: 0,
        })
    }

    fn swap(&self, scene: &mut SceneDoc, pipe: &mut Pipeline) {
        let mut g = lock(&self.tx);
        std::mem::swap(&mut g.scene, scene);
        std::mem::swap(&mut g.pipe, pipe);
    }

    /// Evaluates `source[done..end]` in the context (keeping line numbers).
    fn eval_to(&mut self, end: usize) {
        if self.error.is_some() || end <= self.done {
            return;
        }
        let chunk = &self.source[self.done..end];
        if chunk.trim().is_empty() {
            self.done = end;
            return;
        }
        let (line, col) = position(&self.source, self.done);
        let mut text = String::with_capacity(line + col + chunk.len());
        text.extend(std::iter::repeat_n('\n', line));
        text.extend(std::iter::repeat_n(' ', col));
        text.push_str(chunk);
        self.budget.start_chunk();
        let failed = self.ctx.with(|ctx| {
            let r = ctx
                .eval_with_options::<rquickjs::Value, _>(text, eval_options())
                .catch(&ctx);
            match r {
                Ok(_) => None,
                Err(e) => Some(describe(e)),
            }
        });
        self.done = end;
        let failed = failed.or_else(|| self.run_jobs());
        if let Some((mut message, stack)) = failed {
            if let Some(reason) = self.budget.reason() {
                message = reason;
            }
            let loc = stack.as_deref().and_then(snippet_location);
            self.error = Some(ScriptError {
                message,
                line: loc.map(|l| l.0),
                column: loc.map(|l| l.1),
                call: loc.and_then(|(l, c)| call_text(&self.source, l, c)),
            });
        }
    }

    /// Runs queued promise jobs (async snippets).
    fn run_jobs(&self) -> Option<(String, Option<String>)> {
        for _ in 0..100_000 {
            match self.rt.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => {
                    let msg = e.0.with(|ctx| {
                        describe(CaughtError::from_error(&ctx, rquickjs::Error::Exception))
                    });
                    return Some(msg);
                }
            }
        }
        None
    }

    /// Serialized JSON-able globals: (values, names skipped).
    fn export_globals(&self, max_bytes: usize) -> (IndexMap<String, String>, Vec<String>) {
        self.ctx.with(|ctx| {
            let Ok(f) = ctx.globals().get::<_, Function>("__exportGlobals") else {
                return (IndexMap::new(), Vec::new());
            };
            let Ok(s) = f.call::<_, String>((max_bytes as f64,)) else {
                return (IndexMap::new(), Vec::new());
            };
            let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
            let values = v
                .get("values")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let skipped = v
                .get("skipped")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (values, skipped)
        })
    }
}

// ---------------------------------------------------------------------- engine

struct Failed {
    agent: String,
    source: String,
}

/// Runs `execute` snippets against a document (see the crate docs).
pub struct ScriptEngine {
    pub limits: Limits,
    globals: HashMap<String, IndexMap<String, String>>,
    failed: HashMap<String, Failed>,
    streams: HashMap<String, Run>,
    parking: Option<(SceneDoc, Pipeline)>,
    ids: IdGenerator,
    id_seed: Option<u64>,
}

impl Default for ScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
fn assert_send() {
    fn is_send<T: Send>() {}
    is_send::<ScriptEngine>();
}

/// Applies find/replace edits to a snippet.
pub fn apply_edits(src: &str, edits: &[Edit]) -> Result<String, String> {
    let mut out = src.to_string();
    for (i, e) in edits.iter().enumerate() {
        if e.find.is_empty() {
            return Err(format!("edit {i}: `find` is empty"));
        }
        let mut find = e.find.clone();
        let mut replace = e.replace.clone();
        let mut count = out.matches(&find).count();
        if count == 0 {
            // Models sometimes send escaped or unescaped newlines.
            for (a, b) in [("\\n", "\n"), ("\n", "\\n")] {
                let f2 = e.find.replace(a, b);
                let c = out.matches(&f2).count();
                if c > 0 {
                    find = f2;
                    replace = e.replace.replace(a, b);
                    count = c;
                    break;
                }
            }
        }
        if count == 0 {
            return Err(format!(
                "edit {i}: `find` text not found in the snippet: {}",
                truncate(&e.find, 120)
            ));
        }
        if count > 1 && !e.all {
            return Err(format!(
                "edit {i}: `find` text matches {count} times; make it unique or set all: true"
            ));
        }
        out = if e.all {
            out.replace(&find, &replace)
        } else {
            out.replacen(&find, &replace, 1)
        };
    }
    Ok(out)
}

impl ScriptEngine {
    pub fn new() -> Self {
        ScriptEngine {
            limits: Limits::default(),
            globals: HashMap::new(),
            failed: HashMap::new(),
            streams: HashMap::new(),
            parking: None,
            ids: IdGenerator::new(),
            id_seed: None,
        }
    }

    pub fn with_limits(limits: Limits) -> Self {
        ScriptEngine {
            limits,
            ..Self::new()
        }
    }

    /// Seeds the node id generator of every run (`None`: random). Two runs of
    /// the same snippet on the same document then create the same ids, which
    /// lets tests compare a streamed and a direct `execute` byte for byte.
    pub fn set_id_seed(&mut self, seed: Option<u64>) {
        self.id_seed = seed;
    }

    /// True while `agent_id` has an open (uncommitted) stream.
    pub fn has_stream(&self, agent_id: &str) -> bool {
        self.streams.contains_key(agent_id)
    }

    /// Drops an agent's open stream **without** reverting it: for when its
    /// document is gone (closed tab). Use [`Self::cancel_stream`] otherwise.
    pub fn discard_stream(&mut self, agent_id: &str) -> bool {
        match self.streams.remove(agent_id) {
            Some(r) => {
                self.park(r);
                true
            }
            None => false,
        }
    }

    /// Globals persisted for an agent (`name -> JSON`).
    pub fn persisted_globals(&self, agent_id: &str) -> Option<&IndexMap<String, String>> {
        self.globals.get(agent_id)
    }

    /// Restores persisted globals (e.g. from a saved chat session).
    pub fn set_persisted_globals(&mut self, agent_id: &str, globals: IndexMap<String, String>) {
        self.globals.insert(agent_id.to_string(), globals);
    }

    /// Forgets an agent's globals, failed snippets and open stream (without
    /// touching the document; call [`Self::cancel_stream`] first if needed).
    pub fn forget_agent(&mut self, agent_id: &str) {
        self.globals.remove(agent_id);
        self.failed.retain(|_, f| f.agent != agent_id);
        self.streams.remove(agent_id);
    }

    fn placeholders(&mut self) -> (SceneDoc, Pipeline) {
        self.parking.take().unwrap_or_else(|| {
            (
                SceneDoc::new(Document::new()),
                Pipeline::new(TextEngine::new(), Box::new(NoAssets)),
            )
        })
    }

    /// Drops a finished run, keeping its placeholder scene/pipeline for reuse.
    fn park(&mut self, run: Run) {
        let Run { ctx, rt, tx, .. } = run;
        // The host function (holding a clone of `tx`) lives in the context.
        drop(ctx);
        drop(rt);
        if let Ok(m) = Arc::try_unwrap(tx) {
            let t = m.into_inner().unwrap_or_else(|e| e.into_inner());
            self.parking = Some((t.scene, t.pipe));
        }
    }

    fn new_run(&mut self, agent: &str) -> Result<Run, String> {
        let (s, p) = self.placeholders();
        Run::new(&self.limits, s, p, self.globals.get(agent), self.id_seed)
    }

    /// Evaluates the complete top-level statements of a snippet that is still
    /// arriving. Changes are applied live (and laid out) but stay uncommitted
    /// until the final [`Self::execute`] of the same agent with the full input
    /// (one undo step), or [`Self::cancel_stream`]. The caller must not edit the
    /// document in between.
    pub fn execute_partial(
        &mut self,
        scene: &mut SceneDoc,
        pipe: &mut Pipeline,
        agent_id: &str,
        partial_input: &str,
    ) -> PartialStatus {
        let mut run = match self.streams.remove(agent_id) {
            Some(r) if partial_input.starts_with(&r.source[..r.done]) => r,
            Some(r) => {
                self.rollback_run(&r, scene, pipe);
                self.park(r);
                match self.new_run(agent_id) {
                    Ok(r) => r,
                    Err(e) => {
                        return PartialStatus {
                            error: Some(e),
                            ..Default::default()
                        };
                    }
                }
            }
            None => match self.new_run(agent_id) {
                Ok(r) => r,
                Err(e) => {
                    return PartialStatus {
                        error: Some(e),
                        ..Default::default()
                    };
                }
            },
        };
        run.source = partial_input.to_string();
        let end = complete_prefix(partial_input).max(run.done);
        run.swap(scene, pipe);
        let r = catch_unwind(AssertUnwindSafe(|| {
            run.eval_to(end);
            lock(&run.tx).layout();
        }));
        run.swap(scene, pipe);
        if r.is_err() {
            self.rollback_run(&run, scene, pipe);
            self.park(run);
            return PartialStatus {
                error: Some("internal error while streaming".into()),
                ..Default::default()
            };
        }
        let (touched, ops) = {
            let g = lock(&run.tx);
            let from = run.reported.min(g.out.touched.len());
            (g.out.touched[from..].to_vec(), g.out.ops)
        };
        run.reported += touched.len();
        let status = PartialStatus {
            evaluated: run.done,
            error: run.error.as_ref().map(|e| e.message.clone()),
            touched,
            ops,
        };
        self.streams.insert(agent_id.to_string(), run);
        status
    }

    /// Reverts and drops an agent's open stream. True if there was one.
    pub fn cancel_stream(
        &mut self,
        scene: &mut SceneDoc,
        pipe: &mut Pipeline,
        agent_id: &str,
    ) -> bool {
        match self.streams.remove(agent_id) {
            Some(r) => {
                self.rollback_run(&r, scene, pipe);
                self.park(r);
                true
            }
            None => false,
        }
    }

    fn rollback_run(&self, run: &Run, scene: &mut SceneDoc, pipe: &mut Pipeline) {
        run.swap(scene, pipe);
        let _ = catch_unwind(AssertUnwindSafe(|| lock(&run.tx).rollback()));
        run.swap(scene, pipe);
    }

    /// Runs a snippet as one transaction: on success its changes become one undo
    /// step in `undo`; on any uncaught error nothing is changed. Synchronous.
    pub fn execute(
        &mut self,
        scene: &mut SceneDoc,
        pipe: &mut Pipeline,
        undo: &mut UndoStack,
        host: &mut dyn ScriptHost,
        req: ExecuteRequest,
    ) -> ExecuteResponse {
        let started = Instant::now();
        let fail = |text: String, edit_id: Option<String>| ExecuteResponse {
            ok: false,
            error: Some(ScriptError {
                message: text.clone(),
                line: None,
                column: None,
                call: None,
            }),
            text,
            edit_id,
            duration: started.elapsed(),
            ..Default::default()
        };
        // 1. The source.
        let (source, edit_id) = match &req.edit_id {
            Some(eid) => {
                let Some(f) = self.failed.get(eid) else {
                    return fail(
                        format!(
                            "unknown editId \"{eid}\" (it expired or the snippet already succeeded); send the full snippet as `input`"
                        ),
                        None,
                    );
                };
                match apply_edits(&f.source, &req.edits) {
                    Ok(s) => (s, Some(eid.clone())),
                    Err(e) => {
                        return fail(
                            format!(
                                "{e}\nNothing was run. The stored snippet is unchanged; fix the edits and retry with editId \"{eid}\"."
                            ),
                            Some(eid.clone()),
                        );
                    }
                }
            }
            None => match &req.input {
                Some(s) => (s.clone(), None),
                None => return fail("`input` (the snippet) is required".into(), None),
            },
        };
        // 2. Context: continue a stream of this agent or start fresh.
        let (run, mut continued) = match self.streams.remove(&req.agent_id) {
            Some(r) if edit_id.is_none() && source.starts_with(&r.source[..r.done]) => {
                (Ok(r), true)
            }
            Some(r) => {
                self.rollback_run(&r, scene, pipe);
                self.park(r);
                (self.new_run(&req.agent_id), false)
            }
            None => (self.new_run(&req.agent_id), false),
        };
        let mut run = match run {
            Ok(r) => r,
            Err(e) => return fail(e, None),
        };
        // 3. Evaluate, render, commit or roll back. A streamed run that fails
        // (already rolled back) is retried once from scratch: a chunk split at
        // a false statement boundary must not fail a snippet that is valid as
        // a whole, and a real error is then reported exactly like a direct
        // call.
        let outcome = loop {
            run.source = source.clone();
            run.swap(scene, pipe);
            let outcome = catch_unwind(AssertUnwindSafe(|| self.finish(&mut run, host)));
            let outcome = match outcome {
                Ok(o) => o,
                Err(_) => {
                    let _ = catch_unwind(AssertUnwindSafe(|| lock(&run.tx).rollback()));
                    Err(ScriptError {
                        message: "internal error (the call was rolled back)".into(),
                        line: None,
                        column: None,
                        call: None,
                    })
                }
            };
            run.swap(scene, pipe);
            if outcome.is_err() && continued {
                continued = false;
                self.park(run);
                run = match self.new_run(&req.agent_id) {
                    Ok(r) => r,
                    Err(e) => return fail(e, None),
                };
                continue;
            }
            break outcome;
        };

        let mut resp = ExecuteResponse::default();
        match outcome {
            Ok(images) => {
                let mut g = lock(&run.tx);
                let inverses = std::mem::take(&mut g.inverses);
                let out = std::mem::take(&mut g.out);
                drop(g);
                undo.record(inverses);
                let (globals, skipped) = run.export_globals(self.limits.max_global_bytes);
                if globals.is_empty() {
                    self.globals.remove(&req.agent_id);
                } else {
                    self.globals.insert(req.agent_id.clone(), globals);
                }
                if let Some(eid) = &edit_id {
                    self.failed.remove(eid);
                }
                let mut warnings = out.warnings;
                for s in skipped {
                    warnings.push(format!(
                        "global '{s}' was not persisted (not JSON-serializable or too large); keep only ids and small values in globals"
                    ));
                }
                resp.ok = true;
                resp.images = images.0;
                resp.exported = images.1;
                resp.created = out.created;
                resp.prints = out.prints;
                resp.warnings = warnings;
                resp.touched = out.touched;
                resp.ops = out.ops;
                resp.text = success_text(&resp);
            }
            Err(err) => {
                let eid = edit_id.unwrap_or_else(|| format!("e{}", self.ids.next_id()));
                self.failed.insert(
                    eid.clone(),
                    Failed {
                        agent: req.agent_id.clone(),
                        source: run.source.clone(),
                    },
                );
                // Keep the store small.
                if self.failed.len() > 64
                    && let Some(k) = self.failed.keys().find(|k| **k != eid).cloned()
                {
                    self.failed.remove(&k);
                }
                resp.ok = false;
                resp.text = failure_text(&err, &eid);
                resp.error = Some(err);
                resp.edit_id = Some(eid);
            }
        }
        self.park(run);
        resp.duration = started.elapsed();
        resp
    }

    /// With the document attached to `run`: evaluates the rest of the snippet and
    /// renders screenshots/exports; rolls back on error.
    #[allow(clippy::type_complexity)]
    fn finish(
        &mut self,
        run: &mut Run,
        host: &mut dyn ScriptHost,
    ) -> Result<(Vec<Vec<u8>>, Vec<PathBuf>), ScriptError> {
        let end = run.source.len();
        run.eval_to(end);
        if let Some(e) = run.error.take() {
            lock(&run.tx).rollback();
            return Err(e);
        }
        let mut g = lock(&run.tx);
        g.layout();
        let mut images = Vec::new();
        let mut exported = Vec::new();
        let err = |message: String| ScriptError {
            message,
            line: None,
            column: None,
            call: None,
        };
        let shots = std::mem::take(&mut g.out.shots);
        let exports = std::mem::take(&mut g.out.exports);
        let tx = &mut *g;
        let result = (|| {
            for id in &shots {
                let t = target_of(&tx.scene, id).map_err(err)?;
                let png = host
                    .screenshot(&tx.scene, &mut tx.pipe, &t)
                    .map_err(|e| err(format!("TakeScreenshot([\"{id}\"]) failed: {e}")))?;
                images.push(png);
            }
            for x in &exports {
                let targets = x
                    .ids
                    .iter()
                    .map(|id| target_of(&tx.scene, id))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(err)?;
                let req = ExportRequest {
                    targets,
                    format: x.format,
                    output: x.output.clone(),
                    scale: x.scale,
                    quality: x.quality,
                };
                let files = host
                    .export(&tx.scene, &mut tx.pipe, &req)
                    .map_err(|e| err(format!("Export failed: {e}")))?;
                exported.extend(files);
            }
            Ok(())
        })();
        if let Err(e) = result {
            tx.rollback();
            return Err(e);
        }
        Ok((images, exported))
    }
}

fn target_of(scene: &SceneDoc, id: &str) -> Result<RenderTarget, String> {
    if is_root(id) {
        return Ok(RenderTarget::Document);
    }
    let key = scene
        .find(id)
        .ok_or_else(|| format!("node not found: '{id}'"))?;
    Ok(RenderTarget::Node {
        id: id.to_string(),
        key,
    })
}

fn success_text(r: &ExecuteResponse) -> String {
    let mut t = String::new();
    if r.ops == 0 {
        t.push_str("Done (no document changes).\n");
    } else {
        t.push_str(&format!(
            "Done: {} document operation(s), one undo step.\n",
            r.ops
        ));
    }
    if !r.created.is_empty() {
        t.push_str("\nCreated nodes (name -> id):\n");
        for (name, id) in &r.created {
            t.push_str(&format!("- {name}: {id}\n"));
        }
    }
    if !r.prints.is_empty() {
        t.push_str("\nPrint output:\n");
        for p in &r.prints {
            t.push_str(p);
            t.push('\n');
        }
    }
    if !r.warnings.is_empty() {
        t.push_str("\nWarnings (fix them in the next execute call):\n");
        for w in &r.warnings {
            t.push_str(&format!("- {w}\n"));
        }
    }
    if !r.images.is_empty() {
        t.push_str(&format!("\n{} screenshot(s) attached.\n", r.images.len()));
    }
    if !r.exported.is_empty() {
        t.push_str("\nExported files:\n");
        for p in &r.exported {
            t.push_str(&format!("- {}\n", p.display()));
        }
    }
    t.truncate(t.trim_end().len());
    t
}

fn failure_text(e: &ScriptError, edit_id: &str) -> String {
    let mut t = match (e.line, e.column) {
        (Some(l), Some(c)) => format!("Error at line {l}, column {c}: {}\n", e.message),
        _ => format!("Error: {}\n", e.message),
    };
    if let Some(call) = &e.call {
        t.push_str(&format!("    {call}\n"));
    }
    t.push_str("All changes of this call were rolled back (nothing was applied).\n");
    t.push_str(&format!(
        "Fix it by calling execute with editId \"{edit_id}\" and `edits: [{{find, replace}}]` instead of resending the snippet."
    ));
    t
}
