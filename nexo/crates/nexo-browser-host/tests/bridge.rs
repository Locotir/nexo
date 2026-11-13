//! The host between real agents (over the real local endpoint) and a FAKE extension (over in-memory streams): no
//! browser is involved. The fake extension answers like the real one would and records everything the host sends it,
//! so the tests can say what did and did not reach the browser.

use nexo_relay::chunk::{self, Reassembler};
use nexo_relay::client::{Client, Error};
use nexo_relay::endpoint::{self, Dirs};
use nexo_relay::frame;
use nexo_relay::token;
use serde_json::{json, Value};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{duplex, DuplexStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(5);

/// What the fake extension saw and how it behaves.
struct Ext {
    seen: Mutex<Vec<Value>>,
    allow: AtomicBool,
    next_tab: AtomicI64,
    /// Messages to push to the host on the extension's initiative (CDP events, a tab opened by a page).
    push: mpsc::UnboundedSender<Value>,
}

impl Ext {
    fn seen(&self, kind: &str) -> Vec<Value> {
        self.seen.lock().unwrap().iter().filter(|m| m["t"] == kind).cloned().collect()
    }
}

struct Rig {
    dirs: Dirs,
    ext: Arc<Ext>,
    host: JoinHandle<io::Result<()>>,
    /// Dropping it closes the extension's end: the browser going away.
    ext_task: JoinHandle<()>,
    _tmp: tempfile::TempDir,
}

async fn send(w: &mut DuplexStream, msg: &Value, cid: u64) {
    for piece in chunk::encode(msg, cid) {
        frame::write_frame(w, &piece).await.unwrap();
    }
}

fn spawn_ext(
    mut from_host: DuplexStream,
    mut to_host: DuplexStream,
    id: &str,
    allow: bool,
) -> (Arc<Ext>, JoinHandle<()>) {
    let (push, mut pushed) = mpsc::unbounded_channel();
    let ext =
        Arc::new(Ext { seen: Mutex::default(), allow: AtomicBool::new(allow), next_tab: AtomicI64::new(1), push });
    let log = ext.clone();
    let id = id.to_string();
    let task = tokio::spawn(async move {
        let mut cid = 0;
        let mut parts = Reassembler::new();
        cid += 1;
        send(&mut to_host, &json!({"t": "hello", "browser": {"id": id, "name": "Fake Chrome", "os": "Linux", "version": "157", "ext": "test"}}), cid).await;
        loop {
            let msg = tokio::select! {
                pushed = pushed.recv() => match pushed { Some(m) => { cid += 1; send(&mut to_host, &m, cid).await; continue }, None => return },
                raw = frame::read_json(&mut from_host, frame::MAX_SOCKET) => match raw.unwrap() {
                    Some(raw) => match parts.push(raw).unwrap() { Some(m) => m, None => continue },
                    None => return,
                },
            };
            log.seen.lock().unwrap().push(msg.clone());
            let reply = match msg["t"].as_str() {
                Some("consent") => {
                    let ok = log.allow.load(Ordering::SeqCst);
                    Some(
                        json!({"t": "consent_result", "sid": msg["sid"], "ok": ok, "why": if ok { Value::Null } else { json!("the person said no") }}),
                    )
                }
                Some("req") => {
                    let r = match msg["m"].as_str().unwrap() {
                        "tabs.create" => json!({"tab": log.next_tab.fetch_add(1, Ordering::SeqCst)}),
                        // Every tab ever made, whoever owns it: the host must not pass on the ones that are not the session's.
                        "tabs.list" => {
                            json!({"tabs": (1..log.next_tab.load(Ordering::SeqCst)).map(|n| json!({"tab": n, "url": "about:blank"})).collect::<Vec<_>>()})
                        }
                        "tabs.close" => json!({"tab": msg["p"]["tab"]}),
                        "cdp" if msg["p"]["method"] == "Big.result" => json!({"data": "z".repeat(3_000_000)}),
                        "cdp" => json!({"echo": msg["p"], "sid": msg["sid"]}),
                        _ => json!({}),
                    };
                    Some(json!({"t": "res", "sid": msg["sid"], "id": msg["id"], "r": r}))
                }
                _ => None,
            };
            if let Some(r) = reply {
                cid += 1;
                send(&mut to_host, &r, cid).await;
            }
        }
    });
    (ext, task)
}

async fn rig_with(allow: bool, id: &str, dirs: Option<(Dirs, tempfile::TempDir)>) -> Rig {
    let (dirs, tmp) = dirs.unwrap_or_else(|| {
        let tmp = tempfile::tempdir().unwrap();
        (Dirs::new(tmp.path().join("data"), tmp.path().join("run")), tmp)
    });
    let token = token::load_or_create(&dirs).unwrap();
    let (host_in, ext_w) = duplex(8 << 20);
    let (ext_r, host_out) = duplex(8 << 20);
    let host = tokio::spawn(nexo_browser_host::run(host_in, host_out, dirs.clone(), token));
    let (ext, ext_task) = spawn_ext(ext_r, ext_w, id, allow);
    let start = std::time::Instant::now();
    while endpoint::list(&dirs).is_empty() && !host.is_finished() {
        assert!(start.elapsed() < WAIT, "the host never opened its endpoint");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Rig { dirs, ext, host, ext_task, _tmp: tmp }
}

async fn rig(allow: bool) -> Rig {
    rig_with(allow, "b-test", None).await
}

async fn agent(r: &Rig, name: &str) -> Client {
    Client::open(&r.dirs, "b-test", &format!("client-{name}"), name).await.unwrap()
}

fn remote(e: Error) -> String {
    match e {
        Error::Remote(w) => w,
        other => panic!("expected a refusal from the host, got {other}"),
    }
}

#[tokio::test]
async fn the_token_opens_the_door_and_nothing_else_does() {
    let r = rig(true).await;
    let c = agent(&r, "good").await;
    assert_eq!(c.info().id, "b-test");
    assert_eq!(c.info().name, "Fake Chrome");
    let wrong = endpoint::connect(&r.dirs, "b-test").await.unwrap();
    let err = Client::over(wrong, &"0".repeat(64), "client-bad", "intruder").await.err().unwrap();
    assert!(matches!(err, Error::Denied(_)), "{err}");
    // The refusal changed nothing for the session that was already in.
    assert!(c.request("tabs.list", json!({}), WAIT).await.map_err(remote).unwrap_err().contains("not allowed"));
    assert!(r.ext.seen("session").iter().all(|s| s["name"] != "intruder"), "a refused agent never became a session");
}

#[tokio::test]
async fn the_endpoint_is_private_to_the_user() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let r = rig(true).await;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(r.dirs.run()), 0o700);
        assert_eq!(mode(&r.dirs.run().join("browser-b-test.sock")), 0o600);
        assert_eq!(mode(&token::path(&r.dirs)), 0o600);
    }
}

