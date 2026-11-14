//! `RelayBrowser` end to end, without the person's Chrome: the agent talks to the real host (`nexo_browser_host::run`)
//! over the real local endpoint, and the extension end is a FAKE one that turns the relay messages into raw protocol
//! commands for a headless Chrome the test launches itself (own temporary profile, killed at the end). That is the
//! same translation the real extension does with `chrome.debugger`, so every verb of the relay backend is exercised.
//!
//! Skipped when no Chrome, Chromium or Edge is installed.

mod support;

use chromiumoxide::browser::{Browser as Chrome, BrowserConfig};
use futures::StreamExt;
use nexo_browser::{browsers, Error, RecordOptions, RelayBrowser, Wait};
use nexo_relay::chunk::{self, Reassembler};
use nexo_relay::{endpoint, frame, token, Dirs};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{serve, until, Cdp, WAIT};
use tokio::io::{duplex, DuplexStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const ID: &str = "chrome-test";

#[derive(Default)]
struct TabMap {
    next: i64,
    /// tab number -> (target id, session id)
    ids: HashMap<i64, (String, String)>,
    by_session: HashMap<String, i64>,
    by_target: HashMap<String, i64>,
}

/// What the real extension does with `chrome.debugger`, done with raw commands, plus a log of what it was asked.
struct Fake {
    cdp: Cdp,
    tabs: Mutex<TabMap>,
    out: mpsc::UnboundedSender<Value>,
    allow: AtomicBool,
    /// `cdp` requests that reached Chrome.
    reached: AtomicUsize,
    consents: AtomicUsize,
}

/// Events the real extension forwards (everything else stays in the browser).
const FORWARDED: [&str; 9] = [
    "Runtime.consoleAPICalled",
    "Runtime.exceptionThrown",
    "Log.entryAdded",
    "Network.requestWillBeSent",
    "Network.responseReceived",
    "Page.loadEventFired",
    "Page.screencastFrame",
    "Page.javascriptDialogOpening",
    "Network.loadingFailed",
];

impl Fake {
    async fn attach(&self, target: &str) -> Result<i64, String> {
        let r = self.cdp.call("Target.attachToTarget", json!({"targetId": target, "flatten": true}), None).await?;
        let session = r["sessionId"].as_str().ok_or("no session")?.to_string();
        let mut t = self.tabs.lock().unwrap();
        t.next += 1;
        let n = t.next;
        t.ids.insert(n, (target.to_string(), session.clone()));
        t.by_session.insert(session, n);
        t.by_target.insert(target.to_string(), n);
        Ok(n)
    }

    fn ids(&self, tab: i64) -> Result<(String, String), String> {
        self.tabs.lock().unwrap().ids.get(&tab).cloned().ok_or_else(|| format!("no tab {tab}"))
    }

    async fn request(&self, m: &str, p: &Value) -> Result<Value, String> {
        match m {
            "tabs.create" => {
                let r = self.cdp.call("Target.createTarget", json!({"url": "about:blank"}), None).await?;
                let tab = self.attach(r["targetId"].as_str().ok_or("no target")?).await?;
                Ok(json!({"tab": tab}))
            }
            "tabs.list" => {
                let r = self.cdp.call("Target.getTargets", json!({}), None).await?;
                let t = self.tabs.lock().unwrap();
                let list: Vec<Value> = r["targetInfos"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| {
                        let tab = t.by_target.get(i["targetId"].as_str()?)?;
                        Some(json!({"tab": tab, "title": i["title"], "url": i["url"], "active": false}))
                    })
                    .collect();
                Ok(json!({"tabs": list}))
            }
            "tabs.close" => {
                let tab = p["tab"].as_i64().ok_or("no tab")?;
                let (target, session) = self.ids(tab)?;
                {
                    let mut t = self.tabs.lock().unwrap();
                    t.ids.remove(&tab);
                    t.by_session.remove(&session);
                    t.by_target.remove(&target);
                }
                self.cdp.call("Target.closeTarget", json!({"targetId": target}), None).await?;
                Ok(json!({"tab": tab}))
            }
            "tabs.activate" => {
                let (target, _) = self.ids(p["tab"].as_i64().ok_or("no tab")?)?;
                self.cdp.call("Target.activateTarget", json!({"targetId": target}), None).await?;
                Ok(json!({}))
            }
            "window.resize" => {
                let (target, _) = self.ids(p["tab"].as_i64().ok_or("no tab")?)?;
                let w = self.cdp.call("Browser.getWindowForTarget", json!({"targetId": target}), None).await?;
                let bounds = json!({"windowState": "normal", "width": p["width"], "height": p["height"]});
                self.cdp
                    .call("Browser.setWindowBounds", json!({"windowId": w["windowId"], "bounds": bounds}), None)
                    .await?;
                Ok(json!({}))
            }
            "cdp" => {
                self.reached.fetch_add(1, Ordering::SeqCst);
                let (_, session) = self.ids(p["tab"].as_i64().ok_or("no tab")?)?;
                let method = p["method"].as_str().ok_or("no method")?;
                self.cdp.call(method, p["params"].clone(), Some(&session)).await
            }
            other => Err(format!("unknown request {other}")),
        }
    }

    /// One event of Chrome: forwarded when it belongs to one of the tabs, or turned into a tab opened or closed.
    async fn on_event(self: Arc<Self>, ev: Value) {
        let method = ev["method"].as_str().unwrap_or("").to_string();
        match method.as_str() {
            "Target.targetCreated" => {
                let info = &ev["params"]["targetInfo"];
                let opener =
                    info["openerId"].as_str().and_then(|o| self.tabs.lock().unwrap().by_target.get(o).copied());
                if let (Some(opener), Some(target), "page") =
                    (opener, info["targetId"].as_str(), info["type"].as_str().unwrap_or(""))
                {
                    if let Ok(tab) = self.attach(target).await {
                        let _ = self
                            .out
                            .send(json!({"t": "ev", "tab": tab, "m": "Nexo.tabOpened", "p": {"opener": opener}}));
                    }
                }
            }
            "Target.targetDestroyed" => {
                let target = ev["params"]["targetId"].as_str().unwrap_or("");
                let gone = {
                    let mut t = self.tabs.lock().unwrap();
                    let tab = t.by_target.remove(target);
                    if let Some(tab) = tab {
                        if let Some((_, s)) = t.ids.remove(&tab) {
                            t.by_session.remove(&s);
                        }
                    }
                    tab
                };
                if let Some(tab) = gone {
                    let _ = self.out.send(json!({"t": "ev", "tab": tab, "m": "Nexo.tabClosed", "p": {}}));
                }
            }
            m if FORWARDED.contains(&m) => {
                let tab = ev["sessionId"].as_str().and_then(|s| self.tabs.lock().unwrap().by_session.get(s).copied());
                if let Some(tab) = tab {
                    let _ = self.out.send(json!({"t": "ev", "tab": tab, "m": method, "p": ev["params"]}));
                }
            }
            _ => {}
        }
    }

    async fn on_host(self: Arc<Self>, msg: Value) {
        match msg["t"].as_str() {
            Some("consent") => {
                self.consents.fetch_add(1, Ordering::SeqCst);
                let ok = self.allow.load(Ordering::SeqCst);
                let why = if ok { Value::Null } else { json!("the person clicked Deny") };
                let _ = self.out.send(json!({"t": "consent_result", "sid": msg["sid"], "ok": ok, "why": why}));
            }
            Some("req") => {
                let reply = match self.request(msg["m"].as_str().unwrap_or(""), &msg["p"]).await {
                    Ok(r) => json!({"t": "res", "sid": msg["sid"], "id": msg["id"], "r": r}),
                    Err(e) => json!({"t": "res", "sid": msg["sid"], "id": msg["id"], "e": e}),
                };
                let _ = self.out.send(reply);
            }
            _ => {}
        }
    }
}

async fn send(w: &mut DuplexStream, msg: &Value, cid: u64) {
    for piece in chunk::encode(msg, cid) {
        if frame::write_frame(w, &piece).await.is_err() {
            return;
        }
    }
}

struct Rig {
    dirs: Dirs,
    fake: Arc<Fake>,
    port: u16,
    tasks: Vec<JoinHandle<()>>,
    _chrome: Chrome,
    _tmp: tempfile::TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
}

/// Launches the test's own headless Chrome and wires agent -> host -> fake extension -> Chrome. `None` without Chrome.
async fn rig(allow: bool) -> Option<Rig> {
    let exe = nexo_browser::find_browser()?;
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("chrome-profile");
    let cfg = BrowserConfig::builder()
        .chrome_executable(exe)
        .user_data_dir(&profile)
        .no_sandbox()
        .new_headless_mode()
        .build()
        .unwrap();
    let (chrome, mut handler) = Chrome::launch(cfg).await.unwrap();
    let keep = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let (cdp, mut events) = Cdp::connect(chrome.websocket_address()).await;
    cdp.call("Target.setDiscoverTargets", json!({"discover": true}), None).await.unwrap();

    let dirs = Dirs::new(tmp.path().join("data"), tmp.path().join("run"));
    let token = token::load_or_create(&dirs).unwrap();
    let (host_in, mut ext_w) = duplex(8 << 20);
    let (mut ext_r, host_out) = duplex(8 << 20);
    let host_dirs = dirs.clone();
    let host = tokio::spawn(async move {
        let _ = nexo_browser_host::run(host_in, host_out, host_dirs, token).await;
    });
    let (out, mut out_rx) = mpsc::unbounded_channel::<Value>();
    let fake = Arc::new(Fake {
        cdp,
        tabs: Mutex::default(),
        out,
        allow: AtomicBool::new(allow),
        reached: AtomicUsize::new(0),
        consents: AtomicUsize::new(0),
    });
    let mut tasks = vec![keep, host];
    // Messages to the host, in order, chunked like the real extension does.
    tasks.push(tokio::spawn(async move {
        let mut cid = 0;
        send(&mut ext_w, &json!({"t": "hello", "browser": {"id": ID, "name": "Test Chrome", "os": "Linux", "version": "1", "ext": "test"}}), 0).await;
        while let Some(m) = out_rx.recv().await {
            cid += 1;
            send(&mut ext_w, &m, cid).await;
        }
    }));
    let f = Arc::clone(&fake);
    tasks.push(tokio::spawn(async move {
        let mut parts = Reassembler::new();
        while let Ok(Some(raw)) = frame::read_json(&mut ext_r, frame::MAX_SOCKET).await {
            if let Ok(Some(msg)) = parts.push(raw) {
                tokio::spawn(Arc::clone(&f).on_host(msg));
            }
        }
    }));
    let f = Arc::clone(&fake);
    tasks.push(tokio::spawn(async move {
        while let Some(ev) = events.recv().await {
            tokio::spawn(Arc::clone(&f).on_event(ev));
        }
    }));
    let start = Instant::now();
    while endpoint::list(&dirs).is_empty() {
        assert!(start.elapsed() < WAIT, "the host never opened its endpoint");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (port, server) = serve().await;
    tasks.push(server);
    Some(Rig { dirs, fake, port, tasks, _chrome: chrome, _tmp: tmp })
}

macro_rules! rig_or_skip {
    ($allow:expr) => {
        match rig($allow).await {
            Some(r) => r,
            None => {
                eprintln!("skipped: no chrome");
                return;
            }
        }
    };
}

#[tokio::test]
async fn nothing_is_controlled_before_the_person_allows_it() {
    let rig = rig_or_skip!(false);
    // The browser is listed (it answers the probe) without any consent.
    let (found, bad) = browsers(&rig.dirs).await;
    assert!(bad.is_empty(), "{bad:?}");
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].id.as_str(), found[0].name.as_str()), (ID, "Test Chrome"));
    assert_eq!(rig.fake.consents.load(Ordering::SeqCst), 0, "listing must not ask the person anything");

    let denied = match RelayBrowser::connect(&rig.dirs, Some(ID), "agent-one").await {
        Ok(_) => panic!("a denied browser must not connect"),
        Err(e) => e,
    };
    assert!(matches!(denied, Error::Relay(_)), "{denied}");
    assert!(denied.to_string().contains("Deny"), "{denied}");
    assert_eq!(rig.fake.consents.load(Ordering::SeqCst), 1);
    assert_eq!(rig.fake.reached.load(Ordering::SeqCst), 0, "no protocol command may reach Chrome without consent");

    // Without an id, the one running browser is asked, and says no again.
    assert!(RelayBrowser::connect(&rig.dirs, None, "agent-one").await.is_err());
    assert_eq!(rig.fake.reached.load(Ordering::SeqCst), 0);

    // Once the person allows it, the same call works.
    rig.fake.allow.store(true, Ordering::SeqCst);
    let browser = RelayBrowser::connect(&rig.dirs, Some(ID), "agent-one").await.unwrap();
    assert_eq!(browser.info().name, "Test Chrome");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn an_unknown_browser_id_is_a_clear_error() {
    let rig = rig_or_skip!(true);
    let err = match RelayBrowser::connect(&rig.dirs, Some("nope"), "x").await {
        Ok(_) => panic!("there is no such browser"),
        Err(e) => e,
    };
    assert!(matches!(err, Error::Relay(_)), "{err}");
    let empty = tempfile::tempdir().unwrap();
    let none = Dirs::new(empty.path().join("data"), empty.path().join("run"));
    let err = match RelayBrowser::connect(&none, None, "x").await {
        Ok(_) => panic!("there is no browser"),
        Err(e) => e,
    };
    assert!(matches!(err, Error::NotConnected(_)) && err.to_string().contains("chrome-setup"), "{err}");
}

