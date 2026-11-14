//! Asking several browsers at once (`RelayBrowser::connect` without an id), with scripted extension ends instead of
//! real browsers: each one answers its consent prompt after a delay, or never. No Chrome is involved.

use nexo_browser::{browsers, RelayBrowser};
use nexo_relay::chunk::{self, Reassembler};
use nexo_relay::{frame, token, Dirs};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{duplex, DuplexStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// What a scripted browser saw.
#[derive(Default)]
struct Seen {
    asked: AtomicUsize,
    withdrawn: AtomicBool,
    opened: AtomicUsize,
}

/// How a scripted browser answers the prompt: after this long, yes or no; `None` is never (the person went away).
type Answer = Option<(Duration, bool)>;

struct Script {
    seen: Arc<Seen>,
    /// Messages the scripted extension sends to the host, for what happens in the browser by itself.
    out: mpsc::UnboundedSender<Value>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Script {
    fn drop(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
}

async fn send(w: &mut DuplexStream, msg: &Value, cid: u64) {
    for piece in chunk::encode(msg, cid) {
        if frame::write_frame(w, &piece).await.is_err() {
            return;
        }
    }
}

/// A real host (`nexo_browser_host::run`) in `dirs` with a scripted extension at the other end.
fn start(dirs: &Dirs, id: &str, name: &str, answer: Answer) -> Script {
    let token = token::load_or_create(dirs).unwrap();
    let (host_in, mut ext_w) = duplex(1 << 20);
    let (mut ext_r, host_out) = duplex(1 << 20);
    let host_dirs = dirs.clone();
    let host = tokio::spawn(async move {
        let _ = nexo_browser_host::run(host_in, host_out, host_dirs, token).await;
    });
    let (out, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let hello =
        json!({"t": "hello", "browser": {"id": id, "name": name, "os": "Linux", "version": "1", "ext": "test"}});
    let writer = tokio::spawn(async move {
        send(&mut ext_w, &hello, 0).await;
        let mut cid = 0;
        while let Some(m) = out_rx.recv().await {
            cid += 1;
            send(&mut ext_w, &m, cid).await;
        }
    });
    let seen = Arc::new(Seen::default());
    let s = Arc::clone(&seen);
    let to_host = out.clone();
    let reader = tokio::spawn(async move {
        let mut parts = Reassembler::new();
        while let Ok(Some(raw)) = frame::read_json(&mut ext_r, frame::MAX_SOCKET).await {
            let Ok(Some(msg)) = parts.push(raw) else { continue };
            match msg["t"].as_str() {
                Some("consent") => {
                    s.asked.fetch_add(1, Ordering::SeqCst);
                    s.withdrawn.store(false, Ordering::SeqCst);
                    let Some((after, ok)) = answer else { continue };
                    let (s, out, sid) = (Arc::clone(&s), out.clone(), msg["sid"].clone());
                    tokio::spawn(async move {
                        tokio::time::sleep(after).await;
                        if !s.withdrawn.load(Ordering::SeqCst) {
                            let why = if ok { Value::Null } else { json!("the person clicked Deny") };
                            let _ = out.send(json!({"t": "consent_result", "sid": sid, "ok": ok, "why": why}));
                        }
                    });
                }
                // The real extension closes the prompt on either: the host's cancel, or the session ending (the agent
                // that asked went away, or already has a browser).
                Some("consent_cancel") => s.withdrawn.store(true, Ordering::SeqCst),
                Some("session") if msg["open"] == json!(false) => s.withdrawn.store(true, Ordering::SeqCst),
                Some("req") => {
                    let method = msg["m"].as_str().unwrap_or("");
                    let reply = match method {
                        "tabs.create" => {
                            s.opened.fetch_add(1, Ordering::SeqCst);
                            json!({"t": "res", "sid": msg["sid"], "id": msg["id"], "r": {"tab": 1}})
                        }
                        "cdp" => json!({"t": "res", "sid": msg["sid"], "id": msg["id"], "r": {}}),
                        "tabs.list" => json!({"t": "res", "sid": msg["sid"], "id": msg["id"], "r":
                            {"tabs": [{"tab": 1, "title": "t", "url": "about:blank", "active": true}]}}),
                        _ => json!({"t": "res", "sid": msg["sid"], "id": msg["id"], "e": "scripted browser"}),
                    };
                    let _ = out.send(reply);
                }
                _ => {}
            }
        }
    });
    Script { seen, out: to_host, tasks: vec![host, writer, reader] }
}

async fn both_listed(dirs: &Dirs) {
    listed(dirs, 2).await;
}

async fn listed(dirs: &Dirs, count: usize) {
    let start = Instant::now();
    while browsers(dirs).await.0.len() < count {
        assert!(start.elapsed() < Duration::from_secs(10), "the browsers never showed up");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn dirs(tmp: &tempfile::TempDir) -> Dirs {
    Dirs::new(tmp.path().join("data"), tmp.path().join("run"))
}

#[tokio::test]
async fn the_first_browser_whose_person_allows_is_the_one_used() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs(&tmp);
    let a = start(&dirs, "alpha", "Alpha Chrome", Some((Duration::from_millis(50), false)));
    let b = start(&dirs, "beta", "Beta Brave", Some((Duration::from_millis(300), true)));
    both_listed(&dirs).await;
    let (list, bad) = browsers(&dirs).await;
    assert!(bad.is_empty(), "{bad:?}");
    let mut names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Alpha Chrome", "Beta Brave"]);
    assert_eq!(a.seen.asked.load(Ordering::SeqCst) + b.seen.asked.load(Ordering::SeqCst), 0, "listing asks nobody");

    let browser = RelayBrowser::connect(&dirs, None, "agent").await.unwrap();
    assert_eq!(browser.info().id, "beta", "alpha's person said no; beta's said yes");
    assert_eq!(a.seen.asked.load(Ordering::SeqCst), 1, "every browser was asked");
    assert_eq!(b.seen.asked.load(Ordering::SeqCst), 1);
    assert_eq!(a.seen.opened.load(Ordering::SeqCst), 0, "nothing was opened in the browser that said no");
    assert_eq!(b.seen.opened.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_prompts_of_the_others_are_withdrawn_once_one_is_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs(&tmp);
    let quick = start(&dirs, "quick", "Quick", Some((Duration::from_millis(100), true)));
    // this person never answers
    let away = start(&dirs, "away", "Away", None);
    both_listed(&dirs).await;
    let browser = RelayBrowser::connect(&dirs, None, "agent").await.unwrap();
    assert_eq!(browser.info().id, "quick");
    let start = Instant::now();
    while !away.seen.withdrawn.load(Ordering::SeqCst) {
        assert!(start.elapsed() < Duration::from_secs(10), "the other browser's prompt was left open");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(away.seen.opened.load(Ordering::SeqCst), 0);
    assert!(!quick.seen.withdrawn.load(Ordering::SeqCst), "the chosen browser's own prompt is not withdrawn");
}

#[tokio::test]
async fn when_everybody_says_no_it_is_an_error_and_nothing_is_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs(&tmp);
    let a = start(&dirs, "alpha", "Alpha", Some((Duration::from_millis(20), false)));
    let b = start(&dirs, "beta", "Beta", Some((Duration::from_millis(60), false)));
    both_listed(&dirs).await;
    let err = match RelayBrowser::connect(&dirs, None, "agent").await {
        Ok(_) => panic!("nobody allowed it"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Deny"), "{err}");
    assert_eq!(a.seen.opened.load(Ordering::SeqCst) + b.seen.opened.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancel_on_the_debugging_bar_ends_only_that_session() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = dirs(&tmp);
    let chrome = start(&dirs, "alpha", "Alpha Chrome", Some((Duration::from_millis(20), true)));
    listed(&dirs, 1).await;
    let first = RelayBrowser::connect(&dirs, Some("alpha"), "agent-one").await.unwrap();
    assert!(first.is_alive());
    // Chrome detaches the tab the session opened because the person pressed Cancel on the bar.
    let detached = json!({"t": "ev", "tab": 1, "sid": "s1", "m": "Nexo.detached", "p": {"reason": "canceled_by_user"}});
    chrome.out.send(detached).unwrap();
    let start = Instant::now();
    while first.is_alive() {
        assert!(start.elapsed() < Duration::from_secs(10), "the session never noticed the Cancel");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let err = first.navigate("about:blank").await.unwrap_err().to_string();
    assert!(err.contains("canceled_by_user") && err.contains("Connect again"), "{err}");
    // The browser is still there: asking again gives a fresh, working session.
    first.close().await.unwrap();
    let second = RelayBrowser::connect(&dirs, Some("alpha"), "agent-one").await.unwrap();
    assert!(second.is_alive());
    assert_eq!(chrome.seen.asked.load(Ordering::SeqCst), 2, "the person was asked again");
    second.navigate("about:blank").await.unwrap();
    second.close().await.unwrap();
}
