//! `nexo acp` as an editor would use it: the binary is spawned with the mock provider and
//! driven over its stdin and stdout by a small test client.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

/// What the test client answers to a request of the agent: a result, or an error (code, message).
type Answer = Result<Value, (i64, String)>;

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next: u64,
    /// Everything the agent sent, in order (requests and notifications; responses are returned).
    log: Vec<Value>,
}

struct Dirs {
    _root: tempfile::TempDir,
    config: PathBuf,
    work: PathBuf,
}

/// A config directory with the mock provider (playing `script` when given) and an empty work directory.
fn dirs(script: Option<&Value>) -> Dirs {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let work = root.path().join("work");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let mut toml = String::from(
        "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\"]\n",
    );
    if let Some(s) = script {
        let file = root.path().join("script.json");
        std::fs::write(&file, s.to_string()).unwrap();
        toml.push_str(&format!("extra = {{ script = {:?} }}\n", file.display().to_string()));
    }
    toml.push_str("[context]\npolicy = \"plain\"\n");
    std::fs::write(config.join("config.toml"), toml).unwrap();
    let work = std::fs::canonicalize(&work).unwrap();
    Dirs { _root: root, config, work }
}

impl Client {
    fn start(d: &Dirs) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nexo"))
            .arg("acp")
            .current_dir(&d.work)
            .env("NEXO_CONFIG_DIR", &d.config)
            .env("NEXO_DATA_DIR", d.config.join("data"))
            .env("NEXO_CACHE_DIR", d.config.join("cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("nexo acp starts");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take();
        Client { child, stdin, lines, next: 1, log: Vec::new() }
    }

    fn send(&mut self, v: Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Sends a request and reads until its response, answering the agent's own requests with
    /// `on_request`, which may also send notifications (a cancel) through the client.
    fn call_with(
        &mut self,
        method: &str,
        params: Value,
        on_request: &mut dyn FnMut(&mut Client, &Value) -> Answer,
    ) -> Result<Value, Value> {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(60)).expect("the agent answers in time");
            // nothing but protocol JSON on stdout
            let msg: Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"));
            assert_eq!(msg["jsonrpc"], "2.0", "{line}");
            if msg.get("method").is_some() {
                self.log.push(msg.clone());
                if msg.get("id").is_some() {
                    let reply = match on_request(self, &msg) {
                        Ok(r) => json!({"jsonrpc": "2.0", "id": msg["id"], "result": r}),
                        Err((code, message)) => {
                            json!({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": code, "message": message}})
                        }
                    };
                    self.send(reply);
                }
            } else if msg["id"] == json!(id) {
                return match msg.get("error") {
                    Some(e) => Err(e.clone()),
                    None => Ok(msg["result"].clone()),
                };
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        self.call_with(method, params, &mut |_, m| panic!("unexpected request of the agent: {m}"))
    }

    fn initialize(&mut self, fs: bool) -> Value {
        self.initialize_with(fs, false)
    }

    fn initialize_with(&mut self, fs: bool, terminal: bool) -> Value {
        self.call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": fs, "writeTextFile": fs}, "terminal": terminal},
                "clientInfo": {"name": "test", "version": "0"}
            }),
        )
        .expect("initialize")
    }

    fn new_session(&mut self, work: &Path) -> String {
        self.new_session_with(work, json!([]))
    }

    fn new_session_with(&mut self, work: &Path, mcp: Value) -> String {
        let r = self.call("session/new", json!({"cwd": work, "mcpServers": mcp})).expect("session/new");
        r["sessionId"].as_str().expect("a session id").to_string()
    }

    /// The `session/update` kinds in `log`, in order, with the whole update.
    fn updates(&self) -> Vec<(String, Value)> {
        self.log
            .iter()
            .filter(|m| m["method"] == "session/update")
            .map(|m| {
                (
                    m["params"]["update"]["sessionUpdate"].as_str().unwrap_or("").to_string(),
                    m["params"]["update"].clone(),
                )
            })
            .collect()
    }

    /// Closes stdin and waits for the process to leave.
    fn finish(mut self) {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        assert!(status.success(), "{status:?}");
    }
}

fn selected(id: &str) -> Answer {
    Ok(json!({"outcome": {"outcome": "selected", "optionId": id}}))
}

fn write_script() -> Value {
    json!([
        [{"text": "Writing the file."}, {"tool": {"id": "c1", "name": "write", "args": {"path": "hello.txt", "content": "hi\n"}}}],
        [{"text": "All done."}]
    ])
}

#[test]
fn initialize_and_new_session_speak_the_protocol() {
    let d = dirs(None);
    let mut c = Client::start(&d);
    let init = c.initialize(false);
    assert_eq!(init["protocolVersion"], 1);
    assert_eq!(init["agentCapabilities"]["loadSession"], true);
    assert_eq!(init["authMethods"], json!([]));
    assert_eq!(init["agentInfo"]["name"], "nexo");
    let sid = c.new_session(&d.work);
    let r = c.call("session/new", json!({"cwd": d.work, "mcpServers": []})).unwrap();
    assert_ne!(r["sessionId"], json!(sid), "a second session is another one");
    assert_eq!(r["modes"]["currentModeId"], "ask");
    let ids: Vec<&str> =
        r["modes"]["availableModes"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(ids, ["ask", "accept-edits", "auto", "read-only", "plan"]);
    // a method nobody knows, and a mode nexo does not have
    let e = c.call("session/frobnicate", json!({})).unwrap_err();
    assert_eq!(e["code"], -32601);
    let e = c.call("session/set_mode", json!({"sessionId": sid, "modeId": "yolo"})).unwrap_err();
    assert_eq!(e["code"], -32602);
    // a prompt of text the model echoes
    let r = c
        .call("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "hello there"}]}))
        .unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    let text: String = c
        .updates()
        .iter()
        .filter(|(k, _)| k == "agent_message_chunk")
        .filter_map(|(_, u)| u["content"]["text"].as_str().map(String::from))
        .collect();
    assert_eq!(text, "echo: hello there");
    c.finish();
}

#[test]
fn a_write_asks_the_client_and_streams_updates_in_order() {
    let d = dirs(Some(&write_script()));
    let mut c = Client::start(&d);
    c.initialize(false);
    let sid = c.new_session(&d.work);
    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "make hello.txt"}]}),
            &mut |_, m| {
                assert_eq!(m["method"], "session/request_permission");
                let p = &m["params"];
                assert_eq!(p["sessionId"], json!(sid));
                assert_eq!(p["toolCall"]["toolCallId"], "c1");
                assert_eq!(p["toolCall"]["kind"], "edit");
                assert_eq!(p["toolCall"]["content"][0]["type"], "diff");
                assert_eq!(p["toolCall"]["content"][0]["newText"], "hi\n");
                let kinds: Vec<&str> =
                    p["options"].as_array().unwrap().iter().filter_map(|o| o["kind"].as_str()).collect();
                assert!(kinds.contains(&"allow_once") && kinds.contains(&"reject_once"), "{kinds:?}");
                selected("allow")
            },
        )
        .unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    assert_eq!(std::fs::read_to_string(d.work.join("hello.txt")).unwrap(), "hi\n");

    // the order of everything the agent sent during the turn
    let seq: Vec<String> = c
        .log
        .iter()
        .map(|m| match m["method"].as_str().unwrap() {
            "session/update" => {
                let u = &m["params"]["update"];
                let kind = u["sessionUpdate"].as_str().unwrap();
                match kind {
                    "tool_call" | "tool_call_update" => format!("{kind}:{}", u["status"].as_str().unwrap()),
                    k => k.to_string(),
                }
            }
            other => other.to_string(),
        })
        .collect();
    let at = |what: &str| seq.iter().position(|s| s == what).unwrap_or_else(|| panic!("no {what} in {seq:?}"));
    let (first_text, call, ask, done) = (
        at("agent_message_chunk"),
        at("tool_call:pending"),
        at("session/request_permission"),
        at("tool_call_update:completed"),
    );
    assert!(first_text < call && call < ask && ask < done, "{seq:?}");
    assert_eq!(seq.last().unwrap(), "agent_message_chunk", "the closing text comes last: {seq:?}");
    // the finished call carries the diff and the path it touched
    let finished =
        c.updates().into_iter().find(|(k, u)| k == "tool_call_update" && u["status"] == "completed").unwrap().1;
    assert_eq!(finished["content"][0]["type"], "diff");
    assert_eq!(finished["content"][0]["path"], json!(d.work.join("hello.txt")));
    c.finish();
}

