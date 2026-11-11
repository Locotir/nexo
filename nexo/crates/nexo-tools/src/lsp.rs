//! `diagnostics`: what a language server says of a file (errors, warnings), so
//! the agent can check an edit the way an editor would. One server per
//! language and project root, started on the first call and kept for the
//! session: rust-analyzer, pyright (or pylsp), typescript-language-server,
//! gopls, clangd, whichever is installed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex, Notify};

use crate::checkers;
use crate::util::{display_path, resolve, str_arg};

/// How long the first answer of a server may take (it indexes the project first).
const FIRST: Duration = Duration::from_secs(90);
/// How long `initialize` may take: a live server answers it in a moment (indexing comes after).
const INIT: Duration = Duration::from_secs(20);
/// After diagnostics arrive, how long to wait for more (servers publish in waves).
const SETTLE: Duration = Duration::from_millis(1500);

/// The most an `edit`/`write` waits for an already running server before it reports nothing.
pub const QUICK: Duration = Duration::from_millis(1500);
/// After the first diagnostics of a quick check, how long to wait for more.
const QUICK_SETTLE: Duration = Duration::from_millis(100);

/// The servers for a file, by extension: `(language id, candidate commands)`.
pub fn servers_for(path: &Path) -> Option<(&'static str, &'static [&'static [&'static str]])> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => ("rust", &[&["rust-analyzer"]]),
        "py" | "pyi" => {
            ("python", &[&["pyright-langserver", "--stdio"], &["basedpyright-langserver", "--stdio"], &["pylsp"]])
        }
        "ts" | "tsx" => ("typescript", &[&["typescript-language-server", "--stdio"]]),
        "js" | "jsx" | "mjs" | "cjs" => ("javascript", &[&["typescript-language-server", "--stdio"]]),
        "go" => ("go", &[&["gopls"]]),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" => ("cpp", &[&["clangd"]]),
        _ => return None,
    })
}

/// Files that mark the root of a project.
const MARKERS: [&str; 8] = [
    "Cargo.toml",
    "pyproject.toml",
    "setup.py",
    "package.json",
    "tsconfig.json",
    "go.mod",
    "compile_commands.json",
    ".git",
];

/// The nearest ancestor of `file` with a project marker.
fn marker_root(file: &Path) -> Option<PathBuf> {
    file.ancestors().skip(1).find(|d| MARKERS.iter().any(|m| d.join(m).exists())).map(Path::to_path_buf)
}

/// The project root a server is started in: the nearest ancestor with a marker of the
/// language's projects, else the working directory.
fn root_for(file: &Path, cwd: &Path) -> PathBuf {
    let mut d = file.parent();
    while let Some(dir) = d {
        if MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
        if dir == cwd {
            break;
        }
        d = dir.parent();
    }
    cwd.to_path_buf()
}

fn which(cmd: &str) -> bool {
    nexo_core::shell::find_program(cmd).is_some()
}

/// `file:///…` for a path, Windows drive letters and spaces included.
fn uri(p: &Path) -> String {
    reqwest::Url::from_file_path(p).map(|u| u.to_string()).unwrap_or_else(|_| format!("file://{}", p.display()))
}

/// uri → (the version the diagnostics are for, the diagnostics).
type Diags = Arc<Mutex<HashMap<String, (Option<i64>, Vec<Value>)>>>;

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

