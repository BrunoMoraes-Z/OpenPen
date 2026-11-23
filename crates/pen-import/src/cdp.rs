//! Minimal Chrome DevTools Protocol client over a local websocket.
//!
//! Only request/response is needed by the importer (events are dropped), so
//! this is a few dozen lines instead of a generated binding of the whole
//! protocol. Sessions use the "flatten" mode: every command carries the
//! `sessionId` of the attached page.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

pub struct Cdp {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next: AtomicU64,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Cdp {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Cdp {
    pub async fn connect(ws_url: &str) -> Result<Cdp, String> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(512 << 20))
            .max_frame_size(Some(512 << 20));
        let (ws, _) = tokio_tungstenite::connect_async_with_config(ws_url, Some(config), false)
            .await
            .map_err(|e| format!("cannot connect to the browser: {e}"))?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut rx) = mpsc::unbounded_channel::<Message>();
        let pending: Pending = Arc::default();
        let writer = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });
        let p = pending.clone();
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let Some(id) = v.get("id").and_then(Value::as_u64) else {
                    continue; // event
                };
                if let Some(tx) = p.lock().await.remove(&id) {
                    let r = match v.get("error") {
                        Some(e) => Err(e
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("CDP error")
                            .to_string()),
                        None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(r);
                }
            }
            // Connection closed: fail every outstanding call.
            for (_, tx) in p.lock().await.drain() {
                let _ = tx.send(Err("browser connection closed".into()));
            }
        });
        Ok(Cdp {
            out,
            pending,
            next: AtomicU64::new(1),
            tasks: vec![writer, reader],
        })
    }

    /// Sends `method` (to the page `session`, or the browser when `None`) and waits for its result.
    pub async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = s.into();
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.out
            .send(Message::text(msg.to_string()))
            .map_err(|_| "browser connection closed".to_string())?;
        rx.await
            .map_err(|_| "browser connection closed".to_string())?
            .map_err(|e| format!("{method}: {e}"))
    }
}

/// A page target attached in flatten mode.
pub struct Page {
    pub cdp: Cdp,
    pub session: String,
}

impl Page {
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.cdp.call(Some(&self.session), method, params).await
    }

    /// Evaluates `expr` in the page (awaiting promises) and returns its JSON value.
    pub async fn eval(&self, expr: &str) -> Result<Value, String> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expr,
                    "returnByValue": true,
                    "awaitPromise": true,
                    "userGesture": false,
                }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let text = ex
                .pointer("/exception/description")
                .or_else(|| ex.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("script error");
            return Err(text.lines().next().unwrap_or(text).to_string());
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Evaluates `expr`, which must produce a string.
    pub async fn eval_str(&self, expr: &str) -> Result<String, String> {
        match self.eval(expr).await? {
            Value::String(s) => Ok(s),
            v => Err(format!("unexpected script result: {v}")),
        }
    }
}