#[test]
fn a_rejected_write_does_not_happen() {
    let d = dirs(Some(&write_script()));
    let mut c = Client::start(&d);
    c.initialize(false);
    let sid = c.new_session(&d.work);
    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "x"}]}),
            &mut |_, _| selected("reject"),
        )
        .unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    assert!(!d.work.join("hello.txt").exists());
    let failed = c.updates().into_iter().any(|(k, u)| k == "tool_call_update" && u["status"] == "failed");
    assert!(failed, "the call is reported failed");
    c.finish();
}

#[test]
fn cancel_ends_the_turn_with_stop_reason_cancelled() {
    let d = dirs(Some(&write_script()));
    let mut c = Client::start(&d);
    c.initialize(false);
    let sid = c.new_session(&d.work);
    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "x"}]}),
            &mut |c, _| {
                // the person presses stop while the permission dialog is open
                c.notify("session/cancel", json!({"sessionId": sid}));
                Ok(json!({"outcome": {"outcome": "cancelled"}}))
            },
        )
        .unwrap();
    assert_eq!(r["stopReason"], "cancelled");
    assert!(!d.work.join("hello.txt").exists());
    c.finish();
}

#[test]
fn file_reads_and_writes_go_through_the_client() {
    let d = dirs(Some(&json!([
        [{"tool": {"id": "r1", "name": "read", "args": {"path": "buffer.txt"}}}],
        [{"text": "read it"}],
        [{"tool": {"id": "w1", "name": "write", "args": {"path": "out.txt", "content": "from the model\n"}}}],
        [{"text": "wrote it"}]
    ])));
    let mut c = Client::start(&d);
    c.initialize(true);
    let sid = c.new_session(&d.work);
    // set_mode: the person lets edits and reads go without asking
    c.call("session/set_mode", json!({"sessionId": sid, "modeId": "auto"})).unwrap();
    assert!(c.updates().iter().any(|(k, u)| k == "current_mode_update" && u["currentModeId"] == "auto"));

    let buffer = d.work.join("buffer.txt");
    let out = d.work.join("out.txt");
    let mut writes: Vec<Value> = Vec::new();
    let mut answer = |_: &mut Client, m: &Value| -> Answer {
        match m["method"].as_str().unwrap() {
            "fs/read_text_file" if m["params"]["path"] == json!(buffer) => Ok(json!({"content": "unsaved line\n"})),
            // the new file does not exist in the editor
            "fs/read_text_file" => Err((-32002, "not found".into())),
            "fs/write_text_file" => {
                writes.push(m["params"].clone());
                Ok(Value::Null)
            }
            other => panic!("unexpected {other}"),
        }
    };
    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "read it"}]}),
            &mut answer,
        )
        .unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    let read_done = c
        .updates()
        .into_iter()
        .find(|(k, u)| k == "tool_call_update" && u["toolCallId"] == "r1" && u["status"] == "completed");
    let text = read_done.expect("the read finished").1["content"][0]["content"]["text"].as_str().unwrap().to_string();
    assert!(text.contains("unsaved line"), "the model saw the buffer, not the disk: {text}");
    assert!(!buffer.exists());

    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "write it"}]}),
            &mut answer,
        )
        .unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0]["path"], json!(out));
    assert_eq!(writes[0]["content"], "from the model\n");
    assert_eq!(writes[0]["sessionId"], json!(sid));
    assert!(!out.exists(), "the editor writes the file, not nexo");
    c.finish();
}

