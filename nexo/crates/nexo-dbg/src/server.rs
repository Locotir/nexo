//! The program under test must live between two calls of an agent, so one small server per session owns
//! it (every system call on the program comes from the server's one thread). A call is one JSON line to
//! `127.0.0.1:<port>` with a token, and one JSON line back. The server ends the program when the
//! session's owner is gone, when its state file is removed, or after 30 idle minutes.

use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nexo_bin::{Image, Output, Row};
use serde_json::{json, Value};

use crate::engine::{rows, Session, State};
use crate::platform::{self, Launch};
use crate::{fail, Error, Result};

/// A session with no call for this long ends (the program is killed, or let go if it was attached).
pub const IDLE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub enum Kind {
    Run { exe: PathBuf, args: Vec<String> },
    Attach(u32),
}

#[derive(Debug, Clone)]
pub struct Start {
    pub id: String,
    pub dir: PathBuf,
    pub kind: Kind,
    /// The process whose end ends the session.
    pub owner: Option<u32>,
    pub idle: Duration,
}

/// Where session files live. Inside nexo's scratch folder (`$NEXO_TMP`, removed with the session) when there
/// is one, so the debugger goes away with the conversation that started it.
pub fn state_dir() -> PathBuf {
    if let Some(t) = std::env::var_os("NEXO_TMP").filter(|t| !t.is_empty()) {
        return PathBuf::from(t).join("dbg");
    }
    if let Some(r) = std::env::var_os("XDG_RUNTIME_DIR").filter(|t| !t.is_empty()) {
        return PathBuf::from(r).join("nexo-dbg");
    }
    std::env::temp_dir().join("nexo-dbg")
}

/// The pid in a `nexo-tmp-<digits>-<rest>` folder name, which is the nexo that owns the folder.
pub fn owner_of(dir: &Path) -> Option<u32> {
    let name = dir.parent()?.file_name()?.to_str()?;
    name.strip_prefix("nexo-tmp-")?.split_once('-')?.0.parse().ok()
}

fn random_hex() -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.write_u32(std::process::id());
    format!("{:016x}", h.finish())
}

