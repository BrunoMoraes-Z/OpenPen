//! OpenPen MCP server.
//!
//! Lets external agents (Claude Code, Codex, ...) drive OpenPen through the
//! Model Context Protocol. The crate only speaks the protocol; documents are
//! handled by a [`ToolBackend`] supplied by the GUI app or the headless CLI.
//!
//! Transports:
//! - [`serve_stdio`]: a plain stdio server (headless use);
//! - [`socket::SocketServer`]: a local socket (named pipe on Windows, Unix
//!   socket elsewhere) carrying MCP JSON-RPC lines, one session per connection;
//! - [`socket::run_stdio_shim`]: a stdio <-> socket byte pump that MCP clients
//!   launch (`openpen-cli mcp`) to reach a running app.

pub mod backend;
pub mod config;
mod server;
pub mod skills;
pub mod socket;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::RoleServer;
use rmcp::transport::IntoTransport;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

pub use backend::{
    CallCtx, TOOL_USE_ID_META, ToolBackend, ToolError, ToolOutput, ToolResult, uri_to_path,
};
pub use futures::future::BoxFuture;
pub use server::INSTRUCTIONS;
pub use skills::{SkillStore, find_skills_dir};
pub use tools::{CodeEdit, ExecuteArgs};

use server::{OpenPenServer, Session, Shared};

/// Server settings shared by all transports.
#[derive(Debug, Clone)]
pub struct McpOptions {
    /// Folder with `SKILL.md`, `execute.md`, ... served by `read_skill`/`start_task`.
    pub skills_dir: Option<PathBuf>,
    /// Agent name to report instead of the MCP `clientInfo.name`.
    pub agent_name: Option<String>,
    /// Serve the `save` tool.
    pub enable_save: bool,
}

impl Default for McpOptions {
    fn default() -> Self {
        Self {
            skills_dir: None,
            agent_name: None,
            enable_save: true,
        }
    }
}

impl McpOptions {
    /// Default options with the skills folder found by [`find_skills_dir`].
    pub fn with_default_skills() -> Self {
        Self {
            skills_dir: find_skills_dir(),
            ..Self::default()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("MCP handshake failed: {0}")]
    Init(String),
    #[error("MCP session failed: {0}")]
    Session(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Serves one MCP session over stdin/stdout until the client goes away.
pub async fn serve_stdio(
    backend: Arc<dyn ToolBackend>,
    opts: McpOptions,
) -> Result<(), ServeError> {
    let shared = Shared::new(backend, opts);
    let session = Session::new("stdio".into(), None);
    let stdio = (tokio::io::stdin(), tokio::io::stdout());
    run_session(shared, session, stdio, CancellationToken::new()).await
}

/// Serves one MCP session over an arbitrary byte stream (used by tests and
/// in-process clients). `client_id` identifies the session in [`CallCtx`].
pub async fn serve_io<R, W>(
    backend: Arc<dyn ToolBackend>,
    opts: McpOptions,
    client_id: impl Into<String>,
    read: R,
    write: W,
) -> Result<(), ServeError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let shared = Shared::new(backend, opts);
    let session = Session::new(client_id.into(), None);
    run_session(shared, session, (read, write), CancellationToken::new()).await
}

pub(crate) async fn run_session<T, E, A>(
    shared: Arc<Shared>,
    session: Arc<Session>,
    transport: T,
    ct: CancellationToken,
) -> Result<(), ServeError>
where
    T: IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    use rmcp::ServiceExt as _;
    let server = OpenPenServer::new(shared.clone(), session.clone());
    let result = match server.serve_with_ct(transport, ct).await {
        Ok(running) => running
            .waiting()
            .await
            .map(|_| ())
            .map_err(|e| ServeError::Session(e.to_string())),
        Err(e) => Err(ServeError::Init(e.to_string())),
    };
    session.finish(&shared);
    result
}
