//! Mock backend shared by the integration tests.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pen_mcp::{
    BoxFuture, CallCtx, ExecuteArgs, ImportWebsiteArgs, ImportedPage, ToolBackend, ToolError,
    ToolOutput, ToolResult,
};

/// 1x1 transparent PNG.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connected(CallCtx),
    Disconnected(CallCtx),
    AppState(CallCtx, Option<String>),
    Execute(CallCtx, ExecuteArgs),
    Save(CallCtx, Option<String>),
    Import(CallCtx, ImportWebsiteArgs, String),
}

#[derive(Default)]
pub struct MockBackend {
    pub events: Mutex<Vec<Event>>,
}

impl MockBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn push(&self, e: Event) {
        self.events.lock().unwrap().push(e);
    }
}

impl ToolBackend for MockBackend {
    fn get_app_state(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        self.push(Event::AppState(ctx.clone(), uri.clone()));
        Box::pin(async move {
            match uri.as_deref() {
                Some("missing.pen") => Err(ToolError::NoDocument(
                    "no document open for missing.pen".into(),
                )),
                _ => Ok(ToolOutput::text(format!(
                    "Focused document: file:///demo.pen\nAgent: {}",
                    ctx.agent_name
                ))),
            }
        })
    }

    fn execute(&self, ctx: CallCtx, args: ExecuteArgs) -> BoxFuture<'static, ToolResult> {
        self.push(Event::Execute(ctx, args.clone()));
        Box::pin(async move {
            // Simulate the hop to another thread and back.
            tokio::task::yield_now().await;
            match args.input.as_deref() {
                Some("throw") => Ok(ToolOutput::error("Error: boom at 1:1\neditId: e1")),
                Some("screenshot") => Ok(ToolOutput::text("1 screenshot")
                    .with_image(PNG.to_vec())
                    .with_image(PNG.to_vec())),
                Some("invalid") => Err(ToolError::InvalidParams("bad node data".into())),
                Some("crash") => Err(ToolError::Unavailable("editor is gone".into())),
                Some("fail") => Err(ToolError::Failed("script failed".into())),
                _ => Ok(ToolOutput::text(format!("ran: {}", args.description))),
            }
        })
    }

    fn save(&self, ctx: CallCtx, uri: Option<String>) -> BoxFuture<'static, ToolResult> {
        self.push(Event::Save(ctx, uri));
        Box::pin(async { Ok(ToolOutput::text("saved")) })
    }

    fn import_website(
        &self,
        ctx: CallCtx,
        args: ImportWebsiteArgs,
        url: String,
    ) -> BoxFuture<'static, ToolResult> {
        self.push(Event::Import(ctx, args.clone(), url.clone()));
        Box::pin(async move {
            tokio::task::yield_now().await;
            if url.contains("unreachable") {
                return Err(ToolError::Failed("navigation failed: unreachable".into()));
            }
            Ok(ImportedPage {
                frame_id: "web1".into(),
                title: "Example Domain".into(),
                url,
                size: (args.width.unwrap_or(1440) as f32, 600.0),
                stats: vec![("nodes".into(), "7".into())],
                warnings: vec!["1 image could not be downloaded".into()],
                assets: 1,
                screenshot: Some(PNG.to_vec()),
            }
            .to_output())
        })
    }

    fn agent_connected(&self, ctx: &CallCtx) {
        self.push(Event::Connected(ctx.clone()));
    }

    fn agent_disconnected(&self, ctx: &CallCtx) {
        self.push(Event::Disconnected(ctx.clone()));
    }
}

/// A temporary skills folder with a file outside of it to try escaping to.
pub fn skills_dir(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("pen-mcp-it-{tag}-{}", std::process::id()));
    let root = base.join("skills");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("SKILL.md"), "# OpenPen skills index").unwrap();
    std::fs::write(root.join("execute.md"), "execute API").unwrap();
    std::fs::write(root.join("pen-schema.md"), "schema docs").unwrap();
    std::fs::write(base.join("outside.txt"), "secret").unwrap();
    root
}

/// A pipe/socket name unique to this test run.
pub fn unique_socket(tag: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let name = format!("test-{tag}-{}-{n}", std::process::id());
    #[cfg(unix)]
    {
        // Keep Unix socket paths short and out of $HOME.
        std::env::temp_dir()
            .join(format!("{name}.sock"))
            .display()
            .to_string()
    }
    #[cfg(not(unix))]
    {
        name
    }
}
