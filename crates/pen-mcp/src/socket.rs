//! Local socket transport: the app listens on a named pipe (Windows) or a Unix
//! socket; `openpen-cli mcp` pumps stdio into it.
//!
//! Wire format: newline-delimited MCP JSON-RPC, exactly as on stdio. A client
//! may first send one hello line, `{"openpenHello":{"agent":"<name>"}}`, to
//! name the agent; anything else on the first line is treated as MCP traffic.
//! Only local IPC is used: no TCP, and the Windows pipe rejects remote clients.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use interprocess::local_socket::tokio::Stream as LocalStream;
use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
use interprocess::local_socket::{GenericFilePath, ListenerOptions, Name, ToFsName as _};
use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::server::{Session, Shared};
use crate::{McpOptions, ToolBackend, run_session};

/// Socket name used when none is given (`openpen-cli mcp` without `--socket`).
pub const DEFAULT_SOCKET_NAME: &str = "desktop";
/// Key of the optional first line sent by the shim.
pub const HELLO_KEY: &str = "openpenHello";
const MAX_HELLO_LINE: u64 = 16 * 1024 * 1024;
const CONNECT_ATTEMPTS: u32 = 5;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(200);
/// How long the shim keeps forwarding responses after its stdin closes.
const SHIM_DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Maps a short socket name to its OS location:
/// `\\.\pipe\openpen-<name>` on Windows, `~/.openpen/sock/openpen-<name>.sock`
/// elsewhere. A name that already is a pipe path or an absolute path is kept.
pub fn socket_path(name: &str) -> PathBuf {
    let name = if name.trim().is_empty() {
        DEFAULT_SOCKET_NAME
    } else {
        name.trim()
    };
    #[cfg(windows)]
    {
        if name.starts_with(r"\\.\pipe\") {
            return PathBuf::from(name);
        }
        PathBuf::from(format!(r"\\.\pipe\openpen-{name}"))
    }
    #[cfg(not(windows))]
    {
        if Path::new(name).is_absolute() {
            return PathBuf::from(name);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        home.join(".openpen")
            .join("sock")
            .join(format!("openpen-{name}.sock"))
    }
}

fn fs_name(path: &Path) -> io::Result<Name<'_>> {
    path.to_fs_name::<GenericFilePath>()
}

/// The hello line the shim sends before forwarding stdin.
pub fn hello_line(agent: Option<&str>) -> String {
    let mut hello = serde_json::Map::new();
    if let Some(a) = agent {
        hello.insert("agent".into(), a.into());
    }
    let mut line = serde_json::json!({ HELLO_KEY: hello }).to_string();
    line.push('\n');
    line
}

/// MCP server on a local socket; every connection is an independent session.
pub struct SocketServer {
    path: PathBuf,
    ct: CancellationToken,
    task: JoinHandle<()>,
}

