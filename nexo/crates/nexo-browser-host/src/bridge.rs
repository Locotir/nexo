use nexo_relay::chunk::{self, Reassembler};
use nexo_relay::endpoint::{BoxStream, Dirs, Listener};
use nexo_relay::frame::{self, MAX_FROM_CHROME, MAX_SOCKET, MAX_TO_CHROME};
use nexo_relay::{proto, token, BrowserInfo};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// Agents that may be connected at once.
const MAX_SESSIONS: usize = 16;
/// Requests one session may have waiting for the browser.
const MAX_INFLIGHT: usize = 64;
/// Longest the host waits for the extension to report the person's answer.
const CONSENT_WAIT: Duration = Duration::from_secs(proto::CONSENT_SECS + 5);
const HANDSHAKE: Duration = Duration::from_secs(5);

/// Requests an agent can make that name a tab.
const TAB_METHODS: [&str; 4] = ["tabs.close", "tabs.activate", "window.resize", "cdp"];
/// Requests that name none.
const FREE_METHODS: [&str; 2] = ["tabs.list", "tabs.create"];

struct Session {
    name: String,
    to_agent: mpsc::UnboundedSender<Value>,
    granted: bool,
    /// The id of the `connect` the person has not answered yet.
    consent: Option<u64>,
    inflight: HashMap<u64, String>,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
    /// tab id -> session that owns it
    owners: HashMap<i64, String>,
}

struct Shared {
    state: Mutex<State>,
    to_ext: mpsc::UnboundedSender<Value>,
    token: String,
    info: BrowserInfo,
    next_session: AtomicU64,
}

fn lock(m: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn res_err(id: u64, why: &str) -> Value {
    json!({"t": "res", "id": id, "e": why})
}

/// The host as Chrome starts it: native messaging on stdin and stdout.
pub async fn run_stdio() -> io::Result<()> {
    let dirs = Dirs::for_user();
    let token = token::load_or_create(&dirs)?;
    run(tokio::io::stdin(), tokio::io::stdout(), dirs, token).await
}

/// Runs the host over the given extension streams until the extension goes away.
pub async fn run<R, W>(mut ext_in: R, mut ext_out: W, dirs: Dirs, token: String) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (to_ext, mut ext_box) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut cid = 0u64;
        while let Some(msg) = ext_box.recv().await {
            cid += 1;
            for piece in chunk::encode(&msg, cid) {
                if piece.len() > MAX_TO_CHROME || frame::write_frame(&mut ext_out, &piece).await.is_err() {
                    return;
                }
            }
        }
    });

    let mut parts = Reassembler::new();
    let mut shared: Option<Arc<Shared>> = None;
    let mut acceptor: Option<tokio::task::JoinHandle<()>> = None;
    let outcome = loop {
        let raw = match frame::read_json(&mut ext_in, MAX_FROM_CHROME).await {
            Ok(Some(v)) => v,
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        };
        let msg = match parts.push(raw) {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(e) => break Err(io::Error::new(io::ErrorKind::InvalidData, e)),
        };
        match &shared {
            None => {
                // Nothing is served until the extension says which browser it is.
                if msg.get("t").and_then(Value::as_str) != Some("hello") {
                    continue;
                }
                let Some(info) = BrowserInfo::from_json(&msg["browser"]) else {
                    let _ = to_ext.send(json!({"t": "error", "why": "hello without a valid browser id"}));
                    break Err(io::Error::new(io::ErrorKind::InvalidData, "hello without a valid browser id"));
                };
                let mut listener = match Listener::bind(&dirs, &info.id) {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = to_ext.send(json!({"t": "error", "why": e.to_string()}));
                        break Err(e);
                    }
                };
                let s = Arc::new(Shared {
                    state: Mutex::default(),
                    to_ext: to_ext.clone(),
                    token: token.clone(),
                    info,
                    next_session: AtomicU64::new(1),
                });
                let _ = to_ext.send(json!({"t": "ready", "endpoint": listener.location()}));
                let for_agents = s.clone();
                acceptor = Some(tokio::spawn(async move {
                    while let Ok(stream) = listener.accept().await {
                        tokio::spawn(serve_agent(for_agents.clone(), stream));
                    }
                }));
                shared = Some(s);
            }
            Some(s) => from_extension(s, msg),
        }
    };
    if let Some(a) = acceptor {
        a.abort();
    }
    if let Some(s) = shared {
        let sessions: Vec<_> = lock(&s.state).sessions.values().map(|x| x.to_agent.clone()).collect();
        for tx in sessions {
            let _ = tx.send(json!({"t": "gone", "why": "the browser closed its connection to nexo"}));
        }
    }
    // Give the farewells (and an error report to the extension) a moment to be written before the process ends.
    tokio::time::sleep(Duration::from_millis(100)).await;
    writer.abort();
    outcome
}