#[tokio::test]
async fn nothing_reaches_the_browser_before_the_person_allows_it() {
    let r = rig(true).await;
    let c = agent(&r, "eager").await;
    for (m, p) in [
        ("tabs.create", json!({"url": "https://example.com"})),
        ("tabs.list", json!({})),
        ("cdp", json!({"tab": 1, "method": "Page.navigate"})),
    ] {
        let why = remote(c.request(m, p, WAIT).await.unwrap_err());
        assert!(why.contains("not allowed"), "{m}: {why}");
    }
    assert!(r.ext.seen("req").is_empty(), "no request got through: {:?}", r.ext.seen("req"));
}

#[tokio::test]
async fn a_no_in_the_browser_stays_a_no() {
    let r = rig(false).await;
    let c = agent(&r, "hopeful").await;
    let err = c.consent().await.unwrap_err();
    assert!(matches!(err, Error::Denied(ref w) if w.contains("person said no")), "{err}");
    let asked = r.ext.seen("consent");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["name"], "hopeful", "the prompt can say which agent asks");
    assert!(remote(c.request("tabs.create", json!({}), WAIT).await.unwrap_err()).contains("not allowed"));
    assert!(r.ext.seen("req").is_empty());
    // A later yes (the person changed their mind and the agent asks again) works.
    r.ext.allow.store(true, Ordering::SeqCst);
    c.consent().await.unwrap();
    assert_eq!(c.request("tabs.create", json!({}), WAIT).await.unwrap()["tab"], 1);
}