pub fn new_id() -> String {
    random_hex()[..6].to_string()
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only asks whether the process exists.
        let r = unsafe { libc::kill(pid as i32, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

fn session_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn write_state(dir: &Path, id: &str, v: &Value) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = dir.join(format!("{id}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(v).unwrap_or_default())?;
    std::fs::rename(&tmp, session_file(dir, id))?;
    Ok(())
}

pub fn out_to_json(o: &Output) -> Value {
    json!({
        "op": o.op,
        "meta": o.meta,
        "tail": o.tail,
        "rows": o.rows.iter().map(|r| json!({"k": r.key, "t": r.text, "j": r.json})).collect::<Vec<_>>(),
    })
}

pub fn out_from_json(v: &Value) -> Output {
    let op: &'static str = match v["op"].as_str().unwrap_or("") {
        "stop" => "stop",
        "mem" => "mem",
        "running" => "running",
        "exit" => "exit",
        _ => "dbg",
    };
    Output {
        op,
        meta: v["meta"].clone(),
        tail: v["tail"].as_u64().unwrap_or(0) as usize,
        command: String::new(),
        rows: v["rows"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| Row {
                        key: r["k"].as_u64().unwrap_or(0),
                        text: r["t"].as_str().unwrap_or("").to_string(),
                        json: (!r["j"].is_null()).then(|| r["j"].clone()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn make_session(kind: &Kind) -> Result<Session> {
    let t = match kind {
        Kind::Run { exe, args } => {
            if !exe.is_file() {
                return fail(format!("{} is not a file", exe.display()));
            }
            platform::launch(&Launch { exe: exe.clone(), args: args.clone(), ..Launch::default() })?
        }
        Kind::Attach(pid) => platform::attach(*pid)?,
    };
    let mut t = t;
    let mods = t.modules().unwrap_or_default();
    let path = match kind {
        Kind::Run { exe, .. } => exe.clone(),
        Kind::Attach(_) => PathBuf::from(mods.first().map(|m| m.path.clone()).unwrap_or_default()),
    };
    let img = match Image::open(&path) {
        Ok(i) => i,
        Err(e) => {
            let _ = t.kill();
            return fail(format!("cannot read {} as a program: {e}", path.display()));
        }
    };
    Ok(Session::new(t, img))
}

/// Start the program and take it to its first stop. The caller owns the result and must stay on this thread.
pub fn open(kind: &Kind) -> Result<(Session, Output)> {
    let mut sess = make_session(kind)?;
    let first = match kind {
        Kind::Run { .. } => run_to_entry(&mut sess),
        Kind::Attach(pid) => attach_report(&mut sess, *pid),
    };
    Ok((sess, first))
}

/// Run one session until it ends. Blocks. Returns the exit code.
pub fn serve(start: &Start) -> i32 {
    let fail_with = |msg: String| {
        let _ = write_state(&start.dir, &start.id, &json!({"error": msg}));
        1
    };
    let (mut sess, first) = match open(&start.kind) {
        Ok(s) => s,
        Err(e) => return fail_with(e.to_string()),
    };
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            let _ = sess.t.kill();
            return fail_with(format!("cannot listen on 127.0.0.1: {e}"));
        }
    };
    let _ = listener.set_nonblocking(true);
    let port = listener.local_addr().map_or(0, |a| a.port());
    let token = random_hex();
    let mut pending: Option<Output> = Some(first);
    let _ = write_state(
        &start.dir,
        &start.id,
        &json!({"port": port, "token": token, "server": std::process::id(), "pid": sess.t.pid(),
                "exe": sess.mods.exe_path().display().to_string()}),
    );
    let mut last_call = Instant::now();
    let mut next_check = Instant::now();
    let code = loop {
        match listener.accept() {
            Ok((stream, _)) => {
                last_call = Instant::now();
                if handle(&mut sess, stream, &token, &mut pending) {
                    break 0;
                }
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break 1,
        }
        if sess.state == State::Running {
            sess.drain_output();
            if let Ok(Some((tid, why))) = sess.poll(Duration::from_millis(20)) {
                pending = Some(sess.stopped(tid, why));
            }
        } else {
            std::thread::sleep(Duration::from_millis(30));
        }
        if Instant::now() >= next_check {
            next_check = Instant::now() + Duration::from_secs(1);
            if start.owner.is_some_and(|o| !alive(o))
                || last_call.elapsed() > start.idle
                || !session_file(&start.dir, &start.id).exists()
            {
                break 0;
            }
        }
    };
    teardown(&mut sess);
    let _ = std::fs::remove_file(session_file(&start.dir, &start.id));
    code
}

fn run_to_entry(s: &mut Session) -> Output {
    let Some(entry) = s.t.entry() else {
        return rows("stop", vec!["started; the entry point is unknown".into()], json!({}));
    };
    let run = crate::engine::Run {
        step: None,
        temp: Some(crate::engine::Temp { addr: entry, min_sp: 0, kind: crate::engine::TempKind::Entry }),
    };
    match s.go(run, Duration::from_secs(20)) {
        Ok(o) => {
            let mut o = o;
            let pid = s.t.pid();
            o.rows.insert(0, Row { key: 0, text: format!("started pid {pid}"), json: None });
            for (i, r) in o.rows.iter_mut().enumerate() {
                r.key = i as u64;
            }
            o
        }
        Err(e) => rows("stop", vec![format!("started, but cannot run to the entry: {e}")], json!({})),
    }
}

fn attach_report(s: &mut Session, pid: u32) -> Output {
    let tid = s.t.threads().first().copied().unwrap_or(pid);
    let mut o = s.stopped(tid, crate::engine::Why::Interrupt);
    o.rows.insert(0, Row { key: 0, text: format!("attached to pid {pid}; the program is stopped"), json: None });
    for (i, r) in o.rows.iter_mut().enumerate() {
        r.key = i as u64;
    }
    o
}

fn teardown(s: &mut Session) {
    match s.state {
        State::Exited => {}
        State::Running => {
            let _ = s.t.interrupt();
            let _ = s.t.wait(Duration::from_secs(2));
            s.uninsert_all();
            end_target(s);
        }
        State::Stopped => {
            s.uninsert_all();
            end_target(s);
        }
    }
}

fn end_target(s: &mut Session) {
    if s.t.launched() {
        let _ = s.t.kill();
    } else {
        let _ = s.t.detach();
    }
}

/// Answer one call. True: the session must end now.
fn handle(sess: &mut Session, stream: TcpStream, token: &str, pending: &mut Option<Output>) -> bool {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let mut rd = BufReader::new(&stream);
    if rd.read_line(&mut line).is_err() {
        return false;
    }
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let mut quit = false;
    let reply = if req["token"].as_str() != Some(token) {
        json!({"ok": false, "error": "bad token", "code": 1})
    } else {
        let words: Vec<String> = req["words"]
            .as_array()
            .map(|a| a.iter().filter_map(|w| w.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let cmd = words.first().map(String::as_str).unwrap_or("");
        let res: Result<Output> = match cmd {
            "__first" => Ok(pending.take().unwrap_or_else(|| rows("status", vec!["no news".into()], json!({})))),
            "__end" => {
                quit = true;
                Ok(rows("end", vec!["session ended".into()], json!({})))
            }
            "wait" | "status" | "info" if pending.is_some() && sess.state != State::Running => {
                Ok(pending.take().unwrap_or_else(|| rows("status", vec![], json!({}))))
            }
            "cont" | "c" | "continue" | "step" | "s" | "next" | "n" | "finish" | "until" => {
                *pending = None;
                sess.exec(&words)
            }
            _ => sess.exec(&words),
        };
        match res {
            Ok(o) => json!({"ok": true, "out": out_to_json(&o), "state": state_word(sess)}),
            Err(e) => json!({"ok": false, "error": e.to_string(), "code": e.exit_code(), "state": state_word(sess)}),
        }
    };
    let mut w = &stream;
    let _ = w.write_all(format!("{reply}\n").as_bytes());
    let _ = w.flush();
    quit
}

fn state_word(s: &Session) -> &'static str {
    match s.state {
        State::Stopped => "stopped",
        State::Running => "running",
        State::Exited => "exited",
    }
}

// ---- the caller's side ----

pub struct Reply {
    pub out: Output,
    pub state: String,
}

struct Entry {
    port: u16,
    token: String,
}

fn read_entry(dir: &Path, id: &str) -> Result<Entry> {
    let text = std::fs::read_to_string(session_file(dir, id))
        .map_err(|_| Error::Fail(format!("no debug session {id:?}: `nexo dbg sessions` lists the live ones")))?;
    let v: Value = serde_json::from_str(&text).map_err(|_| Error::Fail("the session file is damaged".into()))?;
    if let Some(e) = v["error"].as_str() {
        let _ = std::fs::remove_file(session_file(dir, id));
        return fail(e.to_string());
    }
    Ok(Entry { port: v["port"].as_u64().unwrap_or(0) as u16, token: v["token"].as_str().unwrap_or("").to_string() })
}

/// Send one command to a session and wait for its answer (the server holds long commands at most their `--wait`).
pub fn request(dir: &Path, id: &str, words: &[String]) -> Result<Reply> {
    let e = read_entry(dir, id)?;
    let mut s = match TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], e.port)),
        Duration::from_secs(3),
    ) {
        Ok(s) => s,
        Err(_) => {
            let _ = std::fs::remove_file(session_file(dir, id));
            return fail(format!(
                "session {id} is gone (its server ended). If the shell tool runs commands in a sandbox that ends background processes with the command, start and use the session inside one command with `do`"
            ));
        }
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(crate::cmds::MAX_WAIT + 60)));
    let req = json!({"token": e.token, "words": words});
    s.write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line)?;
    let v: Value = serde_json::from_str(&line).map_err(|_| Error::Fail("the session did not answer".into()))?;
    let state = v["state"].as_str().unwrap_or("").to_string();
    if v["ok"].as_bool() == Some(true) {
        Ok(Reply { out: out_from_json(&v["out"]), state })
    } else {
        let msg = v["error"].as_str().unwrap_or("failed").to_string();
        Err(if v["code"].as_i64() == Some(2) { Error::Usage(msg) } else { Error::Fail(msg) })
    }
}

/// Live sessions: id, program, pid.
pub fn sessions(dir: &Path) -> Vec<(String, String, u64)> {
    let mut v = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return v };
    for e in rd.flatten() {
        let p = e.path();
        let (Some("json"), Some(id)) = (p.extension().and_then(|x| x.to_str()), p.file_stem().and_then(|x| x.to_str()))
        else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(j) = serde_json::from_str::<Value>(&text) else { continue };
        if j["port"].is_null() {
            continue;
        }
        v.push((id.to_string(), j["exe"].as_str().unwrap_or("").to_string(), j["pid"].as_u64().unwrap_or(0)));
    }
    v.sort();
    v
}

/// Start a server for a new session by running `exe dbg __serve ...`; returns the id once the program is
/// loaded (or the server's own reason for failing).
pub fn spawn(nexo: &Path, dir: &Path, kind: &Kind) -> Result<String> {
    let id = new_id();
    std::fs::create_dir_all(dir)?;
    let mut cmd = std::process::Command::new(nexo);
    cmd.arg("dbg").arg("__serve").arg("--id").arg(&id).arg("--dir").arg(dir);
    if let Some(o) = owner_of(dir) {
        cmd.arg("--owner").arg(o.to_string());
    }
    match kind {
        Kind::Run { exe, args } => {
            cmd.arg("--run").arg(exe);
            for a in args {
                cmd.arg("--arg").arg(a);
            }
        }
        Kind::Attach(pid) => {
            cmd.arg("--attach").arg(pid.to_string());
        }
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; the server must outlive the command that started it and
        // must not die with that command's process group.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().map_err(|e| Error::Fail(format!("cannot start the debug server: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if session_file(dir, &id).exists() {
            return match read_entry(dir, &id) {
                Ok(_) => Ok(id),
                Err(e) => Err(e),
            };
        }
        if let Ok(Some(st)) = child.try_wait() {
            if session_file(dir, &id).exists() {
                continue;
            }
            return fail(format!("the debug server ended at once ({st}) without a word"));
        }
        if Instant::now() > deadline {
            return fail("the debug server did not come up in 40 seconds");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