/// One message from the extension.
fn from_extension(s: &Arc<Shared>, msg: Value) {
    let kind = msg.get("t").and_then(Value::as_str).unwrap_or("");
    let sid = msg.get("sid").and_then(Value::as_str).unwrap_or("").to_string();
    let mut st = lock(&s.state);
    match kind {
        "ping" => {
            let _ = s.to_ext.send(json!({"t": "pong"}));
        }
        "res" => {
            let Some(id) = msg.get("id").and_then(Value::as_u64) else { return };
            let Some(sess) = st.sessions.get_mut(&sid) else { return };
            let method = sess.inflight.remove(&id).unwrap_or_default();
            let tx = sess.to_agent.clone();
            let reply = match msg.get("e").and_then(Value::as_str) {
                Some(e) => res_err(id, e),
                None => {
                    let mut r = msg.get("r").cloned().unwrap_or(Value::Null);
                    match method.as_str() {
                        "tabs.create" => {
                            if let Some(tab) = r.get("tab").and_then(Value::as_i64) {
                                st.owners.insert(tab, sid.clone());
                            }
                        }
                        "tabs.close" => {
                            if let Some(tab) = r.get("tab").and_then(Value::as_i64) {
                                st.owners.remove(&tab);
                            }
                        }
                        "tabs.list" => {
                            // The extension lists the session's tabs; the host does not trust it to be exact.
                            if let Some(list) = r.get_mut("tabs").and_then(Value::as_array_mut) {
                                list.retain(|t| {
                                    t.get("tab")
                                        .and_then(Value::as_i64)
                                        .is_some_and(|n| st.owners.get(&n) == Some(&sid))
                                });
                            }
                        }
                        _ => {}
                    }
                    json!({"t": "res", "id": id, "r": r})
                }
            };
            let _ = tx.send(reply);
        }
        "ev" => {
            let (Some(tab), Some(method)) =
                (msg.get("tab").and_then(Value::as_i64), msg.get("m").and_then(Value::as_str))
            else {
                return;
            };
            let params = msg.get("p").cloned().unwrap_or(Value::Null);
            let owner = match method {
                // A tab one of the session's tabs opened belongs to the same session.
                "Nexo.tabOpened" => {
                    let parent = params.get("opener").and_then(Value::as_i64).and_then(|o| st.owners.get(&o).cloned());
                    if let Some(p) = &parent {
                        st.owners.insert(tab, p.clone());
                    }
                    parent
                }
                "Nexo.tabClosed" => st.owners.remove(&tab),
                _ => st.owners.get(&tab).cloned(),
            };
            if let Some(sess) = owner.and_then(|o| st.sessions.get(&o)) {
                let _ = sess.to_agent.send(json!({"t": "ev", "tab": tab, "m": method, "p": params}));
            }
        }
        "consent_result" => {
            let Some(sess) = st.sessions.get_mut(&sid) else { return };
            let Some(id) = sess.consent.take() else { return };
            let ok = msg.get("ok").and_then(Value::as_bool) == Some(true);
            sess.granted = ok;
            let mut reply = json!({"t": "consent", "id": id, "ok": ok});
            if !ok {
                reply["why"] =
                    json!(clean(msg.get("why").and_then(Value::as_str).unwrap_or("the person did not allow it"), 200));
            }
            let _ = sess.to_agent.send(reply);
        }
        _ => {}
    }
}

/// One agent connection, from handshake to hang-up.
async fn serve_agent(s: Arc<Shared>, mut stream: BoxStream) {
    let Some((client_name, probe)) = agent_handshake(&s, &mut stream).await else { return };
    if probe {
        return;
    }
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (to_agent, mut agent_box) = mpsc::unbounded_channel::<Value>();
    let sid = format!("s{}", s.next_session.fetch_add(1, Ordering::SeqCst));
    {
        let mut st = lock(&s.state);
        if st.sessions.len() >= MAX_SESSIONS {
            let _ = to_agent.send(json!({"t": "gone", "why": "too many nexo sessions are connected to this browser"}));
        } else {
            st.sessions.insert(
                sid.clone(),
                Session {
                    name: client_name.clone(),
                    to_agent: to_agent.clone(),
                    granted: false,
                    consent: None,
                    inflight: HashMap::new(),
                },
            );
        }
    }
    let _ = s.to_ext.send(json!({"t": "session", "sid": sid, "name": client_name, "open": true}));
    let writer = tokio::spawn(async move {
        while let Some(msg) = agent_box.recv().await {
            if frame::write_json(&mut wr, &msg, MAX_SOCKET).await.is_err() {
                break;
            }
        }
    });
    while let Ok(Some(msg)) = frame::read_json(&mut rd, MAX_SOCKET).await {
        from_agent(&s, &sid, &to_agent, msg);
    }
    {
        let mut st = lock(&s.state);
        st.sessions.remove(&sid);
        st.owners.retain(|_, o| *o != sid);
    }
    let _ = s.to_ext.send(json!({"t": "session", "sid": sid, "open": false}));
    writer.abort();
}

