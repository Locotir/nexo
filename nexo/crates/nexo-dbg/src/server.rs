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
use crate::platform::{self, LaunchOpts};
use crate::{fail, Error, Result};

/// A session with no call for this long ends (the program is killed, or let go if it was attached).
pub const IDLE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub enum Kind {
    Run {
        exe: PathBuf,
        args: Vec<String>,
    },
    /// A program started with its own environment, working folder, address randomisation or `argv[0]`.
    RunWith {
        exe: PathBuf,
        args: Vec<String>,
        opts: LaunchOpts,
    },
    Attach(u32),
    /// A core file, and the program's file when the dump's own record of it is not the one to use.
    Core {
        file: PathBuf,
        exe: Option<PathBuf>,
    },
    /// A GDB remote stub at `host:port`, and the program's file here when the stub's own path is not one to open.
    Remote {
        addr: String,
        exe: Option<PathBuf>,
    },
}

impl Kind {
    /// The program a `run` starts, its arguments and how it is started (`None` for the other kinds).
    pub fn program(&self) -> Option<(&Path, &[String], LaunchOpts)> {
        match self {
            Kind::Run { exe, args } => Some((exe, args, LaunchOpts::default())),
            Kind::RunWith { exe, args, opts } => Some((exe, args, opts.clone())),
            _ => None,
        }
    }
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

pub(crate) fn alive(pid: u32) -> bool {
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

/// The session, how the program died (a core file), and a fact about its debug info for the first answer.
type Made = (Session, Option<crate::corefile::Death>, Option<String>);

fn make_session(kind: &Kind) -> Result<Made> {
    if let Kind::Core { file, exe } = kind {
        if crate::minidump::is_minidump(file) {
            let mut dump = crate::minidump::Minidump::open(file)?;
            let death = dump.death();
            let Some(path) = exe.clone().or_else(|| dump.exe()) else {
                return fail("the minidump does not name its program: give it with `--exe <path>`");
            };
            dump.exe_first();
            let img = Image::open(&path)
                .map_err(|e| Error::Fail(format!("cannot read {} as a program: {e}", path.display())))?;
            return Ok((Session::new(Box::new(dump), img), death, None));
        }
        let core = crate::corefile::Core::open(file)?;
        let death = core.death();
        let Some(path) = exe.clone().or_else(|| core.exe()) else {
            return fail("the core file does not name its program: give it with `--exe <path>`");
        };
        let img =
            Image::open(&path).map_err(|e| Error::Fail(format!("cannot read {} as a program: {e}", path.display())))?;
        return Ok((Session::new(Box::new(core), img), death, None));
    }
    if let Kind::Remote { addr, exe } = kind {
        let mut r = crate::remote::Remote::connect(addr)?;
        let path = match exe {
            Some(p) => p.clone(),
            None => {
                let p = PathBuf::from(r.exe_path());
                if !p.is_file() {
                    return fail(format!(
                        "the stub runs {:?} and that file is not here: give the local copy with `--exe <path>`",
                        r.exe_path()
                    ));
                }
                p
            }
        };
        let img =
            Image::open(&path).map_err(|e| Error::Fail(format!("cannot read {} as a program: {e}", path.display())))?;
        r.locate(img.entry, img.image_base);
        return Ok((Session::new(Box::new(r), img), None, None));
    }
    let t = match kind {
        Kind::Run { .. } | Kind::RunWith { .. } => {
            let Some((exe, args, opts)) = kind.program() else { return fail("unreachable") };
            if !exe.is_file() {
                return fail(format!("{} is not a file", exe.display()));
            }
            if let Some(why) = platform::foreign_program(exe) {
                return fail(why);
            }
            if let Some(dir) = &opts.cwd {
                if !dir.is_dir() {
                    return fail(format!("the working folder {} is not a folder", dir.display()));
                }
            }
            platform::launch(&opts.launch(exe, args))?
        }
        Kind::Attach(pid) => platform::attach(*pid)?,
        Kind::Core { .. } | Kind::Remote { .. } => return fail("unreachable"),
    };
    let mut t = t;
    let mods = t.modules().unwrap_or_default();
    let path = match kind {
        Kind::Run { exe, .. } | Kind::RunWith { exe, .. } => exe.clone(),
        _ => PathBuf::from(mods.first().map(|m| m.path.clone()).unwrap_or_default()),
    };
    let mut img = match Image::open_arch(&path, platform::NATIVE_SLICE) {
        Ok(i) => i,
        Err(e) => {
            let _ = t.kill();
            return fail(format!("cannot read {} as a program: {e}", path.display()));
        }
    };
    // a macOS debug map gathered by dsymutil, or the separate debug file an ELF program was stripped of
    let debug = platform::gather_dwarf(&mut img).or_else(|| img.debug_info_line());
    Ok((Session::new(t, img), None, debug))
}

/// Start the program and take it to its first stop. The caller owns the result and must stay on this thread.
pub fn open(kind: &Kind) -> Result<(Session, Output)> {
    let (mut sess, death, debug) = make_session(kind)?;
    let mut first = match kind {
        Kind::Run { .. } | Kind::RunWith { .. } => run_to_entry(&mut sess, kind.program().and_then(|p| p.2.fact())),
        Kind::Attach(pid) => attach_report(&mut sess, *pid),
        Kind::Core { file, .. } => core_report(&mut sess, file, death),
        Kind::Remote { addr, .. } => remote_report(&mut sess, addr),
    };
    if let Some(text) = debug {
        first.rows.insert(first.rows.len().min(1), Row { key: 0, text, json: None });
    }
    // what earlier sessions kept for this binary comes with the first answer
    let known = nexo_bin::names_brief(sess.mods.main_image(), 8);
    if !known.is_empty() {
        let key = first.rows.len() as u64;
        first.rows.push(nexo_bin::Row {
            key,
            text: "known about this binary (kept by earlier sessions; `name` and `note` add):".into(),
            json: None,
        });
        for (i, k) in known.into_iter().enumerate() {
            first.rows.push(nexo_bin::Row { key: key + 1 + i as u64, text: format!("  {k}"), json: None });
        }
    }
    Ok((sess, first))
}

/// One session the server answers for: the program it started, or a child that program forked and the debugger
/// kept (`follow both|child`). Each has its own id, token and state file; all share the server's one port and
/// thread, because only the thread that traces a process may make system calls on it.
struct Slot {
    id: String,
    token: String,
    sess: Session,
    pending: Option<Output>,
    /// The session whose program forked this one.
    parent: Option<String>,
}

fn write_slot(dir: &Path, port: u16, s: &Slot) {
    let _ = write_state(
        dir,
        &s.id,
        &json!({"port": port, "token": s.token, "server": std::process::id(), "pid": s.sess.t.pid(),
                "exe": s.sess.mods.exe_path().display().to_string(), "parent": s.parent}),
    );
}

/// Give the children the sessions kept since the last look a slot and a state file of their own.
fn take_kids(dir: &Path, port: u16, slots: &mut Vec<Slot>) {
    let mut i = 0;
    while i < slots.len() {
        for kid in std::mem::take(&mut slots[i].sess.kids) {
            let s = Slot {
                id: kid.id,
                token: random_hex(),
                sess: kid.sess,
                pending: Some(kid.first),
                parent: Some(slots[i].id.clone()),
            };
            write_slot(dir, port, &s);
            slots.push(s);
        }
        i += 1;
    }
}

fn close(dir: &Path, slots: &mut Vec<Slot>, i: usize) {
    let mut s = slots.remove(i);
    teardown(&mut s.sess);
    let _ = std::fs::remove_file(session_file(dir, &s.id));
}

/// The session that forked session `id`, from its state file.
pub fn parent_of(dir: &Path, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(session_file(dir, id)).ok()?;
    serde_json::from_str::<Value>(&text).ok()?["parent"].as_str().map(str::to_string)
}

/// Run one session, and the forked children it keeps, until they end. Blocks. Returns the exit code.
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
    let dir = &start.dir;
    let mut slots = vec![Slot { id: start.id.clone(), token: random_hex(), sess, pending: Some(first), parent: None }];
    write_slot(dir, port, &slots[0]);
    take_kids(dir, port, &mut slots);
    let mut last_call = Instant::now();
    let mut next_check = Instant::now();
    let code = loop {
        match listener.accept() {
            Ok((stream, _)) => {
                last_call = Instant::now();
                if let Some(i) = handle(dir, port, &mut slots, stream) {
                    close(dir, &mut slots, i);
                }
                if slots.is_empty() {
                    break 0;
                }
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break 1,
        }
        let mut running = false;
        for s in slots.iter_mut().filter(|s| s.sess.state == State::Running) {
            running = true;
            // a call that outlived its `--wait` and returns or faults here is ended by `news` as by `wait`
            if let Some(o) = s.sess.news(Duration::from_millis(20)) {
                s.pending = Some(o);
            }
        }
        if running {
            take_kids(dir, port, &mut slots);
        } else {
            std::thread::sleep(Duration::from_millis(30));
        }
        if Instant::now() >= next_check {
            next_check = Instant::now() + Duration::from_secs(1);
            if start.owner.is_some_and(|o| !alive(o)) || last_call.elapsed() > start.idle {
                break 0;
            }
            // a session whose state file was removed ends; the server ends with the last one
            while let Some(i) = slots.iter().position(|s| !session_file(dir, &s.id).exists()) {
                close(dir, &mut slots, i);
            }
            if slots.is_empty() {
                break 0;
            }
        }
    };
    while !slots.is_empty() {
        close(dir, &mut slots, 0);
    }
    code
}

fn run_to_entry(s: &mut Session, launch: Option<String>) -> Output {
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
            if let Some(note) = s.t.note() {
                o.rows.insert(1, Row { key: 0, text: note, json: None });
            }
            if let Some(text) = launch {
                o.rows.insert(1, Row { key: 0, text, json: None });
            }
            for (i, r) in o.rows.iter_mut().enumerate() {
                r.key = i as u64;
            }
            o
        }
        Err(e) => rows("stop", vec![format!("started, but cannot run to the entry: {e}")], json!({})),
    }
}

