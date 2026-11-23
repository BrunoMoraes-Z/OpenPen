//! Streaming `execute`: the edits of a script appear on the canvas while the
//! agent is still writing it (docs/streaming-execute.md).
//!
//! The chat (or a debug hook) reports the growing source of an `execute`
//! call with `stream_partial`; the [`StreamCoordinator`] keeps the latest
//! prefix of each stream and, at most once per frame, feeds the complete
//! statements to `ScriptEngine::execute_partial` on the target document. The
//! final `execute` of the same tool call (matched by tool use id, else by
//! agent) finalizes the stream: the rest of the snippet runs in the same
//! context and everything becomes **one** undo step, or everything is rolled
//! back.
//!
//! This module has no GPUI: the app plugs in documents through
//! [`StreamHost`] (and tests through [`LocalDocs`]). Time is passed in, so the
//! throttling and the timeouts are deterministic under test.
//!
//! Rules while a stream holds a document:
//! - user edits, undo and redo on it are refused (`SceneDocument` lock); a
//!   user gesture that was already open makes the partials wait;
//! - another agent's partials wait (its stream starts when the document is
//!   free) and another agent's `execute` is parked until the stream ends;
//! - 60 s without a new prefix cancels (rolls back) the stream; closing the
//!   document or disconnecting the agent cancels it too.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use pen_script::{ExecuteRequest, ExecuteResponse, PartialStatus, ScriptEngine, complete_prefix};

use crate::scene_document::SceneDocument;

/// Throttling and timeouts.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Minimum time between two evaluations of a stream (one frame).
    pub min_interval: Duration,
    /// The next evaluation waits at least `cost_factor` × the last one's
    /// cost: slow chunks coalesce more, so the UI thread keeps most frames.
    pub cost_factor: u32,
    /// Upper bound of that adaptive interval.
    pub max_interval: Duration,
    /// A stream without a new prefix for this long is cancelled.
    pub idle_timeout: Duration,
    /// Retry delay while the document is not ready (loading, user gesture).
    pub retry: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        StreamConfig {
            min_interval: Duration::from_millis(16),
            cost_factor: 3,
            max_interval: Duration::from_millis(250),
            idle_timeout: Duration::from_secs(60),
            retry: Duration::from_millis(20),
        }
    }
}

/// Where a stream stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Another agent's stream holds the document: nothing evaluated yet.
    Waiting,
    /// Holds the document; its complete statements are applied live.
    Live,
    /// A chunk failed: its changes were rolled back right away and nothing
    /// more is evaluated. The final `execute` runs the full snippet afresh.
    Failed,
}

/// One streamed `execute` call.
#[derive(Debug, Clone)]
pub struct Stream<D> {
    /// Engine agent id (globals and the engine's stream are keyed by it).
    pub agent: String,
    pub tool_use_id: String,
    pub doc: D,
    pub phase: Phase,
    /// Latest source prefix received.
    latest: String,
    /// The part of the source the engine has evaluated.
    done: String,
    /// Touched entries already flashed.
    pub flashed: usize,
    /// Uncommitted document operations applied so far.
    pub ops: usize,
    pub error: Option<String>,
    last_input: Instant,
    next_eval: Instant,
    /// Evaluations and their total / worst cost (diagnostics).
    pub evals: usize,
    pub eval_time: Duration,
    pub max_eval: Duration,
}

impl<D> Stream<D> {
    /// Bytes of the source evaluated so far.
    pub fn evaluated(&self) -> usize {
        self.done.len()
    }

    /// Latest prefix received.
    pub fn latest(&self) -> &str {
        &self.latest
    }
}

/// The documents a coordinator works on.
pub trait StreamHost {
    type Doc: Clone + PartialEq;
    /// `Some(true)` when a partial can run now (loaded, no user gesture);
    /// `None` when the document is gone.
    fn ready(&mut self, doc: &Self::Doc) -> Option<bool>;
    /// `SceneDocument::stream_partial` plus the UI refresh and the flashes
    /// of `status.touched`. `None` when the document is gone.
    fn partial(
        &mut self,
        doc: &Self::Doc,
        engine: &mut ScriptEngine,
        agent: &str,
        prefix: &str,
    ) -> Option<PartialStatus>;
    /// `SceneDocument::stream_cancel` plus the UI refresh. False when the
    /// document is gone (the engine's stream is then discarded unreverted).
    fn cancel(&mut self, doc: &Self::Doc, engine: &mut ScriptEngine, agent: &str) -> bool;
    /// `SceneDocument::run_script`, flashing `resp.touched[skip_touched..]`
    /// (the rest flashed live). `None` when the document is gone.
    fn execute(
        &mut self,
        doc: &Self::Doc,
        engine: &mut ScriptEngine,
        req: ExecuteRequest,
        skip_touched: usize,
    ) -> Option<ExecuteResponse>;
}