/// One running server.
struct Server {
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
    dead: Arc<AtomicBool>,
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

async fn send(stdin: &Mutex<ChildStdin>, msg: &Value) -> std::io::Result<()> {
    let body = msg.to_string();
    let mut s = stdin.lock().await;
    s.write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await?;
    s.flush().await
}

/// What `initialize` sends. rust-analyzer is told not to build: build scripts, proc macros and the
/// check on save run the project's own code, and a diagnostics call must only read.
fn initialize_params(program: &str, root: &Path) -> Value {
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
    async fn start(cmd: &[&str], root: &Path) -> Result<Server, String> {
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
    async fn check(&self, file: &Path, language: &str) -> Result<Vec<Value>, String> {
        self.check_with(file, language, FIRST, SETTLE).await
    }

    /// [`Server::check`] with its own limit for the first answer and for the quiet after it.
    async fn check_with(
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

/// The running servers, by command and root.
type Running = Mutex<HashMap<(String, PathBuf), Arc<Server>>>;

fn servers() -> &'static Running {
    static S: OnceLock<Running> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// The server already running for `file`: same language, a root above the file (the deepest wins).
async fn running_for(file: &Path, candidates: &[&[&str]]) -> Option<(Arc<Server>, PathBuf)> {
    let table = servers().lock().await;
    table
        .iter()
        .filter(|((cmd, root), s)| {
            file.starts_with(root)
                && !s.dead.load(Ordering::SeqCst)
                && candidates.iter().any(|c| cmd.starts_with(&c.join(" ")))
        })
        .max_by_key(|((_, root), _)| root.components().count())
        .map(|((_, root), s)| (s.clone(), root.clone()))
}

/// Errors and warnings of `file` from a server that is already running, as `(line, text)` with errors
/// first. `None` when there is no such server or it does not answer within `bound`: an edit never waits
/// longer, and never starts a server.
pub async fn quick(file: &Path, bound: Duration) -> Option<Vec<(u64, String)>> {
    let (language, candidates) = servers_for(file)?;
    let work = async {
        let (server, root) = running_for(file, candidates).await?;
        let list = server.check_with(file, language, bound, QUICK_SETTLE).await.ok()?;
        Some((list, root))
    };
    let (list, root) = tokio::time::timeout(bound, work).await.ok()??;
    let shown = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
    let mut rows: Vec<(u64, u64, String)> = list
        .iter()
        .filter_map(|d| {
            let sev = d["severity"].as_u64().unwrap_or(1);
            if sev > 2 {
                return None;
            }
            let line = d.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0) + 1;
            let col = d.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0) + 1;
            let msg = d["message"].as_str().unwrap_or("").lines().next().unwrap_or("");
            let word = if sev == 1 { "error" } else { "warning" };
            let src = d["source"].as_str().map(|s| format!(" ({s})")).unwrap_or_default();
            Some((sev, line, format!("{shown}:{line}:{col} {word}: {msg}{src}")))
        })
        .collect();
    rows.sort();
    Some(rows.into_iter().map(|(_, line, text)| (line, text)).collect())
}

/// Servers being started for an edit, so one start per language and root.
fn starting() -> &'static std::sync::Mutex<std::collections::HashSet<(String, PathBuf)>> {
    static S: OnceLock<std::sync::Mutex<std::collections::HashSet<(String, PathBuf)>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Starts, in the background, the server `file` would use if none runs yet, and gives it the file, so
/// the next edits find it warm. Never for Rust and Go (their compiler check answers) and never outside
/// a project; it does not wait and it fails silently.
pub fn warm(file: &Path) {
    let Some((language, candidates)) = servers_for(file) else { return };
    if matches!(language, "rust" | "go") {
        return;
    }
    let Some(cmd) = candidates.iter().find(|c| usable(c)) else { return };
    let Some(root) = marker_root(file) else { return };
    let env = crate::child_env().map(|e| format!(" {e:?}")).unwrap_or_default();
    let key = (format!("{}{env}", cmd.join(" ")), root.clone());
    let file = file.to_path_buf();
    tokio::spawn(async move {
        if servers().lock().await.contains_key(&key) {
            return;
        }
        let fresh = starting().lock().map(|mut s| s.insert(key.clone())).unwrap_or(false);
        if !fresh {
            return;
        }
        let started = Server::start(cmd, &root).await;
        if let Ok(server) = started {
            let server = Arc::new(server);
            servers().lock().await.entry(key.clone()).or_insert_with(|| server.clone());
            // opens the file, so the server begins to index and check it
            let _ = server.check_with(&file, language, FIRST, SETTLE).await;
        }
        if let Ok(mut s) = starting().lock() {
            s.remove(&key);
        }
    });
}

/// The diagnostics as lines a person (and a model) reads: `path:line:col severity message (source)`.
pub fn format(shown: &str, diags: &[Value]) -> String {
    if diags.is_empty() {
        return format!("{shown}: no problems found");
    }
    let mut rows: Vec<(u64, u64, u64, String)> = diags
        .iter()
        .map(|d| {
            let line = d.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0) + 1;
            let col = d.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0) + 1;
            let sev = d["severity"].as_u64().unwrap_or(1);
            let word = match sev {
                1 => "error",
                2 => "warning",
                3 => "info",
                _ => "hint",
            };
            let msg = d["message"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string();
            let src = d["source"].as_str().map(|s| format!(" ({s})")).unwrap_or_default();
            (sev, line, col, format!("{shown}:{line}:{col} {word}: {msg}{src}"))
        })
        .collect();
    rows.sort();
    let errors = rows.iter().filter(|r| r.0 == 1).count();
    let warnings = rows.iter().filter(|r| r.0 == 2).count();
    let mut out = format!(
        "{shown}: {errors} error{}, {warnings} warning{}\n",
        if errors == 1 { "" } else { "s" },
        if warnings == 1 { "" } else { "s" }
    );
    out.push_str(&rows.into_iter().map(|r| r.3).collect::<Vec<_>>().join("\n"));
    out
}

/// Whether a candidate can run at all: installed, and (for `rust-analyzer`, which a rustup
/// shim stands in for when the component is missing) answering `--version`.
fn usable(cmd: &[&str]) -> bool {
    if !which(cmd[0]) {
        return false;
    }
    if cmd[0] != "rust-analyzer" {
        return true;
    }
    let mut probe = std::process::Command::new(cmd[0]);
    if let Some(env) = crate::child_env() {
        probe.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    probe
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// One source file of each language the project has under `cwd` (shallow and bounded: three levels,
/// 5000 entries, no hidden, vendored or build folders).
fn sample_files(cwd: &Path) -> Vec<PathBuf> {
    const SKIP: [&str; 4] = ["node_modules", "target", "vendor", "dist"];
    let mut found: Vec<PathBuf> = Vec::new();
    let mut seen = 0usize;
    let mut dirs = vec![(cwd.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 5000 {
                break;
            }
            let path = e.path();
            let name = e.file_name();
            let name = name.to_string_lossy();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                if depth < 3 && !name.starts_with('.') && !SKIP.contains(&&*name) {
                    dirs.push((path, depth + 1));
                }
            } else if let Some((language, _)) = servers_for(&path) {
                if !found.iter().any(|f| servers_for(f).is_some_and(|(l, _)| l == language)) {
                    found.push(path);
                }
            }
        }
    }
    found
}

/// Whether `diagnostics` can answer anywhere under `cwd`: a language server (or the compiler check
/// that stands in for it) exists for a language the project has. A project with no source files yet
/// counts every language. It runs once, when the tools are chosen.
pub fn available(cwd: &Path) -> bool {
    let mut found = sample_files(cwd);
    if found.is_empty() {
        found = ["a.rs", "a.py", "a.ts", "a.js", "a.go", "a.c"].iter().map(PathBuf::from).collect();
    }
    found.iter().filter_map(|f| servers_for(f)).any(|(language, candidates)| {
        candidates.iter().any(|c| usable(c))
            || (language == "rust" && which("cargo"))
            || (language == "javascript" && which("node"))
    })
}

pub struct Diagnostics;

impl Diagnostics {
    /// The server failed: a compiler check if one applies, else the server's own error.
    async fn fallback(&self, file: &Path, root: &Path, language: &str, shown: &str, why: String) -> ToolOutput {
        match checkers::check(file, root, language, checkers::LIMIT).await {
            Some(Ok(d)) => ToolOutput::ok(format(shown, &d)),
            Some(Err(e)) => ToolOutput::error(format!("diagnostics: {why}; {e}")),
            None => ToolOutput::error(format!("diagnostics: {why}; run the compiler or the tests with bash instead")),
        }
    }
}

#[async_trait]
impl Tool for Diagnostics {
    fn name(&self) -> &str {
        "diagnostics"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "diagnostics".into(),
            description: "What the language server says of a file: compile errors, type errors, warnings, with \
                line and column. Call it after editing a file to check the edit as an editor would. Works for \
                Rust, Python, TypeScript/JavaScript, Go and C/C++ when their language server is installed; the \
                first call in a project may take a while as the server indexes it."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "The file."}},
                "required": ["path"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact> {
        Some(Compact { description: "Language-server errors and warnings of a file; call after editing.", params: &[] })
    }

    fn rare(&self) -> bool {
        true
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn summary(&self, args: &Value) -> String {
        format!("diagnostics {}", str_arg(args, "path").unwrap_or(""))
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(p) = str_arg(&args, "path") else { return ToolOutput::error("`path` is required") };
        let file = resolve(&ctx.cwd, p);
        let shown = display_path(&ctx.cwd, &file);
        if !file.is_file() {
            return ToolOutput::error(format!("diagnostics: {shown} is not a file"));
        }
        let Some((language, candidates)) = servers_for(&file) else {
            return ToolOutput::error(format!(
                "diagnostics: no language server for {shown} (Rust, Python, TypeScript/JavaScript, Go, C/C++); run the compiler or the tests with bash instead"
            ));
        };
        let root = root_for(&file, &ctx.cwd);
        let Some(cmd) = candidates.iter().find(|c| usable(c)) else {
            // no server: a quick checker of the project kind, or an answer at once
            ctx.report(format!("no {language} language server: using the compiler"));
            return match checkers::check(&file, &root, language, checkers::LIMIT).await {
                Some(Ok(d)) => ToolOutput::ok(format(&shown, &d)),
                Some(Err(e)) => ToolOutput::error(format!("diagnostics: {e}")),
                None => {
                    let names: Vec<&str> = candidates.iter().map(|c| c[0]).collect();
                    ToolOutput::error(format!(
                        "diagnostics: no checker applies to {shown}: no {language} language server is installed or working ({}); run the compiler or the tests with bash instead",
                        names.join(" or ")
                    ))
                }
            };
        };
        ctx.report(format!("asking {} in {}", cmd[0], display_path(&ctx.cwd, &root)));
        // a server started with another environment is another server
        let env = crate::child_env().map(|e| format!(" {e:?}")).unwrap_or_default();
        let key = (format!("{}{env}", cmd.join(" ")), root.clone());
        let server = {
            let mut table = servers().lock().await;
            match table.get(&key) {
                Some(s) => s.clone(),
                None => match Server::start(cmd, &root).await {
                    Ok(s) => {
                        let s = Arc::new(s);
                        table.insert(key.clone(), s.clone());
                        s
                    }
                    Err(e) => return self.fallback(&file, &root, language, &shown, e).await,
                },
            }
        };
        let checked = tokio::select! {
            r = server.check(&file, language) => r,
            _ = ctx.cancel.cancelled() => return ToolOutput::error("interrupted by the person"),
        };
        match checked {
            Ok(d) => ToolOutput::ok(format(&shown, &d)),
            Err(e) => {
                // a server that failed is started afresh next time
                servers().lock().await.remove(&key);
                self.fallback(&file, &root, language, &shown, e).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_by_extension_and_the_report() {
        assert_eq!(servers_for(Path::new("a/b.rs")).unwrap().0, "rust");
        assert_eq!(servers_for(Path::new("x.PY")).unwrap().0, "python");
        assert!(servers_for(Path::new("notes.md")).is_none());
        let d = vec![
            json!({"range": {"start": {"line": 4, "character": 2}}, "severity": 2, "message": "unused", "source": "rustc"}),
            json!({"range": {"start": {"line": 9, "character": 0}}, "severity": 1, "message": "mismatched types\nexpected u32"}),
        ];
        assert_eq!(
            format("src/a.rs", &d),
            "src/a.rs: 1 error, 1 warning\nsrc/a.rs:10:1 error: mismatched types\nsrc/a.rs:5:3 warning: unused (rustc)"
        );
        assert_eq!(format("a.py", &[]), "a.py: no problems found");
    }

    #[test]
    fn the_languages_of_a_project_skip_hidden_and_vendored_folders() {
        let d = tempfile::tempdir().unwrap();
        for f in ["a.py", "b.py", "src/c.rs", "notes.md", "node_modules/x.js", ".git/y.go", "target/z.c"] {
            let p = d.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let mut langs: Vec<&str> = sample_files(d.path()).iter().filter_map(|f| servers_for(f)).map(|s| s.0).collect();
        langs.sort();
        assert_eq!(langs, ["python", "rust"]);
    }

    #[test]
    fn initialize_params_disable_build_scripts() {
        let ra = initialize_params("rust-analyzer", Path::new("/p"));
        assert_eq!(ra["initializationOptions"]["cargo"]["buildScripts"]["enable"], false);
        assert_eq!(ra["initializationOptions"]["procMacro"]["enable"], false);
        assert_eq!(ra["initializationOptions"]["checkOnSave"], false);
        assert!(initialize_params("pyright-langserver", Path::new("/p")).get("initializationOptions").is_none());
    }

    /// A server that exits at once (a rustup shim without the component) fails `initialize` now, not at the limit.
    #[tokio::test]
    async fn a_server_that_exits_fails_fast() {
        let t = std::time::Instant::now();
        let r = Server::start(&["false"], Path::new("/")).await;
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }

    /// Against the real pyright, when it is installed: errors found, then gone once fixed.
    #[tokio::test]
    async fn pyright_finds_the_errors_and_sees_the_fix() {
        if !which("pyright-langserver") {
            eprintln!("pyright-langserver is not installed: skipped");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("pyproject.toml"), "[project]\nname = \"x\"\n").unwrap();
        let f = d.path().join("bad.py");
        std::fs::write(&f, "x: int = \"not a number\"\nprint(undefined_name)\n").unwrap();
        let ctx = ToolContext::new(d.path().to_path_buf());
        let out = Diagnostics.run(json!({"path": "bad.py"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("bad.py: 2 errors"), "{}", out.content);
        assert!(out.content.contains("bad.py:1:") && out.content.contains("bad.py:2:"), "{}", out.content);
        assert!(out.content.contains("undefined_name"), "{}", out.content);
        std::fs::write(&f, "x: int = 3\nprint(x)\n").unwrap();
        let out = Diagnostics.run(json!({"path": "bad.py"}), &ctx).await;
        assert_eq!(out.content, "bad.py: no problems found");
        let none = Diagnostics.run(json!({"path": "pyproject.toml"}), &ctx).await;
        assert!(none.is_error && none.content.contains("no language server"));
    }

    /// Against the real rust-analyzer, when it is installed: a type error in a small crate.
    #[tokio::test]
    async fn rust_analyzer_finds_a_type_error() {
        if !which("rust-analyzer")
            || std::process::Command::new("rust-analyzer")
                .arg("--version")
                .output()
                .map(|o| !o.status.success())
                .unwrap_or(true)
        {
            eprintln!("rust-analyzer is not usable here: skipped");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(
            d.path().join("src/main.rs"),
            "fn main() {\n    let n: u32 = \"three\";\n    println!(\"{n}\");\n}\n",
        )
        .unwrap();
        let ctx = ToolContext::new(d.path().to_path_buf());
        let out = Diagnostics.run(json!({"path": "src/main.rs"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("src/main.rs:2:") && out.content.contains("error"), "{}", out.content);
    }

    /// A language server written in Python: `BAD` in the text gives an error, a warning and a hint;
    /// `SLOW` makes it answer after five seconds; anything else is clean.
    const FAKE: &str = r#"
import sys, json, time
def read():
    n = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            n = int(line.split(b":")[1])
    return json.loads(sys.stdin.buffer.read(n))
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    sys.stdout.buffer.flush()
def publish(doc, text):
    if "SLOW" in text:
        time.sleep(5)
    d = []
    if "BAD" in text:
        r = {"start": {"line": 0, "character": 0}}
        d = [{"range": r, "severity": 3, "message": "hint only"},
             {"range": {"start": {"line": 2, "character": 4}}, "severity": 2, "message": "unused thing", "source": "fake"},
             {"range": r, "severity": 1, "message": "fake error\nsecond line", "source": "fake"}]
    send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
          "params": {"uri": doc["uri"], "version": doc.get("version"), "diagnostics": d}})
while True:
    m = read()
    if m is None:
        break
    meth = m.get("method")
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {}}})
    elif meth == "textDocument/didOpen":
        publish(m["params"]["textDocument"], m["params"]["textDocument"]["text"])
    elif meth == "textDocument/didChange":
        publish(m["params"]["textDocument"], m["params"]["contentChanges"][0]["text"])
"#;

    /// A project with a Python file and the fake server registered as the running `pylsp` there. The
    /// server leaves the table, and its process the machine, when this is dropped (even on a panic).
    struct Fake {
        dir: tempfile::TempDir,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            if let Ok(mut table) = servers().try_lock() {
                table.remove(&("pylsp".to_string(), self.dir.path().to_path_buf()));
            }
        }
    }

    async fn project_with_fake(text: &str) -> Option<(Fake, PathBuf)> {
        let Some(python) = ["python3", "python"].into_iter().find(|p| which(p)) else {
            eprintln!("python is not installed: skipped");
            return None;
        };
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("pyproject.toml"), "[project]\nname = \"x\"\n").unwrap();
        let script = d.path().join("fake_lsp.py");
        std::fs::write(&script, FAKE).unwrap();
        let file = d.path().join("m.py");
        std::fs::write(&file, text).unwrap();
        let script = script.to_string_lossy().into_owned();
        let server = Server::start(&[python, &script], d.path()).await.unwrap();
        servers().lock().await.insert(("pylsp".into(), d.path().to_path_buf()), Arc::new(server));
        Some((Fake { dir: d }, file))
    }

    #[test]
    fn an_edit_that_breaks_the_file_carries_the_server_errors() {
        crate::util::run_bounded(Duration::from_secs(60), "an_edit_that_breaks_the_file", async {
            let Some((d, file)) = project_with_fake("x = 1\n").await else { return };
            let ctx = ToolContext::new(d.dir.path().to_path_buf());
            ctx.seen.mark(&file);
            let out = crate::edit::Edit
                .run(json!({"path": "m.py", "old_string": "x = 1", "new_string": "BAD = 1"}), &ctx)
                .await;
            assert!(!out.is_error, "{}", out.content);
            assert!(
                out.content.contains(
                    "\ndiagnostics:\nm.py:1:1 error: fake error (fake)\nm.py:3:5 warning: unused thing (fake)"
                ),
                "{}",
                out.content
            );
            assert!(!out.content.contains("hint only") && !out.content.contains("second line"), "{}", out.content);
            // fixed: nothing is appended
            let out = crate::edit::Edit
                .run(json!({"path": "m.py", "old_string": "BAD = 1", "new_string": "y = 1"}), &ctx)
                .await;
            assert!(!out.is_error && !out.content.contains("diagnostics:"), "{}", out.content);
        });
    }

    #[test]
    fn a_slow_server_is_waited_for_only_a_moment_and_nothing_is_added() {
        crate::util::run_bounded(Duration::from_secs(60), "a_slow_server_is_waited_for", async {
            let Some((_d, file)) = project_with_fake("SLOW\n").await else { return };
            let t = std::time::Instant::now();
            assert!(quick(&file, Duration::from_millis(300)).await.is_none());
            assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
            let t = std::time::Instant::now();
            assert_eq!(crate::edit_check::report_within(&file, Duration::from_secs(20)).await, "");
            assert!(t.elapsed() < QUICK + Duration::from_secs(2), "{:?}", t.elapsed());
        });
    }

    #[tokio::test]
    async fn no_running_server_means_no_wait_and_no_start() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("pyproject.toml"), "").unwrap();
        let file = d.path().join("m.py");
        std::fs::write(&file, "BAD\n").unwrap();
        let t = std::time::Instant::now();
        assert!(quick(&file, QUICK).await.is_none());
        assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
        assert!(servers().lock().await.keys().all(|(_, root)| root != d.path()));
    }
}
