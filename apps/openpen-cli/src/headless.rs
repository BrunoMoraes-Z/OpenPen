//! `openpen-cli mcp --headless FILE`: a [`ToolBackend`] over one document loaded
//! in memory, without the app.
//!
//! The document, its layout pipeline, the undo stack and the script engine live
//! on a dedicated worker thread (with a large native stack, so JavaScript
//! recursion in `execute` snippets is not cut short); tool calls are sent to it
//! as closures and answered through a oneshot channel. Calls are therefore
//! serialized, exactly like edits in the GUI.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use pen_doc::{FsLibraries, SceneDoc, UndoStack};
use pen_layout::{FsAssets, Pipeline, TextEngine};
use pen_mcp::{
    BoxFuture, CallCtx, ExecuteArgs, ImportWebsiteArgs, ImportedPage, ToolBackend, ToolError,
    ToolOutput, ToolResult,
};
use pen_script::{AppStateInfo, Edit, ExecuteRequest, HeadlessHost, Limits, ScriptEngine};

/// Native stack of the worker thread.
pub const WORKER_STACK: usize = 16 * 1024 * 1024;
/// QuickJS stack limit inside it (the rest is headroom for host calls: layout,
/// rendering, PDF export).
pub const SCRIPT_STACK: usize = 8 * 1024 * 1024;

/// Everything the worker owns.
pub struct DocState {
    pub path: PathBuf,
    pub scene: SceneDoc,
    pub pipe: Pipeline,
    pub undo: UndoStack,
    pub engine: ScriptEngine,
    pub host: HeadlessHost,
    /// Undo depth at the last save (to report unsaved changes).
    saved_at: usize,
    saved_once: bool,
}

impl DocState {
    /// Loads and lays out a `.pen` file.
    pub fn open(path: &Path) -> Result<DocState> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let dir = path.parent().map(Path::to_path_buf);
        let (mut scene, _) = SceneDoc::load(&bytes, dir.as_deref(), &FsLibraries::default())
            .with_context(|| format!("loading {}", path.display()))?;
        let mut pipe = Pipeline::new(TextEngine::new(), Box::new(FsAssets::new(dir)));
        pipe.set_script_runner(Box::new(pen_script::ScriptNodeRunner::new()));
        pipe.set_shader_renderer(std::sync::Arc::new(pen_shader::ShaderEngine::new()));
        pipe.rebuild(&mut scene);
        let engine = ScriptEngine::with_limits(Limits {
            stack_bytes: SCRIPT_STACK,
            ..Limits::default()
        });
        Ok(DocState {
            path,
            scene,
            pipe,
            undo: UndoStack::new(),
            engine,
            host: HeadlessHost,
            saved_at: 0,
            saved_once: false,
        })
    }

    fn dirty(&self) -> bool {
        self.undo.undo_len() != self.saved_at || (!self.saved_once && self.undo.undo_len() > 0)
    }

    pub fn app_state(&mut self, agents: &[String]) -> String {
        self.pipe.update(&mut self.scene);
        let uri = self.path.display().to_string();
        let mut md = pen_script::describe_app_state(
            &self.scene,
            &mut self.pipe,
            &AppStateInfo {
                uri: Some(&uri),
                selection: &[],
                viewport: None,
                connected_agents: agents,
            },
        );
        md.push_str(&format!(
            "\nHeadless mode: no editor window, no selection. Unsaved changes: {}. \
             Call `save` to write the file.\n",
            if self.dirty() { "yes" } else { "no" }
        ));
        md
    }

    pub fn execute(&mut self, agent_id: String, args: ExecuteArgs) -> ToolOutput {
        let req = ExecuteRequest {
            agent_id,
            description: args.description,
            input: args.input,
            edit_id: args.edit_id,
            edits: args
                .edits
                .into_iter()
                .map(|e| Edit {
                    find: e.find,
                    replace: e.replace,
                    all: e.all,
                })
                .collect(),
        };
        let resp = self.engine.execute(
            &mut self.scene,
            &mut self.pipe,
            &mut self.undo,
            &mut self.host,
            req,
        );
        ToolOutput {
            text: resp.text,
            images: resp.images,
            is_error: !resp.ok,
        }
    }

    /// Inserts an imported page at a free spot of the root (one undo step).
    /// Returns the id of the new frame.
    pub fn insert_frame(
        &mut self,
        agent_id: String,
        frame: &serde_json::Value,
    ) -> Result<String, String> {
        let req = pen_script::insert_at_free_spot(agent_id, "Import web page", frame);
        let resp = self.engine.execute(
            &mut self.scene,
            &mut self.pipe,
            &mut self.undo,
            &mut self.host,
            req,
        );
        pen_script::inserted_id(&resp).ok_or_else(|| pen_script::place::insert_error(&resp))
    }

    /// Writes the document (lossless; atomic replace through a temp file).
    pub fn save(&mut self) -> Result<String, String> {
        let text = self.scene.save();
        let tmp = self.path.with_extension("pen.tmp");
        std::fs::write(&tmp, text.as_bytes())
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("cannot write {}: {e}", self.path.display())
            })?;
        self.saved_at = self.undo.undo_len();
        self.saved_once = true;
        Ok(format!(
            "Saved {} ({} bytes).",
            self.path.display(),
            text.len()
        ))
    }
}