/// Result of [`StreamCoordinator::execute`].
#[derive(Debug)]
pub enum Outcome {
    /// The call ran (streamed or not); the text is what the agent gets.
    Done {
        resp: ExecuteResponse,
        /// It finalized a stream.
        streamed: bool,
    },
    /// Another agent's stream holds the document: retry the request (as
    /// given back) after a stream ends.
    Parked(ExecuteRequest),
    /// The document is gone.
    Gone,
}

/// Owns the script engine and the open streams (see the module docs).
pub struct StreamCoordinator<D> {
    pub engine: ScriptEngine,
    pub config: StreamConfig,
    /// In arrival order: an earlier stream gets a contended document first.
    streams: Vec<Stream<D>>,
    /// MCP client id -> engine agent id, set when a client finalizes another
    /// agent's stream (Claude Code CLI: partials from the chat, final call
    /// over MCP), so its later calls share that agent's globals.
    aliases: HashMap<String, String>,
    /// Tool use ids whose stream ended or whose final call already ran
    /// (most recent last): a partial that arrives late for one of them
    /// (the chat forwards deltas on the UI thread, the final call comes over
    /// MCP) must not open a new stream.
    finished: VecDeque<String>,
    log: Vec<String>,
}

/// How many finished tool use ids are remembered.
const FINISHED_CAP: usize = 256;

impl<D: Clone + PartialEq> StreamCoordinator<D> {
    pub fn new(engine: ScriptEngine) -> Self {
        StreamCoordinator {
            engine,
            config: StreamConfig::default(),
            streams: Vec::new(),
            aliases: HashMap::new(),
            finished: VecDeque::new(),
            log: Vec::new(),
        }
    }

    fn mark_finished(&mut self, tool_use_id: &str) {
        if self.finished.iter().any(|t| t == tool_use_id) {
            return;
        }
        if self.finished.len() >= FINISHED_CAP {
            self.finished.pop_front();
        }
        self.finished.push_back(tool_use_id.to_string());
    }

    pub fn streams(&self) -> &[Stream<D>] {
        &self.streams
    }

    /// Lines for the app log, oldest first.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    /// The engine agent id of an MCP client.
    pub fn engine_agent(&self, client: &str) -> String {
        self.aliases
            .get(client)
            .cloned()
            .unwrap_or_else(|| client.to_string())
    }

    /// True while a stream holds `doc`.
    pub fn holds(&self, doc: &D) -> bool {
        self.streams
            .iter()
            .any(|s| s.phase == Phase::Live && s.doc == *doc)
    }

    fn doc_free_for(&self, i: usize) -> bool {
        let doc = &self.streams[i].doc;
        !self
            .streams
            .iter()
            .enumerate()
            .any(|(j, s)| j != i && s.phase == Phase::Live && s.doc == *doc)
    }

    /// Records the latest source prefix of a streamed call. Evaluation
    /// happens in [`Self::pump`]. A different tool call (or document) of the
    /// same agent supersedes (cancels) its previous stream.
    pub fn partial<H: StreamHost<Doc = D>>(
        &mut self,
        host: &mut H,
        agent: &str,
        tool_use_id: &str,
        doc: D,
        prefix: String,
        now: Instant,
    ) {
        if self.finished.iter().any(|t| t == tool_use_id) {
            return;
        }
        if let Some(i) = self.streams.iter().position(|s| s.agent == agent) {
            let s = &mut self.streams[i];
            if s.tool_use_id == tool_use_id && s.doc == doc {
                if prefix != s.latest {
                    s.latest = prefix;
                    s.last_input = now;
                }
                return;
            }
            self.end(host, i, "superseded by a new call");
        }
        self.log
            .push(format!("stream {tool_use_id} of '{agent}' opened"));
        self.streams.push(Stream {
            agent: agent.to_string(),
            tool_use_id: tool_use_id.to_string(),
            doc,
            phase: Phase::Waiting,
            latest: prefix,
            done: String::new(),
            flashed: 0,
            ops: 0,
            error: None,
            last_input: now,
            next_eval: now,
            evals: 0,
            eval_time: Duration::ZERO,
            max_eval: Duration::ZERO,
        });
        let i = self.streams.len() - 1;
        if self.doc_free_for(i) {
            self.streams[i].phase = Phase::Live;
        }
    }

    /// Cancels (rolls back) a stream, by tool use id or else by agent.
    pub fn cancel<H: StreamHost<Doc = D>>(
        &mut self,
        host: &mut H,
        agent: &str,
        tool_use_id: Option<&str>,
    ) -> bool {
        let i = tool_use_id
            .and_then(|t| self.streams.iter().position(|s| s.tool_use_id == t))
            .or_else(|| self.streams.iter().position(|s| s.agent == agent));
        match i {
            Some(i) => {
                self.end(host, i, "cancelled");
                true
            }
            None => false,
        }
    }