#[test]
fn a_recorded_session_loads_and_replays_its_conversation() {
    let d = dirs(None);
    let mut c = Client::start(&d);
    c.initialize(false);
    let sid = c.new_session(&d.work);
    c.call("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "remember this"}]})).unwrap();
    c.finish();

    let mut c = Client::start(&d);
    c.initialize(false);
    let r = c.call("session/load", json!({"sessionId": sid, "cwd": d.work, "mcpServers": []})).expect("session/load");
    assert_eq!(r["modes"]["currentModeId"], "ask");
    let kinds: Vec<String> = c.updates().into_iter().map(|(k, _)| k).collect();
    assert_eq!(kinds, ["user_message_chunk", "agent_message_chunk"], "{kinds:?}");
    let text = c.updates()[0].1["content"]["text"].as_str().unwrap().to_string();
    assert!(text.contains("remember this"), "{text}");
    // the loaded session takes a prompt
    let r =
        c.call("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "and again"}]})).unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    c.finish();
    // an id nexo has not recorded is an error, not a new session
    let mut c = Client::start(&d);
    c.initialize(false);
    let e = c.call("session/load", json!({"sessionId": "nope", "cwd": d.work, "mcpServers": []})).unwrap_err();
    assert_eq!(e["code"], -32603, "{e}");
    c.finish();
}

// ---- terminals ----

/// A client terminal that runs nothing: it answers `terminal/*` the way an editor would and
/// records the methods it was asked, in order.
struct FakeTerminal {
    seen: Vec<(String, Value)>,
    /// Sends `session/cancel` when the agent starts waiting, and answers late.
    cancel_on_wait: Option<String>,
}

impl FakeTerminal {
    fn new() -> FakeTerminal {
        FakeTerminal { seen: Vec::new(), cancel_on_wait: None }
    }

    fn methods(&self) -> Vec<&str> {
        self.seen.iter().map(|(m, _)| m.as_str()).collect()
    }

    fn answer(&mut self, c: &mut Client, m: &Value) -> Answer {
        let method = m["method"].as_str().unwrap().to_string();
        assert!(method.starts_with("terminal/"), "unexpected request {method}");
        assert!(m["params"]["sessionId"].is_string(), "{m}");
        self.seen.push((method.clone(), m["params"].clone()));
        if method != "terminal/create" {
            assert_eq!(m["params"]["terminalId"], "term-1", "{m}");
        }
        match method.as_str() {
            "terminal/create" => Ok(json!({"terminalId": "term-1"})),
            "terminal/wait_for_exit" => {
                if let Some(sid) = self.cancel_on_wait.take() {
                    c.notify("session/cancel", json!({"sessionId": sid}));
                    // the cancel is handled before this answer, which nobody waits for any more
                    std::thread::sleep(Duration::from_millis(300));
                }
                Ok(json!({"exitCode": 0, "signal": null}))
            }
            "terminal/output" => Ok(json!({
                "output": "from the terminal\n", "truncated": false,
                "exitStatus": {"exitCode": 0, "signal": null}
            })),
            "terminal/kill" | "terminal/release" => Ok(Value::Null),
            other => panic!("unexpected {other}"),
        }
    }
}

fn bash_script(command: &str) -> Value {
    json!([
        [{"tool": {"id": "b1", "name": "bash", "args": {"command": command}}}],
        [{"text": "ran it"}]
    ])
}

#[test]
fn bash_runs_in_the_client_terminal_when_it_has_one() {
    let d = dirs(Some(&bash_script("echo hi > made.txt")));
    let mut c = Client::start(&d);
    let init = c.initialize_with(false, true);
    assert_eq!(init["agentCapabilities"]["mcpCapabilities"], json!({"http": true, "sse": true}));
    let sid = c.new_session(&d.work);
    c.call("session/set_mode", json!({"sessionId": sid, "modeId": "auto"})).unwrap();
    let mut term = FakeTerminal::new();
    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "run it"}]}),
            &mut |c, m| term.answer(c, m),
        )
        .unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    assert_eq!(term.methods(), ["terminal/create", "terminal/wait_for_exit", "terminal/output", "terminal/release"]);
    let create = &term.seen[0].1;
    assert_eq!(create["sessionId"], json!(sid));
    assert_eq!(create["cwd"], json!(d.work));
    let args: Vec<&str> = create["args"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(args.iter().any(|a| a.contains("echo hi > made.txt")), "the command reaches the terminal: {args:?}");
    assert!(!d.work.join("made.txt").exists(), "nexo did not run the command itself");

    // the call shows the terminal while it runs, and keeps it when it ends
    let calls: Vec<Value> = c
        .updates()
        .into_iter()
        .filter(|(k, u)| k == "tool_call_update" && u["toolCallId"] == "b1")
        .map(|(_, u)| u)
        .collect();
    let live = calls
        .iter()
        .find(|u| u["content"][0]["type"] == "terminal" && u["status"] == "in_progress")
        .expect("live terminal");
    assert_eq!(live["content"][0]["terminalId"], "term-1");
    let done = calls.last().unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["content"][0], json!({"type": "terminal", "terminalId": "term-1"}));
    assert!(done["rawOutput"]["content"].as_str().unwrap().contains("from the terminal"));
    c.finish();
}

