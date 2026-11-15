use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex, Notify};

use super::table::uri;
use super::{FIRST, INIT, SETTLE};
/// uri → (the version the diagnostics are for, the diagnostics).
pub(super) type Diags = Arc<Mutex<HashMap<String, (Option<i64>, Vec<Value>)>>>;

pub(super) type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

/// One running server.
pub(super) struct Server {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: Mutex<i64>,
    /// uri → (the version the diagnostics are for, the diagnostics).
    diags: Diags,
    published: Arc<Notify>,
    /// Work the server reported begun and not yet ended (`$/progress`): indexing, `cargo check`.
    busy: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// uri → the version last sent.
    open: Mutex<HashMap<String, i64>>,
    /// The server's output ended: it exited or died.
    pub(super) dead: Arc<AtomicBool>,
    /// The server's process, for ending its whole tree on Windows (a launcher may stand in front).
    #[cfg_attr(not(windows), allow(dead_code))]
    pid: Option<u32>,
    _child: Child,
}

#[cfg(windows)]
impl Drop for Server {
    fn drop(&mut self) {
        // `kill_on_drop` ends the process it started, not what that started; an orphan keeps the pipes open
        crate::bash::kill_group(self.pid);
    }
}

pub(super) async fn send(stdin: &Mutex<ChildStdin>, msg: &Value) -> std::io::Result<()> {
    let body = msg.to_string();
    let mut s = stdin.lock().await;
    s.write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await?;
    s.flush().await
}

/// What `initialize` sends. rust-analyzer is told not to build: build scripts, proc macros and the
/// check on save run the project's own code, and a diagnostics call must only read.
pub(super) fn initialize_params(program: &str, root: &Path) -> Value {
    let mut p = json!({
        "processId": std::process::id(),
        "rootUri": uri(root),
        "workspaceFolders": [{"uri": uri(root), "name": "root"}],
        "capabilities": {
            "textDocument": {"publishDiagnostics": {"versionSupport": true}},
            "window": {"workDoneProgress": true}
        },
    });
    if program == "rust-analyzer" {
        p["initializationOptions"] = json!({
            "cargo": {"buildScripts": {"enable": false}},
            "procMacro": {"enable": false},
            "checkOnSave": false
        });
    }
    p
}