    /// Cancels every stream on the documents matching `closed`.
    pub fn doc_closed<H: StreamHost<Doc = D>>(
        &mut self,
        host: &mut H,
        closed: impl Fn(&D) -> bool,
    ) {
        while let Some(i) = self.streams.iter().position(|s| closed(&s.doc)) {
            self.end(host, i, "document closed");
        }
    }

    /// An agent disconnected: cancels its streams and forgets it.
    pub fn forget_agent<H: StreamHost<Doc = D>>(&mut self, host: &mut H, agent: &str) {
        while let Some(i) = self.streams.iter().position(|s| s.agent == agent) {
            self.end(host, i, "agent disconnected");
        }
        self.aliases.remove(agent);
        self.engine.forget_agent(agent);
    }

    fn end<H: StreamHost<Doc = D>>(&mut self, host: &mut H, i: usize, why: &str) {
        let s = self.streams.remove(i);
        self.mark_finished(&s.tool_use_id);
        if s.phase == Phase::Live && !host.cancel(&s.doc, &mut self.engine, &s.agent) {
            self.engine.discard_stream(&s.agent);
        }
        self.log.push(format!(
            "stream {} of '{}' {why} ({} evaluation(s), {} op(s) rolled back)",
            s.tool_use_id,
            s.agent,
            s.evals,
            if s.phase == Phase::Live { s.ops } else { 0 }
        ));
    }