#[test]
fn bash_runs_in_nexo_when_the_client_has_no_terminal() {
    let d = dirs(Some(&bash_script("echo hi > made.txt")));
    let mut c = Client::start(&d);
    let init = c.initialize_with(false, false);
    assert_eq!(init["agentCapabilities"]["mcpCapabilities"]["http"], true);
    let sid = c.new_session(&d.work);
    c.call("session/set_mode", json!({"sessionId": sid, "modeId": "auto"})).unwrap();
    let r =
        c.call("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "run it"}]})).unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    assert_eq!(std::fs::read_to_string(d.work.join("made.txt")).unwrap(), "hi\n");
    let done = c.updates().into_iter().find(|(k, u)| k == "tool_call_update" && u["status"] == "completed").unwrap().1;
    assert_eq!(done["content"][0]["type"], "content", "text, not a terminal: {done}");
    c.finish();
}

#[test]
fn cancelling_kills_and_releases_the_terminal() {
    let d = dirs(Some(&bash_script("sleep 30")));
    let mut c = Client::start(&d);
    c.initialize_with(false, true);
    let sid = c.new_session(&d.work);
    c.call("session/set_mode", json!({"sessionId": sid, "modeId": "auto"})).unwrap();
    let mut term = FakeTerminal::new();
    term.cancel_on_wait = Some(sid.clone());
    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "run it"}]}),
            &mut |c, m| term.answer(c, m),
        )
        .unwrap();
    assert_eq!(r["stopReason"], "cancelled");
    // the turn is over; the kill and the release may still be on their way
    while !term.methods().contains(&"terminal/release") {
        let line = c.lines.recv_timeout(Duration::from_secs(10)).expect("the terminal is released");
        let msg: Value = serde_json::from_str(&line).unwrap();
        if msg.get("method").is_some() && msg.get("id").is_some() {
            let result = term.answer(&mut c, &msg).unwrap();
            c.send(json!({"jsonrpc": "2.0", "id": msg["id"], "result": result}));
        }
    }
    let m = term.methods();
    let at = |what: &str| m.iter().position(|s| *s == what).unwrap_or_else(|| panic!("no {what} in {m:?}"));
    assert!(
        at("terminal/wait_for_exit") < at("terminal/kill") && at("terminal/kill") < at("terminal/release"),
        "{m:?}"
    );
    assert_eq!(m.last(), Some(&"terminal/release"));
    c.finish();
}