/// The challenge, the agent's proof and the host's. Returns the agent's name and whether it only wanted to look.
async fn agent_handshake(s: &Arc<Shared>, stream: &mut BoxStream) -> Option<(String, bool)> {
    let run = async {
        let nonce = proto::nonce();
        frame::write_json(stream, &json!({"t": "challenge", "v": proto::VERSION, "nonce": nonce}), 4096).await.ok()?;
        let hello = frame::read_json(stream, 64 * 1024).await.ok()??;
        let their_nonce = hello.get("nonce").and_then(Value::as_str).unwrap_or("");
        let proof = hello.get("proof").and_then(Value::as_str).unwrap_or("");
        let right = hello.get("t").and_then(Value::as_str) == Some("hello")
            && token::matches(&proto::proof("agent", &s.token, &nonce, their_nonce), proof);
        if !right {
            // A wrong guess costs a little time, and says nothing about what was wrong.
            tokio::time::sleep(Duration::from_millis(250)).await;
            let _ = frame::write_json(stream, &json!({"t": "denied", "why": "the token did not match"}), 4096).await;
            return None;
        }
        let name = clean(hello.get("name").and_then(Value::as_str).unwrap_or("nexo"), 40);
        let probe = hello.get("probe").and_then(Value::as_bool) == Some(true);
        let others: Vec<String> = lock(&s.state).sessions.values().map(|x| x.name.clone()).collect();
        let welcome = json!({
            "t": "welcome", "browser": s.info.to_json(), "others": others,
            "proof": proto::proof("host", &s.token, &nonce, their_nonce),
        });
        frame::write_json(stream, &welcome, 64 * 1024).await.ok()?;
        Some((if name.is_empty() { "nexo".to_string() } else { name }, probe))
    };
    tokio::time::timeout(HANDSHAKE, run).await.ok().flatten()
}

/// One message from an agent, checked before anything of it reaches the browser.
fn from_agent(s: &Arc<Shared>, sid: &str, to_agent: &mpsc::UnboundedSender<Value>, msg: Value) {
    let Some(id) = msg.get("id").and_then(Value::as_u64) else { return };
    let mut st = lock(&s.state);
    let Some(sess) = st.sessions.get_mut(sid) else { return };
    match msg.get("t").and_then(Value::as_str) {
        Some("connect") => {
            if sess.granted {
                let _ = to_agent.send(json!({"t": "consent", "id": id, "ok": true}));
            } else if sess.consent.is_some() {
                let _ = to_agent.send(json!({"t": "consent", "id": id, "ok": false, "why": "another request is already waiting for the person"}));
            } else {
                sess.consent = Some(id);
                let _ = s.to_ext.send(json!({"t": "consent", "sid": sid, "name": sess.name}));
                let (shared, sid) = (s.clone(), sid.to_string());
                tokio::spawn(async move {
                    tokio::time::sleep(CONSENT_WAIT).await;
                    let mut st = lock(&shared.state);
                    if let Some(sess) = st.sessions.get_mut(&sid) {
                        if sess.consent == Some(id) {
                            sess.consent = None;
                            let _ = sess.to_agent.send(json!({"t": "consent", "id": id, "ok": false, "why": "nobody answered the prompt in the browser"}));
                            let _ = shared.to_ext.send(json!({"t": "consent_cancel", "sid": sid}));
                        }
                    }
                });
            }
        }
        Some("req") => {
            let method = msg.get("m").and_then(Value::as_str).unwrap_or("").to_string();
            let params = msg.get("p").cloned().unwrap_or(Value::Null);
            if !sess.granted {
                let _ = to_agent.send(res_err(
                    id,
                    "not allowed: the person has not allowed nexo to control this browser (ask them with connect)",
                ));
                return;
            }
            if sess.inflight.len() >= MAX_INFLIGHT {
                let _ = to_agent.send(res_err(id, "too many requests are waiting for the browser"));
                return;
            }
            if TAB_METHODS.contains(&method.as_str()) {
                let tab = params.get("tab").and_then(Value::as_i64);
                if tab.is_none_or(|t| st.owners.get(&t).map(String::as_str) != Some(sid)) {
                    let _ = to_agent.send(res_err(id, "that tab is not one of this session's tabs"));
                    return;
                }
            } else if !FREE_METHODS.contains(&method.as_str()) {
                let _ = to_agent.send(res_err(id, &format!("unknown request {}", clean(&method, 40))));
                return;
            }
            // Re-borrow: the ownership check above needed the whole state.
            let Some(sess) = st.sessions.get_mut(sid) else { return };
            sess.inflight.insert(id, method.clone());
            let _ = s.to_ext.send(json!({"t": "req", "sid": sid, "id": id, "m": method, "p": params}));
        }
        _ => {}
    }
}
