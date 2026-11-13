//! The agent's end of the wire: finds the hosts, proves it knows the token, and then sends requests to the browser
//! and receives its events. One `Client` is one session with one browser; the host keeps sessions apart.

use crate::endpoint::{self, BoxStream, Dirs};
use crate::{frame, proto, token, BrowserInfo};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

const HANDSHAKE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("the host refused the connection: {0}")]
    Denied(String),
    #[error("the browser connection closed: {0}")]
    Closed(String),
    #[error("{0}")]
    Remote(String),
    #[error("no answer after {0:?}")]
    Timeout(Duration),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// A CDP event of one of the session's tabs.
#[derive(Debug, Clone)]
pub struct Event {
    pub tab: i64,
    pub method: String,
    pub params: Value,
}

/// A browser a host serves, and the agents already using it.
#[derive(Debug, Clone)]
pub struct Listing {
    pub info: BrowserInfo,
    pub others: Vec<String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Error>>>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a host said back to a valid `hello`.
struct Welcome {
    info: BrowserInfo,
    others: Vec<String>,
}

async fn handshake(
    stream: &mut BoxStream,
    token: &str,
    client: &str,
    name: &str,
    probe: bool,
) -> Result<Welcome, Error> {
    let run = async {
        let challenge =
            frame::read_json(stream, 64 * 1024).await?.ok_or_else(|| Error::Closed("the host hung up".into()))?;
        let host_nonce =
            match (challenge.get("t").and_then(Value::as_str), challenge.get("nonce").and_then(Value::as_str)) {
                (Some("challenge"), Some(n)) => n.to_string(),
                _ => return Err(Error::Protocol("the host did not start with a challenge".into())),
            };
        let nonce = proto::nonce();
        let hello = json!({
            "t": "hello", "client": client, "name": name, "nonce": nonce, "probe": probe,
            "proof": proto::proof("agent", token, &host_nonce, &nonce),
        });
        frame::write_json(stream, &hello, frame::MAX_SOCKET).await?;
        let reply =
            frame::read_json(stream, 1024 * 1024).await?.ok_or_else(|| Error::Closed("the host hung up".into()))?;
        match reply.get("t").and_then(Value::as_str) {
            Some("welcome") => {
                let theirs = reply.get("proof").and_then(Value::as_str).unwrap_or("");
                if !token::matches(&proto::proof("host", token, &host_nonce, &nonce), theirs) {
                    return Err(Error::Denied("the host did not prove it knows the token".into()));
                }
                let info = BrowserInfo::from_json(&reply["browser"])
                    .ok_or_else(|| Error::Protocol("welcome without a browser".into()))?;
                let others = reply["others"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                Ok(Welcome { info, others })
            }
            Some("denied") => {
                Err(Error::Denied(reply.get("why").and_then(Value::as_str).unwrap_or("no reason given").to_string()))
            }
            _ => Err(Error::Protocol("the host answered neither welcome nor denied".into())),
        }
    };
    tokio::time::timeout(HANDSHAKE, run).await.map_err(|_| Error::Timeout(HANDSHAKE))?
}

/// The browsers whose hosts answer right now, and the ids that did not (with why).
pub async fn list(dirs: &Dirs) -> (Vec<Listing>, Vec<(String, String)>) {
    let token = match token::load(dirs) {
        Ok(t) => t,
        Err(e) => {
            return (Vec::new(), endpoint::list(dirs).into_iter().map(|f| (f.id, format!("no token: {e}"))).collect())
        }
    };
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for found in endpoint::list(dirs) {
        let tried = async {
            let mut s = endpoint::connect(dirs, &found.id).await?;
            handshake(&mut s, &token, "probe", "nexo", true).await
        };
        match tried.await {
            Ok(w) => ok.push(Listing { info: w.info, others: w.others }),
            Err(e) => bad.push((found.id, e.to_string())),
        }
    }
    (ok, bad)
}

pub struct Client {
    info: BrowserInfo,
    others: Vec<String>,
    out: mpsc::UnboundedSender<Value>,
    pending: Pending,
    next: AtomicU64,
    events: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
    alive: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
}

impl Client {
    /// Opens a session with browser `id`: connects to its host, handshakes with the install's token.
    pub async fn open(dirs: &Dirs, id: &str, client: &str, name: &str) -> Result<Client, Error> {
        let token = token::load(dirs)?;
        let stream = endpoint::connect(dirs, id).await?;
        Client::over(stream, &token, client, name).await
    }

    /// The same over any stream (tests use an in-memory one).
    pub async fn over(mut stream: BoxStream, token: &str, client: &str, name: &str) -> Result<Client, Error> {
        let welcome = handshake(&mut stream, token, client, name, false).await?;
        let (mut rd, mut wr) = tokio::io::split(stream);
        let (out, mut outbox) = mpsc::unbounded_channel::<Value>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Event>();
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));

        let writer = tokio::spawn(async move {
            while let Some(msg) = outbox.recv().await {
                if frame::write_json(&mut wr, &msg, frame::MAX_SOCKET).await.is_err() {
                    break;
                }
            }
        });
        let reader = {
            let (pending, alive) = (pending.clone(), alive.clone());
            tokio::spawn(async move {
                let why = loop {
                    let msg = match frame::read_json(&mut rd, frame::MAX_SOCKET).await {
                        Ok(Some(m)) => m,
                        Ok(None) => break "the host closed the connection".to_string(),
                        Err(e) => break e.to_string(),
                    };
                    match msg.get("t").and_then(Value::as_str) {
                        Some("res") | Some("consent") => {
                            let Some(id) = msg.get("id").and_then(Value::as_u64) else { continue };
                            let Some(tx) = lock(&pending).remove(&id) else { continue };
                            let answer = match msg.get("e").and_then(Value::as_str) {
                                Some(e) => Err(Error::Remote(e.to_string())),
                                None => Ok(msg.get("r").cloned().unwrap_or(msg)),
                            };
                            let _ = tx.send(answer);
                        }
                        Some("ev") => {
                            let (Some(tab), Some(method)) =
                                (msg.get("tab").and_then(Value::as_i64), msg.get("m").and_then(Value::as_str))
                            else {
                                continue;
                            };
                            let params = msg.get("p").cloned().unwrap_or(Value::Null);
                            let _ = ev_tx.send(Event { tab, method: method.to_string(), params });
                        }
                        Some("gone") => {
                            break msg.get("why").and_then(Value::as_str).unwrap_or("the browser went away").to_string()
                        }
                        _ => {}
                    }
                };
                alive.store(false, Ordering::SeqCst);
                for (_, tx) in lock(&pending).drain() {
                    let _ = tx.send(Err(Error::Closed(why.clone())));
                }
            })
        };
        Ok(Client {
            info: welcome.info,
            others: welcome.others,
            out,
            pending,
            next: AtomicU64::new(1),
            events: Mutex::new(Some(ev_rx)),
            alive,
            tasks: vec![writer, reader],
        })
    }

