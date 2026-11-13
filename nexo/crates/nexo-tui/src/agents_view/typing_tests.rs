//! Typing a panel message into a session of another program: asked first, then sent to its inbox.

use super::*;
use nexo_agent::supervise::EvDetail;
use std::io::Read;
use std::os::unix::net::UnixListener;

fn claude_row(cwd: &str) -> AgentRow {
    AgentRow {
        key: "claude:abc".into(),
        name: "fix-build".into(),
        source: SourceKind::External,
        program: "claude".into(),
        state: State::Idle { min: 1 },
        since: 1_000,
        last_step: None,
        idle_secs: Some(60),
        cost_usd: None,
        model: None,
        cwd: Some(cwd.into()),
        permission_mode: None,
        parent: None,
        last_action: None,
        paused: false,
        ribbon: vec![nexo_agent::supervise::Ev {
            at: 999,
            kind: nexo_agent::supervise::EvKind::Read,
            n: 1,
            detail: EvDetail::None,
        }],
        spark: vec![],
        todo: None,
        tests: None,
        subs: vec![],
        unread: 0,
        held: 0,
        retry_secs: None,
    }
}

fn press(v: &mut AgentsView, code: KeyCode) {
    v.key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn write(v: &mut AgentsView, s: &str) {
    s.chars().for_each(|c| press(v, KeyCode::Char(c)));
}

#[test]
fn a_message_for_another_program_is_asked_about_then_typed_into_its_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let socks = dir.path().join("cc-socks");
    std::fs::create_dir_all(&socks).unwrap();
    let listener = UnixListener::bind(socks.join(format!("{}.sock", std::process::id()))).unwrap();
    listener.set_nonblocking(true).unwrap();
    let cwd = std::fs::read_link("/proc/self/cwd").unwrap().to_string_lossy().into_owned();
    let mut v = AgentsView::with_rows(vec![claude_row(&cwd)], 1_060, true);
    v.set_reach(nexo_core::reach::Reach {
        cc_socks: Some(socks),
        claude_home: Some(dir.path().join("claude")),
        tmux_name: Some(format!("nexo-panel-none-{}", std::process::id())),
    });

    // no supervisor behind the panel: the message is still written, and asked about before it goes
    press(&mut v, KeyCode::Char('n'));
    write(&mut v, "run the tests");
    press(&mut v, KeyCode::Enter);
    assert!(v.confirm.is_some());
    assert!(v.note.as_deref().unwrap_or("").contains("Enter yes"), "{:?}", v.note);
    assert!(listener.accept().is_err(), "nothing is typed before the yes");

    // no keeps it as a draft, and `n` picks it up again
    press(&mut v, KeyCode::Esc);
    assert!(v.confirm.is_none() && v.note.as_deref().unwrap_or("").starts_with("not sent"));
    assert!(listener.accept().is_err());
    press(&mut v, KeyCode::Char('n'));
    assert_eq!(v.edit.as_deref(), Some("run the tests"));
    press(&mut v, KeyCode::Enter);

    listener.set_nonblocking(false).unwrap();
    let server = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).unwrap();
        got
    });
    press(&mut v, KeyCode::Enter);
    assert_eq!(v.note.as_deref(), Some("typed into fix-build and pressed Enter"));
    let got: serde_json::Value = serde_json::from_str(server.join().unwrap().trim()).unwrap();
    assert_eq!(got, serde_json::json!({"type": "user", "message": {"role": "user", "content": "run the tests"}}));
}

#[test]
fn a_session_that_cannot_be_typed_into_still_needs_the_supervisor() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = AgentsView::with_rows(vec![claude_row("/nowhere/at/all")], 1_060, true);
    v.set_reach(nexo_core::reach::Reach {
        cc_socks: Some(dir.path().join("cc-socks")),
        claude_home: Some(dir.path().join("claude")),
        tmux_name: Some(format!("nexo-panel-none-{}", std::process::id())),
    });
    press(&mut v, KeyCode::Char('n'));
    assert!(v.edit.is_none());
    assert!(v.note.as_deref().unwrap_or("").contains("supervisor"), "{:?}", v.note);
}