impl SocketServer {
    /// Binds `socket_path(name)` and starts accepting in the current Tokio runtime.
    /// Fails with `AddrInUse` if another OpenPen already listens there.
    pub async fn bind(
        name: &str,
        backend: Arc<dyn ToolBackend>,
        opts: McpOptions,
    ) -> io::Result<Self> {
        let path = socket_path(name);
        prepare_path(&path).await?;
        let listener = ListenerOptions::new()
            .name(fs_name(&path)?)
            .create_tokio()?;
        let shared = Shared::new(backend, opts);
        let ct = CancellationToken::new();
        let task = tokio::spawn(accept_loop(listener, shared, ct.clone()));
        Ok(Self { path, ct, task })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops accepting, closes every session and waits for them to finish.
    pub async fn shutdown(self) {
        self.ct.cancel();
        let _ = self.task.await;
    }
}

#[cfg(unix)]
async fn prepare_path(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    if path.exists() {
        // A live server answers; a stale file from a crash does not.
        if LocalStream::connect(fs_name(path)?).await.is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("another OpenPen is already listening on {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn prepare_path(_path: &Path) -> io::Result<()> {
    Ok(())
}

async fn accept_loop(
    listener: interprocess::local_socket::tokio::Listener,
    shared: Arc<Shared>,
    ct: CancellationToken,
) {
    let next_id = AtomicU64::new(1);
    let mut sessions = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = ct.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    let id = format!("client-{}", next_id.fetch_add(1, Ordering::Relaxed));
                    sessions.spawn(handle_connection(stream, id, shared.clone(), ct.child_token()));
                }
                Err(e) => {
                    eprintln!("openpen mcp: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            // Reap finished sessions so the set does not grow forever.
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
    drop(listener);
    while sessions.join_next().await.is_some() {}
}

async fn handle_connection(
    stream: LocalStream,
    client_id: String,
    shared: Arc<Shared>,
    ct: CancellationToken,
) {
    let (read, write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let mut first = Vec::new();
    let read_first = async {
        (&mut reader)
            .take(MAX_HELLO_LINE)
            .read_until(b'\n', &mut first)
            .await
    };
    let n = tokio::select! {
        _ = ct.cancelled() => return,
        n = read_first => n,
    };
    if !matches!(n, Ok(n) if n > 0) {
        return;
    }
    let (agent, prefix) = match parse_hello(&first) {
        Some(agent) => (agent, Vec::new()),
        None => (None, first),
    };
    let read = std::io::Cursor::new(prefix).chain(reader);
    let session = Session::new(client_id, agent);
    // Errors here mean the client hung up early or sent garbage; nothing to report.
    let _ = run_session(shared, session, (read, write), ct).await;
}

/// `Some(agent)` if `line` is a hello line.
fn parse_hello(line: &[u8]) -> Option<Option<String>> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let hello = v.as_object()?.get(HELLO_KEY)?;
    Some(
        hello
            .get("agent")
            .and_then(|a| a.as_str())
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string),
    )
}

/// A [`SocketServer`] running on its own Tokio runtime, for hosts that are not
/// Tokio-based (the GPUI app). Dropping it shuts the server down.
pub struct BackgroundSocketServer {
    path: PathBuf,
    server: Option<SocketServer>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl BackgroundSocketServer {
    pub fn start(name: &str, backend: Arc<dyn ToolBackend>, opts: McpOptions) -> io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("openpen-mcp")
            .enable_all()
            .build()?;
        let server = runtime.block_on(SocketServer::bind(name, backend, opts))?;
        Ok(Self {
            path: server.path().to_path_buf(),
            server: Some(server),
            runtime: Some(runtime),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let (Some(server), Some(rt)) = (self.server.take(), self.runtime.as_ref()) {
            let _ = rt.block_on(tokio::time::timeout(
                Duration::from_secs(2),
                server.shutdown(),
            ));
        }
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_millis(200));
        }
    }
}

impl Drop for BackgroundSocketServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Why the shim could not run.
#[derive(Debug, thiserror::Error)]
pub enum ShimError {
    #[error(
        "OpenPen is not running: could not connect to {path} ({source}). \
         Start OpenPen and try again."
    )]
    NotRunning { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Connects to a running OpenPen, retrying briefly.
pub async fn connect(name: &str) -> Result<LocalStream, ShimError> {
    let path = socket_path(name);
    let mut last = None;
    for attempt in 0..CONNECT_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(CONNECT_RETRY_DELAY).await;
        }
        match LocalStream::connect(fs_name(&path)?).await {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(ShimError::NotRunning {
        path,
        source: last.unwrap_or_else(|| io::Error::other("unknown error")),
    })
}

/// Pumps this process's stdin/stdout to the OpenPen socket `name`.
pub async fn run_stdio_shim(name: &str, agent: Option<&str>) -> Result<(), ShimError> {
    run_shim(name, agent, tokio::io::stdin(), tokio::io::stdout()).await
}

/// [`run_stdio_shim`] over arbitrary streams. Returns when the app closes the
/// connection, or shortly after `input` reaches EOF.
pub async fn run_shim<R, W>(
    name: &str,
    agent: Option<&str>,
    mut input: R,
    mut output: W,
) -> Result<(), ShimError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let stream = connect(name).await?;
    let (mut sock_read, mut sock_write) = tokio::io::split(stream);
    sock_write.write_all(hello_line(agent).as_bytes()).await?;
    sock_write.flush().await?;

    let up = pump(&mut input, &mut sock_write);
    let down = pump(&mut sock_read, &mut output);
    tokio::pin!(up, down);
    tokio::select! {
        r = &mut down => {
            // The app closed the connection (or exited).
            ignore_disconnect(r)?;
        }
        r = &mut up => {
            ignore_disconnect(r)?;
            // stdin closed: let responses already in flight through, then leave.
            if let Ok(r) = tokio::time::timeout(SHIM_DRAIN_GRACE, &mut down).await {
                ignore_disconnect(r)?;
            }
        }
    }
    Ok(())
}

/// Copies until EOF, flushing after every chunk so JSON-RPC lines are not held back.
async fn pump<R, W>(from: &mut R, to: &mut W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = from.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n]).await?;
        to.flush().await?;
    }
}

fn ignore_disconnect(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::UnexpectedEof
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let p = socket_path("dev");
        #[cfg(windows)]
        assert_eq!(p, PathBuf::from(r"\\.\pipe\openpen-dev"));
        #[cfg(not(windows))]
        assert!(p.ends_with(".openpen/sock/openpen-dev.sock"));
        assert_eq!(socket_path(""), socket_path(DEFAULT_SOCKET_NAME));
    }

    #[test]
    fn hello_roundtrip() {
        assert_eq!(
            parse_hello(hello_line(Some("codex")).as_bytes()),
            Some(Some("codex".into()))
        );
        assert_eq!(parse_hello(hello_line(None).as_bytes()), Some(None));
        assert_eq!(
            parse_hello(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#),
            None
        );
        assert_eq!(parse_hello(b"garbage\n"), None);
    }
}
