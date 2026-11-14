//! Helpers the browser tests share: a small HTTP server with a form page, a raw DevTools connection and a retry loop.
#![allow(dead_code)]

use async_tungstenite::tokio::connect_async;
use async_tungstenite::tungstenite::Message;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

pub const WAIT: Duration = Duration::from_secs(10);

pub const PAGE: &str = r##"<!doctype html><html><head><title>Relay page</title></head><body>
<h1>Hello relay</h1>
<form id="f" onsubmit="event.preventDefault(); document.getElementById('out').textContent = 'sent ' + document.getElementById('name').value + '/' + document.getElementById('color').value + '/' + document.getElementById('agree').checked;">
<label for="name">Full name</label><input id="name" name="name">
<select id="color" name="color"><option value="r">Red</option><option value="g">Green</option></select>
<label><input type="checkbox" id="agree"> I agree</label>
<input type="file" id="file">
<button type="submit">Send</button>
</form>
<button id="hit" onclick="document.getElementById('out').textContent='hit'">Hit me</button>
<div id="out">idle</div>
<script>
console.log('page loaded');
console.warn('careful');
fetch('/api').then((r) => r.json()).then((j) => console.log('api ok ' + j.ok));
</script></body></html>"##;

/// A tiny HTTP server for the page, a second page and a JSON endpoint.
pub async fn serve() -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, kind, body) = match path.as_str() {
                    "/api" => ("200 OK", "application/json", r#"{"ok":true}"#),
                    "/second" => ("200 OK", "text/html", "<title>Second page</title><p>second body</p>"),
                    "/favicon.ico" => ("404 Not Found", "text/plain", ""),
                    _ => ("200 OK", "text/html", PAGE),
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (port, task)
}

pub type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A raw DevTools connection to the test's Chrome (flattened sessions).
pub struct Cdp {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
    tasks: Vec<JoinHandle<()>>,
}

impl Cdp {
    pub async fn connect(url: &str) -> (Cdp, mpsc::UnboundedReceiver<Value>) {
        let (ws, _) = connect_async(url).await.unwrap();
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (events_tx, events) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let writer = tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                if sink.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
        });
        let table = Arc::clone(&pending);
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                match v.get("id").and_then(Value::as_u64) {
                    Some(id) => {
                        let answer = match v.get("error") {
                            Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        if let Some(tx) = table.lock().unwrap().remove(&id) {
                            let _ = tx.send(answer);
                        }
                    }
                    None => {
                        let _ = events_tx.send(v);
                    }
                }
            }
        });
        (Cdp { out, pending, next: AtomicU64::new(1), tasks: vec![writer, reader] }, events)
    }

    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.out.send(msg.to_string()).unwrap();
        tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| format!("{method} timed out"))?
            .map_err(|_| "closed".to_string())?
    }
}

impl Drop for Cdp {
    fn drop(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
}

/// Retries a check for a while: the browser's events arrive a moment after the command that caused them.
pub async fn until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while !check().await {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
