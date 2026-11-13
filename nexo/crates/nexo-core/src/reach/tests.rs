use super::*;

#[test]
fn an_address_names_the_way_in_and_a_bare_name_is_a_nexo_session() {
    assert_eq!(parse("claude:4242"), Some(Ok(Target::Claude(4242))));
    assert_eq!(parse(" tmux:%3 "), Some(Ok(Target::Tmux("%3".into()))));
    assert_eq!(parse("tmux:work:1.0"), Some(Ok(Target::Tmux("work:1.0".into()))));
    assert_eq!(parse("pid:77"), Some(Ok(Target::Pid(77))));
    assert_eq!(parse("codex:77"), Some(Ok(Target::Pid(77))));
    assert_eq!(parse("web"), None);
    assert_eq!(parse("api [ab12]"), None);
    let bad = parse("claude:abc").unwrap().unwrap_err();
    assert!(bad.contains("claude:<pid>, tmux:<pane> or pid:<n>"), "{bad}");
    assert!(parse("tmux:-t").unwrap().is_err());
    assert!(parse("codex:me").unwrap().is_err());
}

#[test]
fn typed_text_is_one_line() {
    assert_eq!(one_line("  run the tests\n\nthen push\tnow "), "run the tests then push now");
    assert_eq!(one_line("x\r\n"), "x");
}

/// A reach that sees only this test's folders and tmux server.
fn own(dir: &Path, tmux: &str) -> Reach {
    Reach {
        cc_socks: Some(dir.join("cc-socks")),
        claude_home: Some(dir.join("claude")),
        tmux_name: Some(tmux.to_string()),
    }
}

#[test]
fn a_process_with_no_way_in_is_said_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let r = own(dir.path(), &format!("nexo-reach-none-{}", std::process::id()));
    let e = r.send(&Target::Pid(std::process::id()), "hi").unwrap_err();
    assert!(e.contains("does not run inside tmux"), "{e}");
    let e = r.send(&Target::Claude(std::process::id()), "hi").unwrap_err();
    // The Claude inbox is reachable only on unix (local sockets); on Windows the send path
    // says so plainly instead, so the "no way in" wording differs by platform.
    #[cfg(unix)]
    assert!(e.contains("has no inbox"), "{e}");
    #[cfg(not(unix))]
    assert!(e.contains("works on Linux and macOS only"), "{e}");
    assert!(r.send(&Target::Claude(1), "  ").unwrap_err().contains("empty"));
    assert!(r.send(&Target::Claude(1), &"x".repeat(TYPE_MAX_BYTES + 1)).unwrap_err().contains("too big"));
}

#[cfg(unix)]
#[test]
fn a_claude_code_inbox_gets_the_key_then_the_prompt() {
    use serde_json::{json, Value};
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let r = own(dir.path(), &format!("nexo-reach-cc-{}", std::process::id()));
    let pid = std::process::id();
    std::fs::create_dir_all(dir.path().join("cc-socks")).unwrap();
    let sock = dir.path().join("cc-socks").join(format!("{pid}.sock"));
    let listener = UnixListener::bind(&sock).unwrap();
    let sessions = dir.path().join("claude").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let token = "0123456789abcdef0123456789abcdef";
    let key = sessions.join(format!("{pid}.{}.key", sha256_hex(sock.to_string_lossy().as_bytes())));
    std::fs::write(&key, format!("{{\"peerToken\":\"{token}\",\"procStart\":\"1\"}}")).unwrap();
    // a dead socket file next to it is not listed
    let _dead = UnixListener::bind(dir.path().join("cc-socks").join("4000000000.sock"));
    let server = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).unwrap();
        got
    });
    let who = r.send(&Target::Pid(pid), "fix the build\nplease").unwrap();
    assert_eq!(who, format!("Claude Code session {pid}"));
    let got = server.join().unwrap();
    let lines: Vec<Value> = got.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{got}");
    assert_eq!(lines[0], json!({"type": "auth", "token": token}));
    assert_eq!(lines[1], json!({"type": "user", "message": {"role": "user", "content": "fix the build\nplease"}}));
    let seen = r.discover();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].address, format!("claude:{pid}"));
    // no key published: the prompt goes alone
    std::fs::remove_file(&key).unwrap();
    let listener = UnixListener::bind(dir.path().join("x.sock")).unwrap();
    std::fs::remove_file(&sock).unwrap();
    std::fs::rename(dir.path().join("x.sock"), &sock).unwrap();
    let server = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).unwrap();
        got
    });
    r.send(&Target::Claude(pid), "second").unwrap();
    let got = server.join().unwrap();
    assert_eq!(got.lines().count(), 1, "{got}");
    assert!(got.contains("\"content\":\"second\""), "{got}");
}

#[test]
fn a_tmux_pane_gets_the_text_and_enter() {
    if Command::new("tmux").arg("-V").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("tmux is not installed: skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let name = format!("nexo-reach-{}", std::process::id());
    let r = own(dir.path(), &name);
    let got = dir.path().join("got");
    let script = format!("IFS= read -r l; printf '%s' \"$l\" > '{}'; sleep 30", got.display());
    let ok = Command::new("tmux")
        .args(["-L", &name, "-f", "/dev/null", "new-session", "-d", "-s", "t", "-c"])
        .arg(dir.path())
        .args(["sh", "-c", &script])
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let wait = |p: &Path| {
        for _ in 0..100 {
            if std::fs::read_to_string(p).is_ok_and(|s| !s.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        std::fs::read_to_string(p).unwrap_or_default()
    };
    /// The test's own server goes when the test ends, passed or not.
    struct Server(String);
    impl Drop for Server {
        fn drop(&mut self) {
            // tmux leaves its socket file behind: it goes too
            let socket = Command::new("tmux").args(["-L", &self.0, "display-message", "-p", "#{socket_path}"]).output();
            let _ = Command::new("tmux").args(["-L", &self.0, "kill-server"]).status();
            if let Ok(o) = socket {
                let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if p.ends_with(&self.0) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
    let _server = Server(name.clone());
    let seen = r.discover();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].address.starts_with("tmux:%"), "{seen:?}");
    // the pane tells its command and folder once the shell has started
    let cwd = dir.path().to_string_lossy().to_string();
    let found = (0..40).map(|_| r.find("sh", &cwd)).find(|f| {
        std::thread::sleep(Duration::from_millis(50));
        f.len() == 1
    });
    assert!(found.is_some(), "{:?}", r.discover());
    // by the process that runs in the pane, the way `pid:` reaches a program with no inbox
    let who = r.send(&Target::Pid(seen[0].pid), "-n hello\nfrom nexo").unwrap();
    assert!(who.contains("tmux pane %"), "{who}");
    assert_eq!(wait(&got), "-n hello from nexo");
    let e = r.send(&Target::Tmux("nosuch".into()), "x").unwrap_err();
    assert!(e.starts_with("Not sent: tmux pane nosuch"), "{e}");
}