impl Server {
    pub(super) async fn start(cmd: &[&str], root: &Path) -> Result<Server, String> {
        let program = nexo_core::shell::find_program(cmd[0]).unwrap_or_else(|| cmd[0].into());
        let mut child = tokio::process::Command::new(program);
        if let Some(env) = crate::child_env() {
            child.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        let mut child = child
            .args(&cmd[1..])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", cmd[0]))?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let pending: Pending = Arc::default();
        let diags: Diags = Arc::default();
        let published = Arc::new(Notify::new());
        let stdin = Arc::new(Mutex::new(stdin));
        // the reader: answers go to their callers, diagnostics to the map, requests get null
        let busy: Arc<std::sync::Mutex<std::collections::HashSet<String>>> = Arc::default();
        let (p, dg, pb, bz) = (pending.clone(), diags.clone(), published.clone(), busy.clone());
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = tokio::spawn(async move {
            let mut r = BufReader::new(stdout);
            loop {
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    match r.read_line(&mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.strip_prefix("Content-Length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut buf = vec![0u8; len];
                if r.read_exact(&mut buf).await.is_err() {
                    return;
                }
                let Ok(msg) = serde_json::from_slice::<Value>(&buf) else { continue };
                match (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
                    (Some(id), None) => {
                        let waiting = match id.as_i64() {
                            Some(i) => p.lock().await.remove(&i),
                            None => None,
                        };
                        if let Some(tx) = waiting {
                            let _ = tx.send(msg.get("result").cloned().unwrap_or(Value::Null));
                        }
                    }
                    (Some(id), Some(_)) => {
                        // a request from the server (configuration, progress tokens): a plain answer
                        let _ = reply_tx.send(json!({"jsonrpc": "2.0", "id": id, "result": Value::Null}));
                    }
                    (None, Some("$/progress")) => {
                        let token = msg["params"]["token"].to_string();
                        if let Ok(mut b) = bz.lock() {
                            match msg["params"]["value"]["kind"].as_str() {
                                Some("begin") => {
                                    b.insert(token);
                                }
                                Some("end") => {
                                    b.remove(&token);
                                }
                                _ => {}
                            }
                        }
                        pb.notify_waiters();
                    }
                    (None, Some("textDocument/publishDiagnostics")) => {
                        let params = &msg["params"];
                        if let Some(u) = params["uri"].as_str() {
                            let list = params["diagnostics"].as_array().cloned().unwrap_or_default();
                            dg.lock().await.insert(u.to_string(), (params["version"].as_i64(), list));
                            pb.notify_waiters();
                        }
                    }
                    _ => {}
                }
            }
        });
        // the server's output ended (it died or exited): nobody will answer, fail the waiters now
        {
            let (pending, dead, pb) = (pending.clone(), dead.clone(), published.clone());
            tokio::spawn(async move {
                let _ = reader.await;
                dead.store(true, Ordering::SeqCst);
                pending.lock().await.clear();
                pb.notify_waiters();
            });
        }
        // answers to the server's own requests, written as they come
        let writer = stdin.clone();
        tokio::spawn(async move {
            while let Some(m) = reply_rx.recv().await {
                let _ = send(&writer, &m).await;
            }
        });
        let server = Server {
            stdin,
            pending,
            next_id: Mutex::new(1),
            diags,
            published,
            busy,
            dead,
            open: Mutex::default(),
            pid,
            _child: child,
        };
        let init = server.request("initialize", initialize_params(cmd[0], root), INIT).await?;
        let _ = init;
        server.notify("initialized", json!({})).await?;
        Ok(server)
    }

    async fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        let id = {
            let mut n = self.next_id.lock().await;
            *n += 1;
            *n
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if self.dead.load(Ordering::SeqCst) {
            return Err("the language server went away".into());
        }
        send(&self.stdin, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
            .map_err(|e| format!("the language server went away: {e}"))?;
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err("the language server went away".into()),
            Err(_) => Err(format!("the language server did not answer `{method}` in {} s", wait.as_secs())),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        send(&self.stdin, &json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
            .map_err(|e| format!("the language server went away: {e}"))
    }

    /// Sends the file as it is on disk and waits for its diagnostics.
    pub(super) async fn check(&self, file: &Path, language: &str) -> Result<Vec<Value>, String> {
        self.check_with(file, language, FIRST, SETTLE).await
    }

    /// [`Server::check`] with its own limit for the first answer and for the quiet after it.
    pub(super) async fn check_with(
        &self,
        file: &Path,
        language: &str,
        first: Duration,
        settle: Duration,
    ) -> Result<Vec<Value>, String> {
        let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let u = uri(file);
        let version = {
            let mut open = self.open.lock().await;
            let v = open.get(&u).map(|v| v + 1).unwrap_or(1);
            open.insert(u.clone(), v);
            v
        };
        self.diags.lock().await.remove(&u);
        if version == 1 {
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument": {"uri": u, "languageId": language, "version": version, "text": text}}),
            )
            .await?;
        } else {
            self.notify(
                "textDocument/didChange",
                json!({"textDocument": {"uri": u, "version": version}, "contentChanges": [{"text": text}]}),
            )
            .await?;
            self.notify("textDocument/didSave", json!({"textDocument": {"uri": u}})).await?;
        }
        // the first publication for this version, then whatever follows soon after
        let deadline = tokio::time::Instant::now() + first;
        loop {
            let notified = self.published.notified();
            if let Some((v, _)) = self.diags.lock().await.get(&u) {
                if v.is_none_or(|v| v >= version) {
                    break;
                }
            }
            if self.dead.load(Ordering::SeqCst) {
                return Err("the language server went away".into());
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(format!("the language server said nothing of the file in {} s", first.as_secs()));
            }
        }
        // then until the server is quiet: nothing new for a moment and no work in progress
        // (rust-analyzer publishes an empty list while it still indexes, then the real one)
        loop {
            let notified = self.published.notified();
            let quiet = tokio::time::timeout(settle, notified).await.is_err();
            let busy = self.busy.lock().map(|b| !b.is_empty()).unwrap_or(false);
            if quiet && !busy {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        Ok(self.diags.lock().await.get(&u).map(|(_, d)| d.clone()).unwrap_or_default())
    }
}