type Job = Box<dyn FnOnce(&mut DocState) + Send>;

/// The headless [`ToolBackend`].
pub struct HeadlessBackend {
    jobs: Mutex<mpsc::Sender<Job>>,
    path: PathBuf,
    canonical: Option<PathBuf>,
    readonly: bool,
    agents: Arc<Mutex<Vec<String>>>,
}

impl HeadlessBackend {
    /// Loads `path` and starts the worker thread.
    pub fn open(path: &Path, readonly: bool) -> Result<HeadlessBackend> {
        let state = DocState::open(path)?;
        let path = state.path.clone();
        let canonical = std::fs::canonicalize(&path).ok();
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("openpen-doc".into())
            .stack_size(WORKER_STACK)
            .spawn(move || {
                let mut state = state;
                while let Ok(job) = rx.recv() {
                    job(&mut state);
                }
            })
            .context("starting the document thread")?;
        Ok(HeadlessBackend {
            jobs: Mutex::new(tx),
            path,
            canonical,
            readonly,
            agents: Arc::default(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` on the worker thread.
    fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut DocState) -> R + Send + 'static,
    ) -> BoxFuture<'static, Result<R, ToolError>> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        run_on(&jobs, f)
    }

    /// `None` when `uri` names the served document (or is absent).
    fn check_uri(&self, uri: Option<&str>) -> Option<ToolError> {
        let uri = uri?.trim();
        if uri.is_empty() {
            return None;
        }
        let matches = pen_mcp::uri_to_path(uri).is_some_and(|p| {
            let abs = std::path::absolute(&p).unwrap_or(p);
            if abs == self.path {
                return true;
            }
            match (&self.canonical, std::fs::canonicalize(&abs)) {
                (Some(a), Ok(b)) => *a == b,
                _ => false,
            }
        });
        (!matches).then(|| {
            ToolError::NoDocument(format!(
                "This headless OpenPen server serves only `{}`; `{uri}` is not open. \
                 Omit `uri` (or pass that path) to work on it, or start another \
                 `openpen-cli mcp --headless <file>` for a different document.",
                self.path.display()
            ))
        })
    }
}

/// Runs `f` on the worker thread behind `jobs`.
fn run_on<R: Send + 'static>(
    jobs: &mpsc::Sender<Job>,
    f: impl FnOnce(&mut DocState) -> R + Send + 'static,
) -> BoxFuture<'static, Result<R, ToolError>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let job: Job = Box::new(move |s| {
        let _ = tx.send(f(s));
    });
    let sent = jobs.send(job).is_ok();
    Box::pin(async move {
        if !sent {
            return Err(ToolError::Unavailable("the document thread stopped".into()));
        }
        rx.await
            .map_err(|_| ToolError::Unavailable("the document thread stopped".into()))
    })
}

/// Globals and failed snippets are kept per agent name, so they survive reconnects.
fn agent_id(ctx: &CallCtx) -> String {
    if ctx.agent_name.trim().is_empty() {
        ctx.client_id.clone()
    } else {
        ctx.agent_name.clone()
    }
}

impl ToolBackend for HeadlessBackend {
    fn get_app_state(&self, _ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        if let Some(e) = self.check_uri(uri.as_deref()) {
            return Box::pin(async move { Err(e) });
        }
        let agents = self
            .agents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let fut = self.call(move |s| s.app_state(&agents));
        Box::pin(async move { fut.await.map(ToolOutput::text) })
    }

    fn execute(&self, ctx: CallCtx, args: ExecuteArgs) -> BoxFuture<'static, ToolResult> {
        if let Some(e) = self.check_uri(args.uri.as_deref()) {
            return Box::pin(async move { Err(e) });
        }
        let id = agent_id(&ctx);
        let fut = self.call(move |s| s.execute(id, args));
        Box::pin(fut)
    }

    fn save(&self, _ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        if let Some(e) = self.check_uri(uri.as_deref()) {
            return Box::pin(async move { Err(e) });
        }
        if self.readonly {
            return Box::pin(async {
                Err(ToolError::Failed(
                    "this server was started with --readonly: changes stay in memory".into(),
                ))
            });
        }
        let fut = self.call(DocState::save);
        Box::pin(async move {
            match fut.await? {
                Ok(t) => Ok(ToolOutput::text(t)),
                Err(e) => Err(ToolError::Failed(e)),
            }
        })
    }

    fn import_website(
        &self,
        ctx: CallCtx,
        args: ImportWebsiteArgs,
        url: String,
    ) -> BoxFuture<'static, ToolResult> {
        if let Some(e) = self.check_uri(args.uri.as_deref()) {
            return Box::pin(async move { Err(e) });
        }
        if self.readonly {
            return Box::pin(async {
                Err(ToolError::Failed(
                    "this server was started with --readonly, and importing a page writes                      image and font files next to the document"
                        .into(),
                ))
            });
        }
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let agent = agent_id(&ctx);
        Box::pin(async move {
            let t0 = std::time::Instant::now();
            let page = crate::web_import::import(&args, url.clone(), dir)
                .await
                .map_err(ToolError::Failed)?;
            let frame = page.frame();
            let frame_id = run_on(&jobs, move |s| s.insert_frame(agent, &frame))
                .await?
                .map_err(ToolError::Failed)?;
            let mut stats = page.stats.clone();
            stats.push((
                "time".into(),
                format!("{:.1} s", t0.elapsed().as_secs_f32()),
            ));
            Ok(ImportedPage {
                frame_id,
                title: page.title.clone(),
                url,
                size: page.size(),
                stats,
                warnings: page.warnings.clone(),
                assets: page.assets,
                screenshot: page.screenshot.clone(),
            }
            .to_output())
        })
    }

    fn agent_connected(&self, ctx: &CallCtx) {
        let mut a = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        if !a.contains(&ctx.agent_name) {
            a.push(ctx.agent_name.clone());
        }
    }

    fn agent_disconnected(&self, ctx: &CallCtx) {
        self.agents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|n| *n != ctx.agent_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str, body: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/headless-unit")
            .join(format!("{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("doc.pen");
        std::fs::write(&p, body).unwrap();
        p
    }

    fn ctx() -> CallCtx {
        CallCtx {
            client_id: "stdio".into(),
            agent_name: "tester".into(),
            ..Default::default()
        }
    }

    fn exec(src: &str) -> ExecuteArgs {
        ExecuteArgs {
            description: "test".into(),
            input: Some(src.into()),
            ..Default::default()
        }
    }

    fn block<T>(f: BoxFuture<'static, T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    const EMPTY: &str = r#"{"version":"2.22","children":[]}"#;

    #[test]
    fn uri_check_and_save() {
        let p = fixture("uri", EMPTY);
        let b = HeadlessBackend::open(&p, false).unwrap();
        assert!(b.check_uri(None).is_none());
        assert!(b.check_uri(Some(&p.display().to_string())).is_none());
        let other = p.with_file_name("other.pen");
        let e = b.check_uri(Some(&other.display().to_string())).unwrap();
        assert!(e.to_string().contains("serves only"), "{e}");

        let out = block(b.execute(
            ctx(),
            exec(r#"Insert(document, {type: "rectangle", name: "R", width: 10, height: 10})"#),
        ))
        .unwrap();
        assert!(!out.is_error, "{}", out.text);
        let st = block(b.get_app_state(ctx(), None)).unwrap();
        assert!(st.text.contains("Unsaved changes: yes"), "{}", st.text);
        let saved = block(b.save(ctx(), None)).unwrap();
        assert!(saved.text.starts_with("Saved"), "{}", saved.text);
        let st = block(b.get_app_state(ctx(), None)).unwrap();
        assert!(st.text.contains("Unsaved changes: no"), "{}", st.text);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"R\""), "{text}");

        let ro = HeadlessBackend::open(&p, true).unwrap();
        assert!(block(ro.save(ctx(), None)).is_err());
    }

    /// JavaScript recursion depth with the worker stack (vs. ~150 on a 1 MiB thread).
    #[test]
    fn deep_recursion_on_worker() {
        let p = fixture("stack", EMPTY);
        let b = HeadlessBackend::open(&p, false).unwrap();
        let out = block(b.execute(
            ctx(),
            exec("let d = 0\nfunction f() { d++; f() }\ntry { f() } catch (e) {}\nPrint(d)"),
        ))
        .unwrap();
        assert!(!out.is_error, "{}", out.text);
        let depth: usize = out
            .text
            .lines()
            .find_map(|l| l.trim().parse().ok())
            .unwrap_or_else(|| panic!("no depth in {}", out.text));
        eprintln!("JS recursion depth on the worker thread: {depth}");
        assert!(depth > 1000, "depth {depth}");
    }
}
