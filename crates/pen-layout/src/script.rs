//! `script` nodes: expansion into generated subtrees.
//!
//! A script node references a `.js` file that returns an array of `.pen` nodes. The
//! JavaScript engine is not part of this crate: a [`ScriptRunner`] (implemented by
//! `pen-script` on QuickJS) is plugged into the [`crate::Pipeline`]. After each layout
//! pass, [`ScriptExpander::sync`] runs the scripts whose source file, inputs or laid-out
//! size changed and attaches the output to the scene with
//! [`SceneDoc::set_generated`] (never saved, not editable). Results are cached by
//! `(source hash, inputs, width, height)`, so undo/redo, rebuilding a root or going back
//! to a previous size re-attaches without running JavaScript again.

use pen_doc::{Changes, Kind, KindData, NodeKey, ParamValue, SceneDoc};
use pen_schema::Node;
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::layout::LayoutEngine;

/// One script evaluation.
#[derive(Debug, Clone, Copy)]
pub struct ScriptRequest<'a> {
    /// `scriptUri` as written in the node (for messages).
    pub uri: &'a str,
    /// Content of the script file.
    pub source: &'a str,
    /// The node's `inputs` (variables resolved), in file order.
    pub inputs: &'a [(Arc<str>, ParamValue)],
    /// Laid-out size of the script node.
    pub width: f32,
    pub height: f32,
}

/// Why a script produced no output. Drawn inside the node as an error placeholder.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptError {
    /// Short title ("Script runtime error", "Script not found", ...).
    pub title: String,
    /// Details (message, stack, offending line).
    pub description: Option<String>,
}

impl ScriptError {
    pub fn new(title: impl Into<String>, description: Option<String>) -> Self {
        Self {
            title: title.into(),
            description,
        }
    }

    /// Title plus the first lines of the description.
    pub fn message(&self) -> String {
        match &self.description {
            Some(d) if !d.trim().is_empty() => {
                let d: Vec<&str> = d.lines().take(4).collect();
                format!("{}\n{}", self.title, d.join("\n"))
            }
            _ => self.title.clone(),
        }
    }
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ScriptError {}

/// Evaluates script files. Implementations must be deterministic for a given request
/// (results are cached).
pub trait ScriptRunner: Send {
    /// Runs the script and returns its output nodes (migrated to the current schema,
    /// without ids).
    fn run(&mut self, req: &ScriptRequest<'_>) -> Result<Vec<Node>, ScriptError>;
}

type Output = Arc<Result<Arc<[Node]>, ScriptError>>;

/// Source file cache entry.
struct Source {
    modified: Option<SystemTime>,
    len: u64,
    text: Arc<str>,
    hash: u64,
}

/// Counters of [`ScriptExpander::sync`] (for tests and benches).
#[derive(Debug, Default, Clone, Copy)]
pub struct ScriptStats {
    /// Script evaluations (cache misses).
    pub runs: u64,
    /// Outputs re-attached from the cache.
    pub reused: u64,
    /// Time spent in [`ScriptRunner::run`].
    pub run_time: Duration,
}

/// Max cached outputs.
const CACHE_CAP: usize = 128;

/// Keeps script outputs in sync with the scene (see the module docs).
pub struct ScriptExpander {
    runner: Box<dyn ScriptRunner>,
    sources: HashMap<PathBuf, Source>,
    cache: HashMap<u64, Output>,
    order: VecDeque<u64>,
    pub stats: ScriptStats,
}

impl ScriptExpander {
    pub fn new(runner: Box<dyn ScriptRunner>) -> Self {
        Self {
            runner,
            sources: HashMap::new(),
            cache: HashMap::new(),
            order: VecDeque::new(),
            stats: ScriptStats::default(),
        }
    }

    /// Forgets cached files and outputs (the next sync re-reads every script).
    pub fn clear_cache(&mut self) {
        self.sources.clear();
        self.cache.clear();
        self.order.clear();
    }

    /// Runs/attaches the scripts affected by a change set: every script on a full
    /// rebuild, the scripts inside relaid roots and dirty script nodes otherwise.
    /// Returns true if the scene changed (its new change set must be laid out).
    pub fn sync(
        &mut self,
        scene: &mut SceneDoc,
        layout: &LayoutEngine,
        changes: &Changes,
        relaid: &[NodeKey],
    ) -> bool {
        let mut cands: Vec<NodeKey> = Vec::new();
        let roots: &[NodeKey] = if changes.full { scene.roots() } else { relaid };
        for &r in roots {
            collect_scripts(scene, r, &mut cands);
        }
        if !changes.full {
            for (k, _) in &changes.dirty {
                if scene.get(*k).is_some_and(|n| n.kind == Kind::Script) {
                    cands.push(*k);
                }
            }
        }
        cands.sort();
        cands.dedup();
        let mut changed = false;
        for k in cands {
            changed |= self.sync_node(scene, layout, k);
        }
        changed
    }

    /// Re-checks every script of the scene (e.g. after files changed on disk).
    pub fn sync_all(&mut self, scene: &mut SceneDoc, layout: &LayoutEngine) -> bool {
        self.sources.clear();
        let mut cands = Vec::new();
        for &r in scene.roots() {
            collect_scripts(scene, r, &mut cands);
        }
        let mut changed = false;
        for k in cands {
            changed |= self.sync_node(scene, layout, k);
        }
        changed
    }