fn core_report(s: &mut Session, file: &Path, death: Option<crate::corefile::Death>) -> Output {
    let tid = s.t.threads().first().copied().unwrap_or(0);
    let why = match death {
        Some(d) => crate::engine::Why::Signal(crate::Fault {
            number: d.signal,
            code: d.code,
            addr: d.addr,
            touch: d.touch,
            first_chance: false,
        }),
        None => crate::engine::Why::Interrupt,
    };
    let mut o = s.stopped(tid, why);
    let n = s.t.threads().len();
    let text = format!(
        "core file {}: the program died here ({n} thread{}); nothing can run",
        file.display(),
        if n == 1 { "" } else { "s" }
    );
    o.rows.insert(0, Row { key: 0, text, json: None });
    for (i, r) in o.rows.iter_mut().enumerate() {
        r.key = i as u64;
    }
    o
}

fn remote_report(s: &mut Session, addr: &str) -> Output {
    let tid = s.t.threads().first().copied().unwrap_or(0);
    let mut o = s.stopped(tid, crate::engine::Why::Interrupt);
    o.rows.insert(
        0,
        Row { key: 0, text: format!("connected to the stub at {addr}; the program is stopped"), json: None },
    );
    for (i, r) in o.rows.iter_mut().enumerate() {
        r.key = i as u64;
    }
    o
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

/// Answer one call for the session whose token it carries. `Some(index)`: that session must end now.
fn handle(dir: &Path, port: u16, slots: &mut Vec<Slot>, stream: TcpStream) -> Option<usize> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let mut rd = BufReader::new(&stream);
    if rd.read_line(&mut line).is_err() {
        return None;
    }
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let mut quit = None;
    let at = slots.iter().position(|s| req["token"].as_str() == Some(s.token.as_str()));
    let reply = if let Some(i) = at {
        let Slot { sess, pending, .. } = &mut slots[i];
        let words: Vec<String> = req["words"]
            .as_array()
            .map(|a| a.iter().filter_map(|w| w.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let cmd = words.first().map(String::as_str).unwrap_or("");
        let before = sess.nstop;
        let res: Result<Output> = match cmd {
            "__first" => Ok(pending.take().unwrap_or_else(|| rows("status", vec!["no news".into()], json!({})))),
            "__end" => {
                quit = Some(i);
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
            Ok(o) => {
                let stop = sess
                    .last_stop
                    .as_ref()
                    .map(|s| json!({"n": s.n, "event": s.event, "place": s.place, "addr": s.addr}));
                let bridge = if sess.nstop != before && sess.state == State::Stopped { sess.bridge() } else { None };
                json!({"ok": true, "out": out_to_json(&o), "state": state_word(sess), "stop": stop, "fresh": sess.nstop != before, "bridge": bridge})
            }
            Err(e) => json!({"ok": false, "error": e.to_string(), "code": e.exit_code(), "state": state_word(sess)}),
        }
    } else {
        json!({"ok": false, "error": "bad token", "code": 1})
    };
    // before the answer goes out, so the call that follows it finds the sessions it names: the children kept, and
    // no longer a parent let go at a fork (`follow child`), whose process runs on without the debugger
    take_kids(dir, port, slots);
    if let Some(i) = at.filter(|&i| {
        let s = &slots[i];
        quit.is_none()
            && s.pending.is_none()
            && s.sess.state == State::Exited
            && s.sess.last_stop.as_ref().is_some_and(|x| x.event == "fork")
    }) {
        let _ = std::fs::remove_file(session_file(dir, &slots[i].id));
        quit = Some(i);
    }
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
    /// The last stop (its number, event, place and address), and whether this very call produced it.
    pub stop: Option<crate::engine::StopInfo>,
    pub fresh: bool,
    /// The program's path and the file address of the pc, for a fresh stop in the program's own code.
    pub bridge: Option<(String, u64)>,
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
        let stop = v["stop"].as_object().map(|_| crate::engine::StopInfo {
            n: v["stop"]["n"].as_u64().unwrap_or(0),
            event: v["stop"]["event"].as_str().unwrap_or("").to_string(),
            place: v["stop"]["place"].as_str().unwrap_or("").to_string(),
            addr: v["stop"]["addr"].as_u64().unwrap_or(0),
        });
        let bridge = match (v["bridge"][0].as_str(), v["bridge"][1].as_u64()) {
            (Some(exe), Some(addr)) => Some((exe.to_string(), addr)),
            _ => None,
        };
        Ok(Reply { out: out_from_json(&v["out"]), state, stop, fresh: v["fresh"].as_bool() == Some(true), bridge })
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
        // a server that is gone (its nexo ended) left its file behind: it is no session
        if j["server"].as_u64().is_some_and(|s| !alive(s as u32)) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        v.push((id.to_string(), j["exe"].as_str().unwrap_or("").to_string(), j["pid"].as_u64().unwrap_or(0)));
    }
    v.sort();
    v
}

/// The launch environment as one line of hex per `K=V`, for the pipe to the server (hex keeps newlines and
/// any byte of a value intact).
pub fn encode_env(env: &[(String, String)]) -> String {
    let mut s = String::new();
    for (k, v) in env {
        for b in format!("{k}={v}").bytes() {
            s.push_str(&format!("{b:02x}"));
        }
        s.push('\n');
    }
    s
}

/// The inverse of [`encode_env`]; a line that is not hex or has no `=` is an error naming its number.
pub fn decode_env(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
        let bad = || Error::Usage(format!("environment line {} from the starter is not NAME=VALUE in hex", n + 1));
        if line.len() % 2 != 0 {
            return Err(bad());
        }
        let bytes: Option<Vec<u8>> = (0..line.len())
            .step_by(2)
            .map(|i| line.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
            .collect();
        let s = String::from_utf8(bytes.ok_or_else(bad)?).map_err(|_| bad())?;
        let (k, v) = s.split_once('=').ok_or_else(bad)?;
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

/// Start a server for a new session by running `exe dbg __serve ...`; returns the id once the program is
/// loaded (or the server's own reason for failing).
pub fn spawn(nexo: &Path, dir: &Path, kind: &Kind) -> Result<String> {
    let id = new_id();
    std::fs::create_dir_all(dir)?;
    let mut env_text: Option<String> = None;
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
        Kind::RunWith { exe, args, opts } => {
            cmd.arg("--run").arg(exe);
            for a in args {
                cmd.arg("--arg").arg(a);
            }
            if !opts.env.is_empty() {
                // The values go down a pipe, never the command line: a command line is readable by every local user.
                cmd.arg("--env-stdin").arg("on");
                env_text = Some(encode_env(&opts.env));
            }
            for k in &opts.unset {
                cmd.arg("--unset").arg(k);
            }
            if let Some(d) = &opts.cwd {
                cmd.arg("--cwd").arg(d);
            }
            if opts.aslr {
                cmd.arg("--aslr").arg("on");
            }
            if let Some(a) = &opts.argv0 {
                cmd.arg("--argv0").arg(a);
            }
        }
        Kind::Attach(pid) => {
            cmd.arg("--attach").arg(pid.to_string());
        }
        Kind::Core { file, exe } => {
            cmd.arg("--core").arg(file);
            if let Some(e) = exe {
                cmd.arg("--exe").arg(e);
            }
        }
        Kind::Remote { addr, exe } => {
            cmd.arg("--remote").arg(addr);
            if let Some(e) = exe {
                cmd.arg("--exe").arg(e);
            }
        }
    }
    cmd.stdin(if env_text.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
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
    if let (Some(text), Some(mut pipe)) = (env_text, child.stdin.take()) {
        use std::io::Write;
        // Closing the pipe (the drop) is what tells the server the list is complete.
        pipe.write_all(text.as_bytes())
            .map_err(|e| Error::Fail(format!("cannot hand the environment to the debug server: {e}")))?;
    }
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

/// Start a server for a new session on a thread of this process; returns the id once the program is
/// loaded (or the server's own reason for failing). The program ends with the process (it is killed when
/// nexo exits, by the kernel if need be), so nothing outlives the run.
pub fn spawn_thread(dir: &Path, kind: &Kind) -> Result<String> {
    let id = new_id();
    std::fs::create_dir_all(dir)?;
    let start = Start { id: id.clone(), dir: dir.to_path_buf(), kind: kind.clone(), owner: None, idle: IDLE };
    let handle = std::thread::Builder::new()
        .name(format!("dbg-{id}"))
        .spawn(move || serve(&start))
        .map_err(|e| Error::Fail(format!("cannot start the debug server: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if session_file(dir, &id).exists() {
            return read_entry(dir, &id).map(|_| id);
        }
        if handle.is_finished() {
            if session_file(dir, &id).exists() {
                continue;
            }
            return fail("the debug server ended at once without a word");
        }
        if Instant::now() > deadline {
            return fail("the debug server did not come up in 40 seconds");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