#[tokio::test]
async fn after_consent_a_session_works_its_own_tabs_and_only_those() {
    let r = rig(true).await;
    let c = agent(&r, "a").await;
    c.consent().await.unwrap();
    c.consent().await.unwrap(); // asking again does not ask the person again
    assert_eq!(r.ext.seen("consent").len(), 1);
    let tab =
        c.request("tabs.create", json!({"url": "https://example.com"}), WAIT).await.unwrap()["tab"].as_i64().unwrap();
    let out = c
        .request("cdp", json!({"tab": tab, "method": "Page.navigate", "params": {"url": "https://example.com"}}), WAIT)
        .await
        .unwrap();
    assert_eq!(out["echo"]["method"], "Page.navigate");
    assert!(out["sid"].as_str().unwrap().starts_with('s'), "the host names the session to the extension");
    // A tab nobody gave this session.
    let before = r.ext.seen("req").len();
    for m in ["cdp", "tabs.close", "tabs.activate", "window.resize"] {
        let why = remote(c.request(m, json!({"tab": 99, "method": "x"}), WAIT).await.unwrap_err());
        assert!(why.contains("not one of this session's tabs"), "{m}: {why}");
    }
    let why = remote(c.request("cdp", json!({"method": "x"}), WAIT).await.unwrap_err());
    assert!(why.contains("not one of this session's tabs"), "{why}");
    assert!(remote(c.request("Browser.close", json!({}), WAIT).await.unwrap_err()).contains("unknown request"));
    assert_eq!(r.ext.seen("req").len(), before, "refused requests never left the host");
    // Closing a tab ends the session's claim to it.
    c.request("tabs.close", json!({"tab": tab}), WAIT).await.unwrap();
    assert!(
        remote(c.request("cdp", json!({"tab": tab, "method": "x"}), WAIT).await.unwrap_err()).contains("not one of")
    );
}

#[tokio::test]
async fn two_sessions_never_see_each_others_tabs_or_events() {
    let r = rig(true).await;
    let (a, b) = (agent(&r, "a").await, agent(&r, "b").await);
    assert_eq!(b.others(), ["a"], "an agent is told who else is on the browser");
    a.consent().await.unwrap();
    b.consent().await.unwrap();
    let ta = a.request("tabs.create", json!({}), WAIT).await.unwrap()["tab"].as_i64().unwrap();
    let tb = b.request("tabs.create", json!({}), WAIT).await.unwrap()["tab"].as_i64().unwrap();
    assert_ne!(ta, tb);
    // B cannot reach A's tab; each list holds only its own.
    assert!(remote(b.request("cdp", json!({"tab": ta, "method": "x"}), WAIT).await.unwrap_err()).contains("not one of"));
    let listed =
        |v: Value| v["tabs"].as_array().unwrap().iter().map(|t| t["tab"].as_i64().unwrap()).collect::<Vec<_>>();
    assert_eq!(listed(a.request("tabs.list", json!({}), WAIT).await.unwrap()), vec![ta]);
    assert_eq!(listed(b.request("tabs.list", json!({}), WAIT).await.unwrap()), vec![tb]);
    // An event of A's tab goes to A alone.
    let (mut ea, mut eb) = (a.take_events().unwrap(), b.take_events().unwrap());
    r.ext.push.send(json!({"t": "ev", "tab": ta, "m": "Runtime.consoleAPICalled", "p": {"type": "log"}})).unwrap();
    r.ext.push.send(json!({"t": "ev", "tab": tb, "m": "Page.loadEventFired", "p": {}})).unwrap();
    let got = tokio::time::timeout(WAIT, ea.recv()).await.unwrap().unwrap();
    assert_eq!((got.tab, got.method.as_str()), (ta, "Runtime.consoleAPICalled"));
    let got = tokio::time::timeout(WAIT, eb.recv()).await.unwrap().unwrap();
    assert_eq!((got.tab, got.method.as_str()), (tb, "Page.loadEventFired"));
    assert!(ea.try_recv().is_err() && eb.try_recv().is_err(), "each got exactly its own");
    // A tab opened by A's page is A's, not B's.
    r.ext.push.send(json!({"t": "ev", "tab": 50, "m": "Nexo.tabOpened", "p": {"opener": ta}})).unwrap();
    let opened = tokio::time::timeout(WAIT, ea.recv()).await.unwrap().unwrap();
    assert_eq!(opened.method, "Nexo.tabOpened");
    a.request("cdp", json!({"tab": 50, "method": "x"}), WAIT).await.unwrap();
    assert!(remote(b.request("cdp", json!({"tab": 50, "method": "x"}), WAIT).await.unwrap_err()).contains("not one of"));
    // A tab opened by nobody's tab is nobody's.
    r.ext.push.send(json!({"t": "ev", "tab": 51, "m": "Nexo.tabOpened", "p": {"opener": 777}})).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(remote(a.request("cdp", json!({"tab": 51, "method": "x"}), WAIT).await.unwrap_err()).contains("not one of"));
}