    /// When [`Self::pump`] next has work (an evaluation that is due, a
    /// retry or a timeout); `None` without streams.
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        self.streams
            .iter()
            .map(|s| {
                let timeout =
                    (s.last_input + self.config.idle_timeout).saturating_duration_since(now);
                let pending = s.phase != Phase::Failed && complete_prefix(&s.latest) > s.done.len();
                if pending && s.phase == Phase::Live {
                    timeout.min(s.next_eval.saturating_duration_since(now))
                } else {
                    timeout
                }
            })
            .min()
    }

    /// Times streams out, starts waiting streams whose document is free, and
    /// evaluates the streams whose complete prefix grew and that are due.
    pub fn pump<H: StreamHost<Doc = D>>(&mut self, host: &mut H, now: Instant) {
        let mut i = 0;
        while i < self.streams.len() {
            if now.saturating_duration_since(self.streams[i].last_input) >= self.config.idle_timeout
            {
                self.end(host, i, "timed out");
            } else {
                i += 1;
            }
        }
        let mut i = 0;
        while i < self.streams.len() {
            if self.streams[i].phase == Phase::Waiting && self.doc_free_for(i) {
                self.streams[i].phase = Phase::Live;
                self.streams[i].next_eval = now;
            }
            if self.eval(host, i, now) {
                i += 1;
            }
        }
    }

    /// Evaluates stream `i` if it is due. False if it was removed.
    fn eval<H: StreamHost<Doc = D>>(&mut self, host: &mut H, i: usize, now: Instant) -> bool {
        let s = &self.streams[i];
        if s.phase != Phase::Live || now < s.next_eval {
            return true;
        }
        let boundary = complete_prefix(&s.latest);
        let restart = !s.latest.starts_with(s.done.as_str());
        if boundary <= s.done.len() && !restart {
            // Nothing new is complete: never re-evaluate the same prefix.
            return true;
        }
        match host.ready(&s.doc) {
            None => {
                self.end(host, i, "document closed");
                return false;
            }
            Some(false) => {
                self.streams[i].next_eval = now + self.config.retry;
                return true;
            }
            Some(true) => {}
        }
        let doc = s.doc.clone();
        let agent = s.agent.clone();
        let t0 = Instant::now();
        let status = host.partial(&doc, &mut self.engine, &agent, &self.streams[i].latest);
        let cost = t0.elapsed();
        let Some(status) = status else {
            self.end(host, i, "document closed");
            return false;
        };
        let cfg = &self.config;
        let s = &mut self.streams[i];
        if restart {
            s.flashed = 0;
        }
        s.done = s.latest[..status.evaluated.min(s.latest.len())].to_string();
        s.flashed += status.touched.len();
        s.ops = status.ops;
        s.evals += 1;
        s.eval_time += cost;
        s.max_eval = s.max_eval.max(cost);
        let wait = (cost * cfg.cost_factor).clamp(cfg.min_interval, cfg.max_interval);
        s.next_eval = now + wait;
        if let Some(e) = status.error {
            // Show the failure right away: revert what the stream applied.
            // The final call reruns the whole snippet and reports the error.
            if !host.cancel(&doc, &mut self.engine, &agent) {
                self.engine.discard_stream(&agent);
            }
            let s = &mut self.streams[i];
            s.phase = Phase::Failed;
            self.log.push(format!(
                "stream {} of '{}' failed, rolled back: {e}",
                s.tool_use_id, s.agent
            ));
            s.error = Some(e);
        }
        true
    }

    /// Runs an `execute` of MCP client `client` on `doc`. If it is the final
    /// call of a stream (same tool use id, else same agent) on that document
    /// whose source starts with what was evaluated, it finalizes the stream
    /// (one undo step, or a full rollback). Otherwise a matching stream is
    /// cancelled first and the call runs as usual. While another agent's
    /// stream holds `doc` the request is parked.
    pub fn execute<H: StreamHost<Doc = D>>(
        &mut self,
        host: &mut H,
        client: &str,
        tool_use_id: Option<&str>,
        doc: &D,
        mut req: ExecuteRequest,
    ) -> Outcome {
        let engine_agent = self.engine_agent(client);
        if let Some(t) = tool_use_id
            && !self.streams.iter().any(|s| s.tool_use_id == t)
        {
            // Final call without (or before) its partials.
            self.mark_finished(t);
        }
        let found = tool_use_id
            .and_then(|t| self.streams.iter().position(|s| s.tool_use_id == t))
            .or_else(|| self.streams.iter().position(|s| s.agent == engine_agent));
        if let Some(i) = found {
            let s = &self.streams[i];
            let continues = s.phase == Phase::Live
                && s.doc == *doc
                && req.edit_id.is_none()
                && req
                    .input
                    .as_deref()
                    .is_some_and(|src| src.starts_with(s.done.as_str()));
            if continues {
                let s = self.streams.remove(i);
                self.mark_finished(&s.tool_use_id);
                if s.agent != client {
                    self.aliases.insert(client.to_string(), s.agent.clone());
                }
                req.agent_id = s.agent.clone();
                let resp = host.execute(doc, &mut self.engine, req, s.flashed);
                self.log.push(format!(
                    "stream {} of '{}' finalized after {} evaluation(s) ({:.1?} total, {:.1?} worst): {}",
                    s.tool_use_id,
                    s.agent,
                    s.evals,
                    s.eval_time,
                    s.max_eval,
                    match &resp {
                        Some(r) if r.ok => "committed",
                        Some(_) => "rolled back",
                        None => "document gone",
                    }
                ));
                return match resp {
                    Some(resp) => Outcome::Done {
                        resp,
                        streamed: true,
                    },
                    None => {
                        self.engine.discard_stream(&s.agent);
                        Outcome::Gone
                    }
                };
            }
            let why = if s.phase != Phase::Live {
                "replaced by the final call"
            } else if s.doc != *doc {
                "cancelled: the final call targets another document"
            } else {
                "cancelled: the final call does not continue the streamed source"
            };
            self.end(host, i, why);
        }
        // The engine must not continue a stream of this agent elsewhere.
        while let Some(i) = self.streams.iter().position(|s| s.agent == engine_agent) {
            self.end(host, i, "cancelled by another call of the agent");
        }
        req.agent_id = engine_agent;
        if self.holds(doc) {
            return Outcome::Parked(req);
        }
        match host.execute(doc, &mut self.engine, req, 0) {
            Some(resp) => Outcome::Done {
                resp,
                streamed: false,
            },
            None => Outcome::Gone,
        }
    }
}

/// [`StreamHost`] over plain documents (`Doc` = index; `None` = closed):
/// for tests and windowless use.
pub struct LocalDocs {
    pub docs: Vec<Option<SceneDocument>>,
    /// Pretend a user gesture is open (partials must wait).
    pub busy: bool,
    /// Extra time each partial takes (simulates a slow document).
    pub delay: Duration,
    /// Every touched entry flashed, in order (`(doc, id)`).
    pub flashes: Vec<(usize, String)>,
}

impl LocalDocs {
    pub fn new(docs: Vec<SceneDocument>) -> Self {
        LocalDocs {
            docs: docs.into_iter().map(Some).collect(),
            busy: false,
            delay: Duration::ZERO,
            flashes: Vec::new(),
        }
    }

    pub fn doc(&self, i: usize) -> &SceneDocument {
        self.docs[i].as_ref().expect("document is open")
    }

    pub fn doc_mut(&mut self, i: usize) -> &mut SceneDocument {
        self.docs[i].as_mut().expect("document is open")
    }
}

impl StreamHost for LocalDocs {
    type Doc = usize;

    fn ready(&mut self, doc: &usize) -> Option<bool> {
        let busy = self.busy;
        self.docs.get(*doc)?.as_ref().map(|_| !busy)
    }

