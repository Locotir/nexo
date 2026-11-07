//! JSON-RPC 2.0 over lines: one JSON object per line, in both directions.
//!
//! Everything written to stdout goes through one writer task, so lines never
//! interleave and their order is the order of the calls that sent them. The
//! agent's requests to the client (permissions, file reads) wait here for the
//! matching response.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// A JSON-RPC error object.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into() }
    }

    pub fn invalid_params(message: impl Into<String>) -> RpcError {
        RpcError::new(-32602, message)
    }

    pub fn internal(message: impl Into<String>) -> RpcError {
        RpcError::new(-32603, message)
    }

    pub fn method_not_found(method: &str) -> RpcError {
        RpcError::new(-32601, format!("method not found: {method}"))
    }

    /// The ACP code for "sign in first".
    pub fn auth_required(message: impl Into<String>) -> RpcError {
        RpcError::new(-32000, message)
    }
}

/// What came in on stdin.
#[derive(Debug)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        result: Result<Value, RpcError>,
    },
    /// Valid JSON that is none of the three.
    Invalid {
        id: Value,
    },
}

impl Incoming {
    pub fn parse(v: Value) -> Incoming {
        let id = v.get("id").cloned();
        let method = v.get("method").and_then(Value::as_str).map(String::from);
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            (Some(method), Some(id)) if !id.is_null() => Incoming::Request { id, method, params },
            (Some(method), _) => Incoming::Notification { method, params },
            (None, Some(id)) if v.get("result").is_some() || v.get("error").is_some() => {
                let result = match v.get("error") {
                    Some(e) => Err(RpcError::new(
                        e.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        e.get("message").and_then(Value::as_str).unwrap_or("the client reported an error"),
                    )),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                Incoming::Response { id, result }
            }
            (None, id) => Incoming::Invalid { id: id.unwrap_or(Value::Null) },
        }
    }
}

enum Out {
    Line(String),
    Flush(oneshot::Sender<()>),
}

type Waiter = oneshot::Sender<Result<Value, RpcError>>;

/// The sending half of the connection, shared by every session.
pub struct Rpc {
    tx: mpsc::UnboundedSender<Out>,
    pending: Mutex<HashMap<u64, Waiter>>,
    next: AtomicU64,
}

impl Rpc {
    /// Starts the writer task on `w` (stdout in the server, a pipe in tests).
    pub fn start<W: AsyncWrite + Unpin + Send + 'static>(mut w: W) -> Arc<Rpc> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Out>();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                match m {
                    Out::Line(mut l) => {
                        l.push('\n');
                        if w.write_all(l.as_bytes()).await.is_err() || w.flush().await.is_err() {
                            return;
                        }
                    }
                    Out::Flush(done) => {
                        let _ = w.flush().await;
                        let _ = done.send(());
                    }
                }
            }
        });
        Arc::new(Rpc { tx, pending: Mutex::new(HashMap::new()), next: AtomicU64::new(1) })
    }

    fn send(&self, v: Value) {
        // serde_json escapes newlines inside strings, so one value is always one line
        let _ = self.tx.send(Out::Line(v.to_string()));
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        match result {
            Ok(r) => self.send(json!({"jsonrpc": "2.0", "id": id, "result": r})),
            Err(e) => self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": e.code, "message": e.message}})),
        }
    }

    /// Asks the client something and waits for its answer.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        rx.await.unwrap_or_else(|_| Err(RpcError::internal("the connection closed")))
    }

    /// Hands a response of the client to the request that waits for it.
    pub fn resolve(&self, id: &Value, result: Result<Value, RpcError>) {
        let waiter = id.as_u64().and_then(|n| self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&n));
        match waiter {
            Some(w) => {
                let _ = w.send(result);
            }
            None => tracing::debug!("response {id} matches no request"),
        }
    }

    /// Ends every request still waiting (the client went away).
    pub fn fail_all(&self, why: &str) {
        let all: Vec<_> = self.pending.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
        for (_, w) in all {
            let _ = w.send(Err(RpcError::internal(why)));
        }
    }

    /// Returns once everything sent so far is written.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Out::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_message_kinds_are_told_apart() {
        let r = Incoming::parse(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}));
        assert!(matches!(r, Incoming::Request { .. }));
        let n = Incoming::parse(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s"}}));
        assert!(matches!(n, Incoming::Notification { .. }));
        let ok = Incoming::parse(json!({"jsonrpc":"2.0","id":3,"result":{"a":1}}));
        assert!(matches!(ok, Incoming::Response { result: Ok(_), .. }));
        let bad = Incoming::parse(json!({"jsonrpc":"2.0","id":3,"error":{"code":-1,"message":"no"}}));
        assert!(matches!(bad, Incoming::Response { result: Err(_), .. }));
        assert!(matches!(Incoming::parse(json!({"hello": 1})), Incoming::Invalid { .. }));
    }
}
