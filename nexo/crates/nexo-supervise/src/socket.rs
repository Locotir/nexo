//! The local socket between the supervisor and the hooks of the external sessions. One request per
//! connection: a JSON line in, a JSON line out. The socket file is private to the person (mode 0600). A
//! client gives up after its time limit and the caller carries on as if nobody answered, so a supervisor that
//! is not running never holds a session back.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long a hook waits for the supervisor.
pub const HOOK_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    /// A turn ends: the place to say "go on".
    Stop,
    /// A tool is about to ask the person for permission.
    Permission,
    /// A notification (a permission prompt, an idle prompt).
    Notify,
}

impl HookKind {
    pub fn parse(s: &str) -> Option<HookKind> {
        match s {
            "stop" => Some(HookKind::Stop),
            "permission" => Some(HookKind::Permission),
            "notify" => Some(HookKind::Notify),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            HookKind::Stop => "stop",
            HookKind::Permission => "permission",
            HookKind::Notify => "notify",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// What a hook got on its standard input.
    Hook {
        kind: HookKind,
        payload: Value,
    },
    /// A nudge the person wrote, for the session's next stop.
    Nudge {
        key: String,
        text: String,
    },
    /// Pause one session, or all of them when `key` is empty.
    Pause {
        key: Option<String>,
        on: bool,
    },
    Ping,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    /// For a hook: exactly what it prints.
    pub out: Value,
}

impl Reply {
    pub fn ok(out: Value) -> Reply {
        Reply { ok: true, out }
    }
}

/// `$NEXO_SUPERVISOR_SOCK`, else `supervisor.sock` in the nexo data folder.
pub fn default_path() -> PathBuf {
    match std::env::var_os("NEXO_SUPERVISOR_SOCK") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => nexo_core::paths::data_dir().join("supervisor.sock"),
    }
}

/// The folder of the audit log and the live state file.
pub fn default_dir() -> PathBuf {
    nexo_core::paths::data_dir().join("supervisor")
}

#[cfg(unix)]
const MAX_LINE: u64 = 1 << 20;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    /// Send one request and wait for the answer, at most `limit` in all. None: nobody answered.
    pub fn ask(path: &Path, req: &Request, limit: Duration) -> Option<Reply> {
        let start = Instant::now();
        let mut s = UnixStream::connect(path).ok()?;
        let left = |start: Instant| limit.checked_sub(start.elapsed()).filter(|d| !d.is_zero());
        s.set_write_timeout(left(start)).ok()?;
        let mut line = serde_json::to_vec(req).ok()?;
        line.push(b'\n');
        s.write_all(&line).ok()?;
        s.set_read_timeout(left(start)).ok()?;
        let mut text = String::new();
        BufReader::new(s.take(MAX_LINE)).read_line(&mut text).ok()?;
        serde_json::from_str(text.trim()).ok()
    }

    /// Is a supervisor answering at `path`?
    pub fn alive(path: &Path) -> bool {
        UnixStream::connect(path).is_ok()
    }

    /// The listening socket; dropping it stops the thread and removes the file.
    pub struct Server {
        path: PathBuf,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::fs::remove_file(&self.path);
        }
    }

    impl Server {
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    pub type Handler = Arc<dyn Fn(Request) -> Reply + Send + Sync>;

    /// Listen at `path`. Fails with `AddrInUse` when another supervisor already answers there; a socket
    /// file left by a dead one is replaced. The file is made private before it takes its name.
    pub fn serve(path: &Path, handler: Handler) -> std::io::Result<Server> {
        if alive(path) {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "a supervisor is already running"));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("sock.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let listener = UnixListener::bind(&tmp)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        let _ = conn.set_nonblocking(false);
                        answer(conn, &handler);
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(15)),
                }
            }
        });
        Ok(Server { path: path.to_path_buf(), stop })
    }

    fn answer(mut conn: UnixStream, handler: &Handler) {
        let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = conn.set_write_timeout(Some(Duration::from_secs(2)));
        let mut text = String::new();
        let Ok(clone) = conn.try_clone() else { return };
        if BufReader::new(clone.take(MAX_LINE)).read_line(&mut text).is_err() {
            return;
        }
        let reply = match serde_json::from_str::<Request>(text.trim()) {
            Ok(req) => handler(req),
            Err(_) => Reply { ok: false, out: Value::Null },
        };
        if let Ok(mut line) = serde_json::to_vec(&reply) {
            line.push(b'\n');
            let _ = conn.write_all(&line);
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;
    use std::sync::Arc;

    pub fn ask(_: &Path, _: &Request, _: Duration) -> Option<Reply> {
        None
    }

    pub fn alive(_: &Path) -> bool {
        false
    }

    pub struct Server;

    impl Server {
        pub fn path(&self) -> &Path {
            Path::new("")
        }
    }

    pub type Handler = Arc<dyn Fn(Request) -> Reply + Send + Sync>;

    pub fn serve(_: &Path, _: Handler) -> std::io::Result<Server> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "the supervisor socket needs a Unix system"))
    }
}

pub use imp::{alive, ask, serve, Handler, Server};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn echo() -> Handler {
        Arc::new(|r| match r {
            Request::Ping => Reply::ok(serde_json::json!("pong")),
            Request::Nudge { key, text } => Reply::ok(serde_json::json!({"key": key, "text": text})),
            _ => Reply { ok: false, out: Value::Null },
        })
    }

    #[test]
    fn a_request_gets_its_answer_and_the_file_is_private() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        let srv = serve(&p, echo()).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let r = ask(&p, &Request::Ping, HOOK_TIMEOUT).unwrap();
        assert_eq!(r.out, "pong");
        let r = ask(&p, &Request::Nudge { key: "k".into(), text: "go".into() }, HOOK_TIMEOUT).unwrap();
        assert_eq!(r.out["text"], "go");
        drop(srv);
        assert!(!p.exists());
        assert!(ask(&p, &Request::Ping, HOOK_TIMEOUT).is_none());
    }

    #[test]
    fn a_second_supervisor_is_refused_and_a_dead_ones_file_is_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        let _srv = serve(&p, echo()).unwrap();
        assert_eq!(serve(&p, echo()).err().map(|e| e.kind()), Some(std::io::ErrorKind::AddrInUse));
        let q = d.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&q).unwrap());
        assert!(q.exists(), "a file left behind");
        assert!(serve(&q, echo()).is_ok());
    }

    #[test]
    fn garbage_gets_a_refusal_and_a_silent_server_costs_only_the_time_limit() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        let _srv = serve(&p, echo()).unwrap();
        use std::io::{Read, Write};
        let mut c = std::os::unix::net::UnixStream::connect(&p).unwrap();
        c.write_all(b"not json\n").unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        assert!(out.contains("\"ok\":false"), "{out}");
        // a listener that never answers
        let q = d.path().join("mute.sock");
        let _l = std::os::unix::net::UnixListener::bind(&q).unwrap();
        let t = std::time::Instant::now();
        assert!(ask(&q, &Request::Ping, Duration::from_millis(300)).is_none());
        assert!(t.elapsed() < Duration::from_millis(900));
    }
}