    fn partial(
        &mut self,
        doc: &usize,
        engine: &mut ScriptEngine,
        agent: &str,
        prefix: &str,
    ) -> Option<PartialStatus> {
        let d = self.docs.get_mut(*doc)?.as_mut()?;
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let st = d.stream_partial(engine, agent, prefix);
        self.flashes
            .extend(st.touched.iter().map(|t| (*doc, t.id.clone())));
        Some(st)
    }

    fn cancel(&mut self, doc: &usize, engine: &mut ScriptEngine, agent: &str) -> bool {
        match self.docs.get_mut(*doc).and_then(Option::as_mut) {
            Some(d) => {
                d.stream_cancel(engine, agent);
                true
            }
            None => false,
        }
    }

    fn execute(
        &mut self,
        doc: &usize,
        engine: &mut ScriptEngine,
        req: ExecuteRequest,
        skip_touched: usize,
    ) -> Option<ExecuteResponse> {
        let d = self.docs.get_mut(*doc)?.as_mut()?;
        let resp = d.run_script(engine, &mut super::AppScriptHost, req);
        self.flashes.extend(
            resp.touched
                .iter()
                .skip(skip_touched)
                .map(|t| (*doc, t.id.clone())),
        );
        Some(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "a = Insert(document, {type: \"frame\", name: \"A\", width: 100, height: 100})\nb = Insert(a, {type: \"rectangle\", name: \"B\", width: 10, height: 10})\nc = Insert(document, {type: \"frame\", name: \"C\", x: 200, width: 50, height: 50})\nPrint(Get(b).name)\n";

    fn req(agent: &str, src: &str) -> ExecuteRequest {
        ExecuteRequest {
            agent_id: agent.into(),
            description: "Stream test".into(),
            input: Some(src.into()),
            ..Default::default()
        }
    }

    fn setup(n: usize) -> (StreamCoordinator<usize>, LocalDocs) {
        let mut engine = ScriptEngine::new();
        engine.set_id_seed(Some(7));
        let docs = (0..n).map(|_| SceneDocument::empty(&[])).collect();
        (StreamCoordinator::new(engine), LocalDocs::new(docs))
    }

    fn roots(h: &LocalDocs, i: usize) -> usize {
        h.doc(i).scene.roots().len()
    }

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    #[test]
    fn coalesces_to_one_evaluation_per_frame_and_skips_unchanged_prefixes() {
        let (mut c, mut h) = setup(1);
        // Fixed one-frame interval (the adaptive part is tested below).
        c.config.cost_factor = 0;
        let t = Instant::now();
        let first = SRC.find("\nb").unwrap() + 2; // first statement + a token
        c.partial(&mut h, "chat", "t1", 0, SRC[..first].into(), t);
        c.pump(&mut h, t);
        assert_eq!(c.streams()[0].evals, 1);
        assert_eq!(roots(&h, 0), 1);
        assert_eq!(h.doc(0).stream_lock(), Some("chat"));
        // Three more prefixes inside the same frame: nothing runs yet.
        let second = SRC.find("\nc").unwrap() + 4; // `c` alone could still grow into `catch`
        for (k, cut) in [first + 5, first + 20, second].into_iter().enumerate() {
            c.partial(
                &mut h,
                "chat",
                "t1",
                0,
                SRC[..cut].into(),
                ms(t, 2 + k as u64),
            );
            c.pump(&mut h, ms(t, 2 + k as u64));
        }
        assert_eq!(c.streams()[0].evals, 1);
        let w = c.next_wake(ms(t, 5)).unwrap();
        assert!(w <= Duration::from_millis(16), "{w:?}");
        // Next frame: one evaluation with the latest prefix.
        c.pump(&mut h, ms(t, 17));
        assert_eq!(c.streams()[0].evals, 2);
        assert_eq!(c.streams()[0].evaluated(), second - 3);
        // Same complete prefix again (only an unfinished token grew): no eval.
        c.partial(&mut h, "chat", "t1", 0, SRC[..second + 3].into(), ms(t, 40));
        c.pump(&mut h, ms(t, 40));
        assert_eq!(c.streams()[0].evals, 2);
        // No stream work is pending except the timeout.
        assert!(c.next_wake(ms(t, 40)).unwrap() > Duration::from_secs(50));
        // The touched nodes were flashed live.
        assert!(h.flashes.len() >= 2);
    }

    #[test]
    fn slow_chunks_coalesce_more() {
        let (mut c, mut h) = setup(1);
        h.delay = Duration::from_millis(30);
        let t = Instant::now();
        let first = SRC.find("\nb").unwrap() + 2;
        c.partial(&mut h, "chat", "t1", 0, SRC[..first].into(), t);
        c.pump(&mut h, t);
        assert!(c.streams()[0].max_eval >= Duration::from_millis(30));
        let second = SRC.find("\nc").unwrap() + 4;
        c.partial(&mut h, "chat", "t1", 0, SRC[..second].into(), ms(t, 1));
        // 3 x 30 ms: not due after one or several frames...
        c.pump(&mut h, ms(t, 17));
        c.pump(&mut h, ms(t, 60));
        assert_eq!(c.streams()[0].evals, 1);
        assert!(c.next_wake(ms(t, 60)).unwrap() >= Duration::from_millis(25));
        // ...only after the adaptive interval (3 x the measured cost).
        let cost = c.streams()[0].max_eval;
        c.pump(&mut h, t + cost * 3 - Duration::from_millis(1));
        assert_eq!(c.streams()[0].evals, 1);
        c.pump(&mut h, t + cost * 3);
        assert_eq!(c.streams()[0].evals, 2);
    }

    #[test]
    fn finalize_is_one_undo_step_identical_to_a_direct_execute() {
        // Direct run.
        let (mut c, mut h) = setup(1);
        let direct = match c.execute(&mut h, "chat", Some("t0"), &0, req("chat", SRC)) {
            Outcome::Done { resp, streamed } => {
                assert!(!streamed);
                resp
            }
            o => panic!("{o:?}"),
        };
        let direct_doc = h.doc(0).save_string();

        // Streamed run on a fresh document with the same id seed.
        let (mut c, mut h) = setup(1);
        let before = h.doc(0).save_string();
        let t = Instant::now();
        for (k, cut) in (10..SRC.len()).step_by(17).enumerate() {
            let now = ms(t, 20 * k as u64);
            c.partial(&mut h, "chat", "t1", 0, SRC[..cut].into(), now);
            c.pump(&mut h, now);
        }
        assert!(roots(&h, 0) >= 1);
        assert_eq!(h.doc(0).undo_steps(), 0);
        let resp = match c.execute(&mut h, "client-9", Some("t1"), &0, req("client-9", SRC)) {
            Outcome::Done { resp, streamed } => {
                assert!(streamed);
                resp
            }
            o => panic!("{o:?}"),
        };
        assert!(resp.ok, "{}", resp.text);
        assert_eq!(resp.text, direct.text);
        assert_eq!(h.doc(0).save_string(), direct_doc);
        assert_eq!(h.doc(0).undo_steps(), 1);
        assert_eq!(h.doc(0).stream_lock(), None);
        assert!(c.streams().is_empty());
        // The MCP client is now an alias of the streaming agent.
        assert_eq!(c.engine_agent("client-9"), "chat");
        // One undo reverts the whole call.
        assert!(h.doc_mut(0).undo());
        h.doc_mut(0).flush();
        assert_eq!(h.doc(0).save_string(), before);
    }

    #[test]
    fn mismatched_final_input_cancels_and_runs_normally() {
        let (mut c, mut h) = setup(1);
        let t = Instant::now();
        c.partial(
            &mut h,
            "chat",
            "t1",
            0,
            SRC[..SRC.find("\nc").unwrap() + 2].into(),
            t,
        );
        c.pump(&mut h, t);
        assert!(roots(&h, 0) >= 1);
        let other = "z = Insert(document, {type: \"frame\", name: \"Z\", width: 5, height: 5})";
        match c.execute(&mut h, "chat", Some("t1"), &0, req("chat", other)) {
            Outcome::Done { resp, streamed } => {
                assert!(resp.ok, "{}", resp.text);
                assert!(!streamed);
            }
            o => panic!("{o:?}"),
        }
        // Only Z: the streamed A was rolled back first.
        assert_eq!(roots(&h, 0), 1);
        assert_eq!(h.doc(0).undo_steps(), 1);
        assert!(h.doc(0).save_string().contains("\"Z\""));
        assert!(!h.doc(0).save_string().contains("\"A\""));

        // An editId patch also cancels the stream.
        c.partial(
            &mut h,
            "chat",
            "t2",
            0,
            SRC[..SRC.find("\nb").unwrap() + 2].into(),
            t,
        );
        c.pump(&mut h, t);
        assert_eq!(roots(&h, 0), 2);
        let mut patch = req("chat", "");
        patch.input = None;
        patch.edit_id = Some("e-unknown".into());
        match c.execute(&mut h, "chat", Some("t2"), &0, patch) {
            Outcome::Done { resp, .. } => assert!(!resp.ok),
            o => panic!("{o:?}"),
        }
        assert_eq!(roots(&h, 0), 1);
        assert_eq!(h.doc(0).stream_lock(), None);
    }

    #[test]
    fn idle_stream_times_out_and_rolls_back() {
        let (mut c, mut h) = setup(1);
        let t = Instant::now();
        c.partial(
            &mut h,
            "chat",
            "t1",
            0,
            SRC[..SRC.find("\nb").unwrap() + 2].into(),
            t,
        );
        c.pump(&mut h, t);
        assert_eq!(roots(&h, 0), 1);
        c.pump(&mut h, t + Duration::from_secs(59));
        assert_eq!(c.streams().len(), 1);
        assert!(c.next_wake(t + Duration::from_secs(59)).unwrap() <= Duration::from_secs(1));
        c.pump(&mut h, t + Duration::from_secs(60));
        assert!(c.streams().is_empty());
        assert_eq!(roots(&h, 0), 0);
        assert_eq!(h.doc(0).stream_lock(), None);
        assert!(c.take_log().iter().any(|l| l.contains("timed out")));
        // The late final call runs as a normal execute.
        match c.execute(&mut h, "chat", Some("t1"), &0, req("chat", SRC)) {
            Outcome::Done { resp, streamed } => assert!(resp.ok && !streamed),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn error_mid_stream_rolls_back_everything() {
        let (mut c, mut h) = setup(1);
        let bad = "a = Insert(document, {type: \"frame\", name: \"A\"})\nUpdate(\"nope\", {name: \"x\"})\nb = Insert(document, {type: \"frame\", name: \"B\"})\n";
        let t = Instant::now();
        let cut = bad.find("\nUpdate").unwrap() + 2;
        c.partial(&mut h, "chat", "t1", 0, bad[..cut].into(), t);
        c.pump(&mut h, t);
        assert_eq!(roots(&h, 0), 1);
        let cut = bad.find("\nb =").unwrap() + 2;
        c.partial(&mut h, "chat", "t1", 0, bad[..cut].into(), ms(t, 20));
        c.pump(&mut h, ms(t, 20));
        // The failing chunk reverted the stream at once and unlocked the doc.
        assert_eq!(c.streams()[0].phase, Phase::Failed);
        assert_eq!(roots(&h, 0), 0);
        assert_eq!(h.doc(0).stream_lock(), None);
        // More partials do nothing.
        c.partial(&mut h, "chat", "t1", 0, bad.into(), ms(t, 40));
        c.pump(&mut h, ms(t, 40));
        assert_eq!(roots(&h, 0), 0);
        // The final call reports the error like a direct execute.
        let (mut c2, mut h2) = setup(1);
        let direct = match c2.execute(&mut h2, "chat", None, &0, req("chat", bad)) {
            Outcome::Done { resp, .. } => resp,
            o => panic!("{o:?}"),
        };
        let resp = match c.execute(&mut h, "chat", Some("t1"), &0, req("chat", bad)) {
            Outcome::Done { resp, .. } => resp,
            o => panic!("{o:?}"),
        };
        assert!(!resp.ok);
        assert_eq!(resp.error, direct.error);
        assert_eq!(roots(&h, 0), 0);
        assert_eq!(h.doc(0).undo_steps(), 0);
        assert!(!h.doc(0).is_dirty());
    }

    #[test]
    fn locked_document_refuses_user_edits_and_undo() {
        let (mut c, mut h) = setup(1);
        // A first committed step to have something to undo.
        let _ = c.execute(
            &mut h,
            "chat",
            None,
            &0,
            req(
                "chat",
                "Insert(document, {type: \"frame\", name: \"Base\"})",
            ),
        );
        assert_eq!(h.doc(0).undo_steps(), 1);
        let t = Instant::now();
        c.partial(
            &mut h,
            "chat",
            "t1",
            0,
            SRC[..SRC.find("\nb").unwrap() + 2].into(),
            t,
        );
        c.pump(&mut h, t);
        let d = h.doc_mut(0);
        assert_eq!(d.stream_lock(), Some("chat"));
        assert!(!d.undo());
        assert!(!d.redo());
        let root = d.scene.roots()[0];
        assert!(!d.rename_key(root, "Renamed by the user"));
        // Cancel: unlocked, the stream is gone, undo works again.
        assert!(c.cancel(&mut h, "chat", Some("t1")));
        let d = h.doc_mut(0);
        assert_eq!(d.stream_lock(), None);
        assert_eq!(d.scene.roots().len(), 1);
        assert!(d.undo());
    }

    #[test]
    fn user_gesture_makes_partials_wait() {
        let (mut c, mut h) = setup(1);
        h.busy = true;
        let t = Instant::now();
        c.partial(
            &mut h,
            "chat",
            "t1",
            0,
            SRC[..SRC.find("\nb").unwrap() + 2].into(),
            t,
        );
        c.pump(&mut h, t);
        assert_eq!(c.streams()[0].evals, 0);
        assert_eq!(roots(&h, 0), 0);
        assert_eq!(c.next_wake(t), Some(c.config.retry));
        h.busy = false;
        c.pump(&mut h, ms(t, 20));
        assert_eq!(roots(&h, 0), 1);
    }

    #[test]
    fn second_agent_is_serialized() {
        let (mut c, mut h) = setup(1);
        let t = Instant::now();
        let a = "x = Insert(document, {type: \"frame\", name: \"FromA\"})\ny = 1\n";
        let b = "x = Insert(document, {type: \"frame\", name: \"FromB\"})\ny = 2\n";
        c.partial(&mut h, "agent-a", "ta", 0, a.into(), t);
        c.partial(&mut h, "agent-b", "tb", 0, b.into(), t);
        c.pump(&mut h, t);
        assert_eq!(c.streams()[0].phase, Phase::Live);
        assert_eq!(c.streams()[1].phase, Phase::Waiting);
        assert_eq!(roots(&h, 0), 1);
        // A third agent's plain execute is parked.
        let parked = match c.execute(
            &mut h,
            "agent-c",
            None,
            &0,
            req(
                "agent-c",
                "Insert(document, {type: \"frame\", name: \"FromC\"})",
            ),
        ) {
            Outcome::Parked(r) => r,
            o => panic!("{o:?}"),
        };
        // A finalizes; B starts on the next pump; C can run.
        match c.execute(&mut h, "agent-a", Some("ta"), &0, req("agent-a", a)) {
            Outcome::Done { resp, streamed } => assert!(resp.ok && streamed),
            o => panic!("{o:?}"),
        }
        c.pump(&mut h, ms(t, 1));
        assert_eq!(c.streams()[0].phase, Phase::Live);
        assert_eq!(roots(&h, 0), 2);
        assert!(matches!(
            c.execute(&mut h, "agent-c", None, &0, parked.clone()),
            Outcome::Parked(_)
        ));
        match c.execute(&mut h, "agent-b", Some("tb"), &0, req("agent-b", b)) {
            Outcome::Done { resp, streamed } => assert!(resp.ok && streamed),
            o => panic!("{o:?}"),
        }
        match c.execute(&mut h, "agent-c", None, &0, parked) {
            Outcome::Done { resp, .. } => assert!(resp.ok),
            o => panic!("{o:?}"),
        }
        assert_eq!(roots(&h, 0), 3);
        assert_eq!(h.doc(0).undo_steps(), 3);
    }

    #[test]
    fn closed_document_cancels_and_final_targets_elsewhere() {
        let (mut c, mut h) = setup(2);
        let t = Instant::now();
        c.partial(
            &mut h,
            "chat",
            "t1",
            0,
            SRC[..SRC.find("\nb").unwrap() + 2].into(),
            t,
        );
        c.pump(&mut h, t);
        assert_eq!(roots(&h, 0), 1);
        // Final call on another document: the stream is rolled back there.
        match c.execute(&mut h, "chat", Some("t1"), &1, req("chat", SRC)) {
            Outcome::Done { resp, streamed } => assert!(resp.ok && !streamed),
            o => panic!("{o:?}"),
        }
        assert_eq!(roots(&h, 0), 0);
        assert_eq!(roots(&h, 1), 2);
        // A closed document (gone) discards the engine's stream.
        c.partial(
            &mut h,
            "chat",
            "t2",
            0,
            SRC[..SRC.find("\nb").unwrap() + 2].into(),
            t,
        );
        c.pump(&mut h, t);
        h.docs[0] = None;
        c.doc_closed(&mut h, |d| *d == 0);
        assert!(c.streams().is_empty());
        assert!(!c.engine.has_stream("chat"));
    }

    #[test]
    fn late_partials_after_the_final_call_are_ignored() {
        let (mut c, mut h) = setup(1);
        let t = Instant::now();
        // The final call arrived over MCP before the chat forwarded the
        // partials of the same tool call: it runs normally...
        match c.execute(&mut h, "client-3", Some("t1"), &0, req("client-3", SRC)) {
            Outcome::Done { resp, streamed } => assert!(resp.ok && !streamed),
            o => panic!("{o:?}"),
        }
        assert_eq!(h.doc(0).undo_steps(), 1);
        // ...and the late partials do not open a stream (no lock, no
        // duplicated nodes).
        c.partial(&mut h, "chat", "t1", 0, SRC[..40].into(), t);
        c.pump(&mut h, t);
        assert!(c.streams().is_empty());
        assert_eq!(h.doc(0).stream_lock(), None);
        assert_eq!(roots(&h, 0), 2);
        // Same after a finalized or cancelled stream.
        let cut = SRC
            .find(
                "
b",
            )
            .unwrap()
            + 2;
        c.partial(&mut h, "chat", "t2", 0, SRC[..cut].into(), t);
        c.pump(&mut h, t);
        assert!(c.cancel(&mut h, "chat", Some("t2")));
        c.partial(&mut h, "chat", "t2", 0, SRC.into(), ms(t, 20));
        assert!(c.streams().is_empty());
        // A new tool call still streams.
        c.partial(&mut h, "chat", "t3", 0, SRC[..cut].into(), ms(t, 40));
        assert_eq!(c.streams().len(), 1);
    }
}