    fn sync_node(&mut self, scene: &mut SceneDoc, layout: &LayoutEngine, k: NodeKey) -> bool {
        let Some(n) = scene.get(k) else { return false };
        // Scripts inside generated output (a component instantiated by a script) are not
        // expanded: no recursion.
        if n.kind != Kind::Script || scene.generated_owner(k).is_some() {
            return false;
        }
        let KindData::Script { uri, inputs } = &n.data else {
            return false;
        };
        let (uri, inputs) = (uri.clone(), inputs.clone());
        let Some(l) = layout.get(k) else {
            return false;
        };
        let (w, h) = (l.w, l.h);
        if uri.trim().is_empty() {
            if scene.generated_tag(k).is_some() {
                scene.clear_generated(k);
                return true;
            }
            return false;
        }
        let path = resolve_path(scene.base_dir(), &uri);
        let source = self.source(&path);
        let mut hs = DefaultHasher::new();
        match &source {
            Ok(s) => s.1.hash(&mut hs),
            Err(_) => path.hash(&mut hs),
        }
        hash_params(&inputs, &mut hs);
        w.to_bits().hash(&mut hs);
        h.to_bits().hash(&mut hs);
        let key = hs.finish();
        if scene.generated_tag(k) == Some(key) {
            return false;
        }
        let out = match self.cache.get(&key) {
            Some(o) => {
                self.stats.reused += 1;
                o.clone()
            }
            None => {
                let out: Output = Arc::new(match &source {
                    Err(e) => Err(e.clone()),
                    Ok((text, _)) => {
                        let t0 = Instant::now();
                        let r = self.runner.run(&ScriptRequest {
                            uri: &uri,
                            source: text,
                            inputs: &inputs,
                            width: w,
                            height: h,
                        });
                        self.stats.runs += 1;
                        self.stats.run_time += t0.elapsed();
                        r.map(Arc::from)
                    }
                });
                if source.is_ok() {
                    self.insert(key, out.clone());
                }
                out
            }
        };
        match &*out {
            Ok(nodes) => scene.set_generated(k, nodes, key),
            Err(e) => scene.set_generated_error(k, &e.message(), key),
        }
    }

    fn insert(&mut self, key: u64, out: Output) {
        if self.cache.insert(key, out).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > CACHE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.cache.remove(&old);
            }
        }
    }

    /// Text and hash of a script file (re-read when its size or mtime changes).
    fn source(&mut self, path: &Path) -> Result<(Arc<str>, u64), ScriptError> {
        let not_found = || {
            ScriptError::new(
                "Script not found",
                Some(format!(
                    "Could not find \u{201c}{}\u{201d}. Check for typos or missing folders.",
                    path.display()
                )),
            )
        };
        let meta = std::fs::metadata(path).map_err(|_| not_found())?;
        let (modified, len) = (meta.modified().ok(), meta.len());
        if let Some(s) = self.sources.get(path)
            && s.modified == modified
            && s.len == len
        {
            return Ok((s.text.clone(), s.hash));
        }
        let bytes = std::fs::read(path).map_err(|_| not_found())?;
        let text: Arc<str> = String::from_utf8_lossy(&bytes).into();
        let mut hs = DefaultHasher::new();
        text.hash(&mut hs);
        let hash = hs.finish();
        self.sources.insert(
            path.to_path_buf(),
            Source {
                modified,
                len,
                text: text.clone(),
                hash,
            },
        );
        Ok((text, hash))
    }
}

/// Script nodes in the subtree of `root` (not inside generated output).
fn collect_scripts(scene: &SceneDoc, root: NodeKey, out: &mut Vec<NodeKey>) {
    for k in scene.descendants(root) {
        if scene.get(k).is_some_and(|n| n.kind == Kind::Script) {
            out.push(k);
        }
    }
}

/// Path of a document-relative uri (`./a.js`, `scripts/a.js`, `file:///abs/a.js`).
pub fn resolve_path(base: Option<&Path>, uri: &str) -> PathBuf {
    let u = uri.strip_prefix("file://").unwrap_or(uri);
    // `file:///C:/x` -> `C:/x` on Windows.
    let u = if u.len() > 3 && u.starts_with('/') && u.as_bytes()[2] == b':' {
        &u[1..]
    } else {
        u
    };
    let p = Path::new(u);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let rel = u.strip_prefix("./").unwrap_or(u);
    match base {
        Some(b) => b.join(rel),
        None => PathBuf::from(rel),
    }
}

/// Hashes resolved parameters (script inputs, shader uniforms).
pub fn hash_params(params: &[(Arc<str>, ParamValue)], hs: &mut impl Hasher) {
    params.len().hash(hs);
    for (k, v) in params {
        k.hash(hs);
        match v {
            ParamValue::Num(n) => (0u8, n.to_bits()).hash(hs),
            ParamValue::Bool(b) => (1u8, *b).hash(hs),
            ParamValue::Str(s) => (2u8, s).hash(hs),
            ParamValue::Vec(v) => {
                3u8.hash(hs);
                for x in v {
                    x.to_bits().hash(hs);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_against_the_document() {
        let base = Path::new("/docs/a");
        assert_eq!(
            resolve_path(Some(base), "./clock.js"),
            Path::new("/docs/a").join("clock.js")
        );
        assert_eq!(
            resolve_path(Some(base), "scripts/x.js"),
            Path::new("/docs/a").join("scripts/x.js")
        );
        assert_eq!(resolve_path(None, "x.js"), PathBuf::from("x.js"));
    }

    #[test]
    fn error_message_has_title_and_details() {
        let e = ScriptError::new(
            "Script runtime error",
            Some("ReferenceError: x\n  at 3".into()),
        );
        assert_eq!(
            e.message(),
            "Script runtime error\nReferenceError: x\n  at 3"
        );
        assert_eq!(ScriptError::new("T", None).message(), "T");
    }
}
