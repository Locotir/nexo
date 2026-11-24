//! The status bar's fleet chip never counts other live sessions: they belong to `/panel`, and a bar that says
//! `7 sessions` because other tools run tells the person nothing about this one. This binary owns its process,
//! so it may point the data folder at a fixture and set the process-wide mailbox; it never reads the real `~/.nexo`.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::mail::Inbound;
use nexo_core::mailbox::Mailbox;
use nexo_tui::{App, TuiOptions};
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        // an empty session drifts its stars for a while (src/app/tests/bursts.rs); these frames are about other things
        home_stars: false,
        bursts: true,
        todo_strip: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    App::new(opts, tx)
}

#[test]
fn another_live_session_never_lights_the_chip() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_DATA_DIR", home.path().join("data"));
    std::env::set_var("NEXO_CONFIG_DIR", home.path().join("config"));
    std::env::set_var("NEXO_CACHE_DIR", home.path().join("cache"));
    let root = nexo_core::mailbox::mail_root();
    assert!(root.starts_with(home.path()), "the fixture folder, never the real one: {root:?}");
    std::fs::create_dir_all(&root).unwrap();
    let own = Mailbox::open(&root, "20261115-100000-aaaa", "here", "/w", false, Inbound::Auto).unwrap();
    // another session: a heartbeat whose process is alive and is not this one (the test's parent)
    let _peer = Mailbox::open(&root, "20261115-100000-bbbb", "other", "/w", false, Inbound::Auto).unwrap();
    let file = root.join("20261115-100000-bbbb").join("session.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    v["pid"] = std::os::unix::process::parent_id().into();
    std::fs::write(&file, v.to_string()).unwrap();
    nexo_tui::set_mailbox(own);

    let mut a = app();
    // the watch starts at startup and polls once a second: a few polls see the other session
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        a.tick();
        assert!(a.fleet.is_none(), "{:?}", a.fleet);
        std::thread::sleep(Duration::from_millis(200));
    }
}