#[tokio::test]
async fn sessions_are_announced_to_the_extension_and_closed_with_the_agent() {
    let r = rig(true).await;
    {
        let c = agent(&r, "short lived").await;
        c.consent().await.unwrap();
        c.request("tabs.create", json!({}), WAIT).await.unwrap();
    }
    let start = std::time::Instant::now();
    while r.ext.seen("session").len() < 2 {
        assert!(start.elapsed() < WAIT, "{:?}", r.ext.seen("session"));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let s = r.ext.seen("session");
    assert_eq!((s[0]["open"].clone(), s[0]["name"].clone()), (json!(true), json!("short lived")));
    assert_eq!((s[1]["open"].clone(), s[1]["sid"].clone()), (json!(false), s[0]["sid"].clone()));
    // The tab it made is nobody's now: a new session cannot take it over.
    let c = agent(&r, "next").await;
    c.consent().await.unwrap();
    assert!(remote(c.request("cdp", json!({"tab": 1, "method": "x"}), WAIT).await.unwrap_err()).contains("not one of"));
}

#[tokio::test]
async fn big_messages_cross_in_chunks_in_both_directions() {
    let r = rig(true).await;
    let c = agent(&r, "big").await;
    c.consent().await.unwrap();
    let tab = c.request("tabs.create", json!({}), WAIT).await.unwrap()["tab"].as_i64().unwrap();
    // Agent -> extension: 1.5 MB of parameters, over Chrome's 1 MiB limit for a host message.
    let params = json!({"expression": "q".repeat(1_500_000)});
    let out =
        c.request("cdp", json!({"tab": tab, "method": "Runtime.evaluate", "params": params}), WAIT).await.unwrap();
    assert_eq!(out["echo"]["params"]["expression"].as_str().unwrap().len(), 1_500_000);
    // Extension -> agent: a 3 MB answer, sent by the extension in pieces.
    let big = c.request("cdp", json!({"tab": tab, "method": "Big.result"}), WAIT).await.unwrap();
    assert_eq!(big["data"].as_str().unwrap().len(), 3_000_000);
}

#[tokio::test]
async fn when_the_browser_closes_the_port_the_agents_are_told_and_the_endpoint_goes() {
    let r = rig(true).await;
    let c = agent(&r, "left behind").await;
    c.consent().await.unwrap();
    r.ext_task.abort();
    let out = tokio::time::timeout(WAIT, r.host).await.unwrap().unwrap();
    assert!(out.is_ok(), "a closed port is a normal end: {out:?}");
    let start = std::time::Instant::now();
    while c.is_alive() {
        assert!(start.elapsed() < WAIT, "the agent was never told");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(matches!(c.request("tabs.list", json!({}), WAIT).await, Err(Error::Closed(_))));
    assert!(endpoint::list(&r.dirs).is_empty());
}

#[tokio::test]
async fn a_second_host_for_the_same_browser_is_refused_and_the_extension_is_told() {
    let r = rig(true).await;
    let token = token::load(&r.dirs).unwrap();
    let (host_in, ext_w) = duplex(1 << 20);
    let (ext_r, host_out) = duplex(1 << 20);
    let second = tokio::spawn(nexo_browser_host::run(host_in, host_out, r.dirs.clone(), token));
    let (ext2, _task) = spawn_ext(ext_r, ext_w, "b-test", true);
    let out = tokio::time::timeout(WAIT, second).await.unwrap().unwrap();
    assert_eq!(out.unwrap_err().kind(), io::ErrorKind::AddrInUse);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let told = ext2.seen("error");
    assert_eq!(told.len(), 1, "{:?}", ext2.seen.lock().unwrap());
    assert!(told[0]["why"].as_str().unwrap().contains("already serves"));
    // The first host is untouched.
    let c = agent(&r, "still works").await;
    c.consent().await.unwrap();
}

#[tokio::test]
async fn a_hello_without_a_usable_browser_id_ends_the_host() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = Dirs::new(tmp.path().join("data"), tmp.path().join("run"));
    let token = token::load_or_create(&dirs).unwrap();
    let (host_in, mut ext_w) = duplex(1 << 20);
    let (_ext_r, host_out) = duplex(1 << 20);
    let host = tokio::spawn(nexo_browser_host::run(host_in, host_out, dirs.clone(), token));
    frame::write_json(&mut ext_w, &json!({"t": "hello", "browser": {"id": "../../etc/x"}}), 4096).await.unwrap();
    let out = tokio::time::timeout(WAIT, host).await.unwrap().unwrap();
    assert_eq!(out.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert!(endpoint::list(&dirs).is_empty());
}