// ---- MCP servers over HTTP ----

/// (JSON-RPC method, `x-api-key` header) of each POST.
type Posts = std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

/// A one-thread-per-connection HTTP MCP server on 127.0.0.1 with one tool, `echo`.
struct MockMcp {
    url: String,
    posts: Posts,
}

fn mock_mcp() -> MockMcp {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let posts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = posts.clone();
    std::thread::spawn(move || {
        for sock in listener.incoming().map_while(Result::ok) {
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut sock = sock;
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    match sock.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let header =
                    |k: &str| head.lines().find_map(|l| l.strip_prefix(&format!("{k}:")).map(|v| v.trim().to_string()));
                let len: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                while buf.len() < end + len {
                    match sock.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let mut reply = |code: &str, ctype: &str, body: &str| {
                    let r = format!(
                        "HTTP/1.1 {code}\r\nconnection: close\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(r.as_bytes());
                };
                if !head.starts_with("post ") {
                    return reply("405 Method Not Allowed", "text/plain", "");
                }
                let msg: Value = serde_json::from_slice(&buf[end..]).unwrap_or(Value::Null);
                let method = msg["method"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((method.clone(), header("x-api-key")));
                let Some(id) = msg.get("id") else { return reply("202 Accepted", "text/plain", "") };
                let result = match method.as_str() {
                    "initialize" => json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                                           "serverInfo": {"name": "mock", "version": "1"}}),
                    "tools/list" => json!({"tools": [{"name": "echo", "description": "Echoes.",
                        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}),
                    "tools/call" => json!({"content": [{"type": "text",
                        "text": format!("echo: {}", msg["params"]["arguments"]["text"].as_str().unwrap_or(""))}]}),
                    _ => json!({}),
                };
                reply("200 OK", "application/json", &json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string());
            });
        }
    });
    MockMcp { url, posts }
}

#[test]
fn an_http_mcp_server_from_session_new_is_registered_and_called() {
    let mock = mock_mcp();
    let d = dirs(Some(&json!([
        [{"tool": {"id": "m1", "name": "mcp__mock__echo", "args": {"text": "hello"}}}],
        [{"text": "called it"}]
    ])));
    let mut c = Client::start(&d);
    let init = c.initialize(false);
    assert_eq!(init["agentCapabilities"]["mcpCapabilities"], json!({"http": true, "sse": true}));
    let sid = c.new_session_with(
        &d.work,
        json!([{"type": "http", "name": "mock", "url": mock.url, "headers": [{"name": "x-api-key", "value": "k-123"}]}]),
    );
    c.call("session/set_mode", json!({"sessionId": sid, "modeId": "auto"})).unwrap();
    let r = c.call("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "go"}]})).unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    let done = c
        .updates()
        .into_iter()
        .find(|(k, u)| k == "tool_call_update" && u["toolCallId"] == "m1" && u["status"] != "in_progress")
        .unwrap()
        .1;
    assert_eq!(done["status"], "completed", "{done}");
    assert!(done["rawOutput"]["content"].as_str().unwrap().contains("echo: hello"), "{done}");
    c.finish();
    let posts = mock.posts.lock().unwrap();
    let methods: Vec<&str> = posts.iter().map(|(m, _)| m.as_str()).collect();
    assert!(
        methods.contains(&"initialize") && methods.contains(&"tools/list") && methods.contains(&"tools/call"),
        "{methods:?}"
    );
    assert!(
        posts.iter().all(|(_, key)| key.as_deref() == Some("k-123")),
        "the header goes with every request: {posts:?}"
    );
}

#[test]
fn with_the_sandbox_on_bash_stays_in_nexo_even_if_the_client_has_a_terminal() {
    let d = dirs(Some(&bash_script("echo hi > made.txt")));
    let conf = d.config.join("config.toml");
    let mut toml = std::fs::read_to_string(&conf).unwrap();
    toml.push_str("[permissions]\nsandbox = \"workspace-write\"\n");
    std::fs::write(&conf, toml).unwrap();
    let mut c = Client::start(&d);
    c.initialize_with(false, true);
    let sid = c.new_session(&d.work);
    c.call("session/set_mode", json!({"sessionId": sid, "modeId": "auto"})).unwrap();
    let mut term = FakeTerminal::new();
    let r = c
        .call_with(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": "run it"}]}),
            &mut |c, m| term.answer(c, m),
        )
        .unwrap();
    assert_eq!(r["stopReason"], "end_turn");
    assert!(term.seen.is_empty(), "no terminal/* request with the sandbox on: {:?}", term.methods());
    let terminal_shown = c.updates().iter().any(|(_, u)| u["content"][0]["type"] == "terminal");
    assert!(!terminal_shown, "no terminal in the tool call");
    c.finish();
}