    pub fn info(&self) -> &BrowserInfo {
        &self.info
    }

    /// Names of the other agents that were using this browser when this session began.
    pub fn others(&self) -> &[String] {
        &self.others
    }

    /// False once the connection is gone.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// The stream of events from the session's tabs; it can be taken once.
    pub fn take_events(&self) -> Option<mpsc::UnboundedReceiver<Event>> {
        lock(&self.events).take()
    }

    async fn call(&self, msg: impl FnOnce(u64) -> Value, wait: Duration) -> Result<Value, Error> {
        if !self.is_alive() {
            return Err(Error::Closed("the connection is already closed".into()));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        if self.out.send(msg(id)).is_err() {
            lock(&self.pending).remove(&id);
            return Err(Error::Closed("the connection is already closed".into()));
        }
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(Error::Closed("the connection closed while waiting".into())),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(Error::Timeout(wait))
            }
        }
    }

    /// One request to the browser: `tabs.list`, `tabs.create`, `tabs.close`, `tabs.activate`, `window.resize`, `cdp`.
    pub async fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value, Error> {
        self.call(|id| json!({"t": "req", "id": id, "m": method, "p": params}), wait).await
    }

    /// Asks the person, in the browser, to allow nexo to control it, and waits for the answer.
    pub async fn consent(&self) -> Result<(), Error> {
        let wait = Duration::from_secs(proto::CONSENT_SECS + 10);
        let answer = self.call(|id| json!({"t": "connect", "id": id}), wait).await?;
        if answer.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            Err(Error::Denied(
                answer.get("why").and_then(Value::as_str).unwrap_or("the person did not allow it").to_string(),
            ))
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}