#[tokio::test]
async fn drives_the_page_and_keeps_to_its_own_tabs() {
    let rig = rig_or_skip!(true);
    let base = format!("http://127.0.0.1:{}", rig.port);
    let b = RelayBrowser::connect(&rig.dirs, None, "agent-one").await.unwrap();
    let br = &b;
    assert!(b.is_alive());
    assert!(b.others().is_empty());

    // The person's own tab (Chrome's first one) is not listed and is not touched.
    let tabs = b.tabs().await.unwrap();
    assert_eq!(tabs.len(), 1, "{tabs:?}");
    assert!(tabs[0].active);

    b.navigate(&format!("{base}/")).await.unwrap();
    let text = b.read_text(None).await.unwrap();
    assert!(text.contains("Hello relay"), "{text}");
    assert!(b.read_text(Some("#nope")).await.is_err());
    assert_eq!(b.tabs().await.unwrap()[0].title, "Relay page");

    // The snapshot has refs, and the verbs take them.
    let snap = b.snapshot().await.unwrap().render();
    assert!(snap.contains("button \"Send\"") && snap.contains("button \"Hit me\""), "{snap}");
    b.fill("Full name", "Ada").await.unwrap();
    b.select("#color", "Green").await.unwrap();
    b.set_checkbox("#agree", true).await.unwrap();
    b.click("Send").await.unwrap();
    assert_eq!(b.read_text(Some("#out")).await.unwrap(), "sent Ada/g/true");
    let report = b.fill_batch(&[("#name", "Grace"), ("#ghost", "x")]).await.unwrap();
    assert_eq!((report.filled.len(), report.missing.len()), (1, 1), "{report:?}");

    // Clicks land where the element is, including after it was scrolled to.
    b.click("Hit me").await.unwrap();
    assert_eq!(b.read_text(Some("#out")).await.unwrap(), "hit");
    b.hover("Hit me").await.unwrap();
    b.scroll("bottom").await.unwrap();
    b.scroll("top").await.unwrap();
    assert!(b.click("#nothing-here").await.is_err());
    let file_err = b.click("#file").await.unwrap_err().to_string();
    assert!(file_err.contains("upload"), "a click on a file input would open the system picker: {file_err}");

    // Keys and typing go to the focused element.
    b.click("#name").await.unwrap();
    b.press("Ctrl+A Backspace").await.unwrap();
    b.type_text("Zed").await.unwrap();
    b.press("Enter").await.unwrap();
    assert_eq!(b.read_text(Some("#out")).await.unwrap(), "sent Zed/g/true");
    assert!(b.press("Ctrl+Nope").await.is_err());

    // JavaScript: REPL semantics, thrown errors, results as JSON.
    assert_eq!(b.eval("1 + 1").await.unwrap(), "2");
    assert_eq!(b.eval("await Promise.resolve('x')").await.unwrap(), "\"x\"");
    assert_eq!(b.eval("document.title").await.unwrap(), "\"Relay page\"");
    assert_eq!(b.eval("undefined").await.unwrap(), "undefined");
    let thrown = b.eval("throw new Error('boom')").await.unwrap_err().to_string();
    assert!(thrown.contains("boom"), "{thrown}");

    // Console and network buffers fill from the forwarded events.
    until("the console", || async move {
        let c = br.console(None).await.unwrap();
        c.iter().any(|e| e.text == "page loaded") && c.iter().any(|e| e.text == "api ok true")
    })
    .await;
    let warn = b.console(Some("warning")).await.unwrap();
    assert!(warn.iter().any(|e| e.text == "careful"), "{warn:?}");
    b.eval("setTimeout(() => { throw new Error('later') }, 0)").await.unwrap();
    until("the uncaught error", || async move {
        br.console(Some("error")).await.unwrap().iter().any(|e| e.text.contains("later"))
    })
    .await;
    let api = format!("{base}/api");
    until("the request", || async {
        let n = br.network(Some("/api")).await.unwrap();
        n.iter().any(|e| e.url == api && e.status == Some(200))
    })
    .await;
    // network detail: what the extension passes on, never a body from the person's browser
    let seq = b.network(Some("/api")).await.unwrap().iter().find(|e| e.url == api).unwrap().seq;
    let d = b.network_detail(seq).await.unwrap();
    assert_eq!((d.entry.status, d.body.as_deref()), (Some(200), None));
    assert!(d.body_note.unwrap().contains("not read"));
    assert!(b.network_detail(99_999).await.is_err());

    // Mouse at a point: double and right clicks with modifiers, hover, wheel, a mouse-only drag.
    b.eval("window.ev = []; for (const t of ['dblclick', 'contextmenu', 'mousemove', 'mouseup', 'wheel']) document.addEventListener(t, (e) => window.ev.push(t + ':' + e.clientX + ':' + e.clientY + ':' + e.ctrlKey)); 0").await.unwrap();
    let ctrl = nexo_browser::Modifiers::parse("ctrl").unwrap();
    b.double_click(5.0, 6.0, ctrl).await.unwrap();
    b.right_click(7.0, 8.0, nexo_browser::Modifiers::default()).await.unwrap();
    b.hover_xy(9.0, 10.0).await.unwrap();
    b.scroll_xy(9.0, 10.0, 0.0, 40.0).await.unwrap();
    // the double click selected a word; a press on it would start a drag of the text, so it is cleared first
    b.eval("getSelection().removeAllRanges(); 0").await.unwrap();
    assert!(!b.drag((1.0, 1.0), (30.0, 2.0), nexo_browser::Modifiers::default()).await.unwrap());
    let ev = b.eval("window.ev.join(',')").await.unwrap();
    for want in
        ["dblclick:5:6:true", "contextmenu:7:8:false", "mousemove:9:10:false", "wheel:9:10:false", "mouseup:30:2:false"]
    {
        assert!(ev.contains(want), "{want} in {ev}");
    }
    // a drag that starts on selected text is called off, and the mouse still works after it
    b.eval("getSelection().selectAllChildren(document.body); window.ev = []; 0").await.unwrap();
    b.drag((20.0, 20.0), (60.0, 20.0), nexo_browser::Modifiers::default()).await.unwrap();
    b.double_click(11.0, 12.0, nexo_browser::Modifiers::default()).await.unwrap();
    let ev = b.eval("window.ev.join(',')").await.unwrap();
    assert!(ev.contains("dblclick:11:12:false"), "{ev}");

    // A dialog would block the page for good with nobody to answer it: it is closed and noted.
    assert_eq!(b.eval("alert('look'); 7").await.unwrap(), "7");
    assert_eq!(b.eval("confirm('sure?')").await.unwrap(), "false");
    until("the dialog note", || async move {
        br.console(Some("dialog")).await.unwrap().iter().any(|e| e.text.contains("look"))
    })
    .await;

    // Pictures: the viewport and the whole page.
    let png = b.screenshot(false).await.unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let full = b.screenshot(true).await.unwrap();
    assert_eq!(&full[..4], b"\x89PNG");

    // Files go to a file input without the picker.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cv.txt");
    std::fs::write(&file, "hello").unwrap();
    b.upload("#file", &file).await.unwrap();
    assert_eq!(b.eval("document.getElementById('file').files[0].name").await.unwrap(), "\"cv.txt\"");
    assert!(b.upload("#name", &file).await.unwrap_err().to_string().contains("not a file input"));
    assert!(b.upload("#ghost", &file).await.is_err());

    // History and waiting.
    b.navigate(&format!("{base}/second")).await.unwrap();
    assert!(b.read_text(None).await.unwrap().contains("second body"));
    b.back().await.unwrap();
    assert!(b.read_text(None).await.unwrap().contains("Hello relay"));
    b.forward().await.unwrap();
    assert!(b.read_text(None).await.unwrap().contains("second body"));
    b.reload().await.unwrap();
    b.wait(Wait::Selector("p", 2_000)).await.unwrap();
    assert!(matches!(b.wait(Wait::Selector("never there", 300)).await, Err(Error::NotFound(_))));
    b.wait(Wait::Millis(10)).await.unwrap();

    // Tabs: the agent's own, in order; the person's tab never shows.
    let second = b.tab_new(&format!("{base}/second")).await.unwrap();
    assert_eq!(second, 1);
    let tabs = b.tabs().await.unwrap();
    assert_eq!(tabs.len(), 2, "{tabs:?}");
    assert!(tabs[1].active && tabs[1].title == "Second page", "{tabs:?}");
    b.tab_switch(0).await.unwrap();
    assert!(b.tabs().await.unwrap()[0].active);
    assert!(b.tab_switch(9).await.is_err());
    assert!(b.tab_close(9).await.is_err());
    b.tab_close(1).await.unwrap();
    assert_eq!(b.tabs().await.unwrap().len(), 1);

    // A page that opens a window: the new tab is the session's too.
    b.navigate(&format!("{base}/")).await.unwrap();
    b.eval(&format!("window.open('{base}/second'); 1")).await.unwrap();
    until("the tab the page opened", || async move { br.tabs().await.unwrap().len() == 2 }).await;
    b.tab_switch(1).await.unwrap();
    until(
        "the opened tab to load",
        || async move { br.read_text(None).await.is_ok_and(|t| t.contains("second body")) },
    )
    .await;
    b.tab_close(1).await.unwrap();

    // The window can be resized (outer size).
    b.resize(900, 700).await.unwrap();
    until("the new size", || async move { br.eval("window.outerWidth").await.unwrap() == "900" }).await;

    // Recording: frames while the page changes, one GIF out.
    let gif = dir.path().join("rec.gif");
    assert!(b.record_stop_gif(&gif).await.is_err(), "nothing is recording yet");
    b.record_start(RecordOptions::default()).await.unwrap();
    assert!(b.record_start(RecordOptions::default()).await.is_err(), "one recording at a time");
    for color in ["red", "green", "blue"] {
        b.eval(&format!("document.body.style.background = '{color}'; 1")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let summary = b.record_stop_gif(&gif).await.unwrap();
    assert!(summary.frames >= 1 && summary.bytes > 0, "{summary:?}");
    assert_eq!(&std::fs::read(&gif).unwrap()[..6], b"GIF89a");

    assert_eq!(rig.fake.consents.load(Ordering::SeqCst), 1);
    b.close().await.unwrap();
}

#[tokio::test]
async fn two_sessions_do_not_see_each_others_tabs() {
    let rig = rig_or_skip!(true);
    let a = RelayBrowser::connect(&rig.dirs, Some(ID), "agent-a").await.unwrap();
    let b = RelayBrowser::connect(&rig.dirs, Some(ID), "agent-b").await.unwrap();
    assert_eq!(b.others(), ["agent-a"]);
    a.navigate(&format!("http://127.0.0.1:{}/second", rig.port)).await.unwrap();
    assert_eq!(a.tabs().await.unwrap().len(), 1);
    assert_eq!(b.tabs().await.unwrap().len(), 1);
    assert_eq!(a.tabs().await.unwrap()[0].title, "Second page");
    assert_eq!(b.tabs().await.unwrap()[0].url, "about:blank");
    let (found, _) = browsers(&rig.dirs).await;
    assert_eq!(found[0].in_use.len(), 2, "{found:?}");
    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test]
async fn closing_the_browser_ends_the_session_with_a_clear_error() {
    let rig = rig_or_skip!(true);
    let b = RelayBrowser::connect(&rig.dirs, Some(ID), "agent-one").await.unwrap();
    let br = &b;
    b.navigate("about:blank").await.unwrap();
    // Tasks 2 and 3 own the two ends of the extension's stdio; dropping them is the browser closing the connection.
    rig.tasks[2].abort();
    rig.tasks[3].abort();
    until("the session to notice", || async move { !br.is_alive() }).await;
    let err = b.navigate("about:blank").await.unwrap_err().to_string();
    assert!(err.contains("closed") || err.contains("connection"), "{err}");
}
