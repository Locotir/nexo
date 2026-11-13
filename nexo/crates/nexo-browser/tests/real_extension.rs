//! The REAL extension in a headless Chrome the test starts itself (own empty profile in a temporary folder, killed at
//! the end), talking to the REAL host program through Chrome's native messaging, driven by `RelayBrowser`. The
//! person's own Chrome is never involved: nothing here attaches to it, reads its profile or kills it.
//!
//! It needs three things, so it is `#[ignore]`d and runs on request:
//!
//! ```text
//! cargo build -p nexo-cli                      # or -p nexo-browser-host: the program Chrome starts as the host
//! cargo test -p nexo-browser --test real_extension -- --ignored --nocapture
//! ```
//!
//! `NEXO_HOST_BIN` names the host program (default: `nexo` or `nexo-browser-host` in the target folder). Chrome
//! must be a build that still allows `--enable-unsafe-extension-debugging` with `Extensions.loadUnpacked` (Chrome for
//! Testing, Chromium, or branded Chrome from version 137 with that flag).

mod support;

use nexo_browser::{browsers, RelayBrowser};
use nexo_relay::{endpoint, Dirs};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use support::{serve, until, Cdp};

/// The id Chrome gives the extension: it comes from the `key` in its manifest.
const EXTENSION_ID: &str = "lollgkhihanglklkkahimoikiegabdon";

struct Chrome(Child);

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn host_program() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NEXO_HOST_BIN").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let target = std::env::current_exe().ok()?.parent()?.parent()?.to_path_buf();
    ["nexo", "nexo-browser-host"]
        .iter()
        .map(|n| target.join(format!("{n}{}", std::env::consts::EXE_SUFFIX)))
        .find(|p| p.is_file())
}

/// `Target.getTargets`, as a list of `(targetId, type, url)`.
async fn targets(cdp: &Cdp) -> Vec<(String, String, String)> {
    let v = cdp.call("Target.getTargets", json!({}), None).await.unwrap();
    let list = v["targetInfos"].as_array().cloned().unwrap_or_default();
    list.iter().map(|t| (str_of(&t["targetId"]), str_of(&t["type"]), str_of(&t["url"]))).collect()
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

async fn attach(cdp: &Cdp, target: &str) -> String {
    let v = cdp.call("Target.attachToTarget", json!({"targetId": target, "flatten": true}), None).await.unwrap();
    str_of(&v["sessionId"])
}

async fn eval(cdp: &Cdp, session: &str, expr: &str) -> Value {
    let v = cdp
        .call(
            "Runtime.evaluate",
            json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
            Some(session),
        )
        .await
        .unwrap();
    v["result"]["value"].clone()
}

fn write_native_manifest(profile: &Path, host: &Path) {
    let dir = profile.join("NativeMessagingHosts");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = json!({
        "name": "dev.nexo.browser",
        "description": "nexo browser host (test)",
        "path": host.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    });
    std::fs::write(dir.join("dev.nexo.browser.json"), manifest.to_string()).unwrap();
}

#[tokio::test]
#[ignore = "starts a headless Chrome and needs the host program built; see the file header"]
async fn the_real_extension_asks_first_and_then_drives_its_own_tabs() {
    let Some(chrome_exe) = nexo_browser::find_browser() else {
        eprintln!("skipped: no chrome");
        return;
    };
    let Some(host) = host_program() else {
        eprintln!("skipped: build the host program first (cargo build -p nexo-cli)");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("chrome-profile");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&profile).unwrap();
    write_native_manifest(&profile, &host);
    // The host Chrome starts inherits this environment, so it keeps its token and socket inside the temporary folder.
    let dirs = Dirs::new(data.clone(), data.join("browser").join("run"));
    let ext = Path::new(env!("CARGO_MANIFEST_DIR")).join("../nexo-cli/assets/chrome-extension").canonicalize().unwrap();
    let child = Command::new(&chrome_exe)
        .arg("--headless=new")
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--remote-debugging-port=0", "--no-sandbox", "--disable-gpu", "--no-first-run"])
        .arg("--enable-unsafe-extension-debugging")
        .arg("about:blank")
        .env("NEXO_DATA_DIR", &data)
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _chrome = Chrome(child);

    // Chrome writes where it listens into the profile once it is up.
    let active = profile.join("DevToolsActivePort");
    let start = Instant::now();
    let ws = loop {
        if let Ok(text) = std::fs::read_to_string(&active) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                break format!("ws://127.0.0.1:{port}{path}");
            }
        }
        assert!(start.elapsed() < Duration::from_secs(30), "Chrome never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let (cdp, _events) = Cdp::connect(&ws).await;
    let loaded = cdp.call("Extensions.loadUnpacked", json!({"path": ext.to_string_lossy()}), None).await;
    let loaded = match loaded {
        Ok(v) => v,
        Err(e) => {
            eprintln!("skipped: this Chrome does not load unpacked extensions over DevTools ({e})");
            return;
        }
    };
    assert_eq!(loaded["id"], EXTENSION_ID);

    // The extension starts its host on load and announces itself: the browser is listed, with no question asked yet.
    let start = Instant::now();
    while endpoint::list(&dirs).is_empty() {
        assert!(start.elapsed() < Duration::from_secs(30), "the extension never reached the host");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (found, bad) = browsers(&dirs).await;
    assert!(bad.is_empty(), "{bad:?}");
    assert_eq!(found.len(), 1, "{found:?}");
    let id = found[0].id.clone();
    assert!(id.len() > 12, "the id is a name and a random part: {id}");
    assert!(
        !targets(&cdp).await.iter().any(|t| t.2.contains("consent.html")),
        "listing the browser must not open the prompt"
    );

    // Asking to connect opens the prompt in the browser; nothing is controlled before it is answered.
    let dirs2 = dirs.clone();
    let id2 = id.clone();
    let connecting = tokio::spawn(async move { RelayBrowser::connect(&dirs2, Some(&id2), "real-test").await });
    let mut prompt = None;
    let start = Instant::now();
    while prompt.is_none() {
        assert!(start.elapsed() < Duration::from_secs(30), "no prompt appeared");
        prompt = targets(&cdp).await.into_iter().find(|t| t.2.contains("consent.html"));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (prompt_id, _, prompt_url) = prompt.unwrap();
    assert!(prompt_url.contains("name=real-test"), "{prompt_url}");
    let page = attach(&cdp, &prompt_id).await;
    let heading = eval(&cdp, &page, "document.querySelector('h1').textContent").await;
    assert_eq!(heading, "Allow nexo to control this browser?");
    assert_eq!(
        eval(&cdp, &page, "document.activeElement && document.activeElement.id").await,
        "deny",
        "Deny has the focus"
    );

    // A script cannot click for the person: the click is not trusted, so the prompt stays and nothing connects.
    eval(&cdp, &page, "document.getElementById('allow').click(); true").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!connecting.is_finished(), "a script's click must not let a session in");
    assert!(targets(&cdp).await.iter().any(|t| t.2.contains("consent.html")), "the prompt is still open");

    // A real click (the browser's own input pipeline) allows.
    let rect = eval(
        &cdp,
        &page,
        "(() => { const r = document.getElementById('allow').getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2}; })()",
    )
    .await;
    let (x, y) = (rect["x"].as_f64().unwrap(), rect["y"].as_f64().unwrap());
    for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
        cdp.call(
            "Input.dispatchMouseEvent",
            json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
            Some(&page),
        )
        .await
        .unwrap();
    }
    let browser = tokio::time::timeout(Duration::from_secs(30), connecting)
        .await
        .expect("the session never opened after Allow")
        .unwrap()
        .expect("connect failed after Allow");
    assert!(browser.is_alive());

    // It drives a page of its own, in a tab group named nexo, and the person's tab stays out of it.
    let (port, server) = serve().await;
    let base = format!("http://127.0.0.1:{port}");
    browser.navigate(&format!("{base}/")).await.unwrap();
    assert!(browser.read_text(None).await.unwrap().contains("Hello relay"));
    browser.fill("Full name", "Ada").await.unwrap();
    browser.click("Send").await.unwrap();
    assert_eq!(browser.read_text(Some("#out")).await.unwrap(), "sent Ada/r/false");
    let shot = browser.screenshot(false).await.unwrap();
    assert!(shot.len() > 500, "a screenshot came back");
    let tabs = browser.tabs().await.unwrap();
    assert!(tabs.iter().any(|t| t.title == "Relay page"), "{tabs:?}");

    // Chrome's own view: one group called nexo that holds the agent's tab.
    let worker = targets(&cdp)
        .await
        .into_iter()
        .find(|t| t.1 == "service_worker" && t.2.contains(EXTENSION_ID))
        .expect("the extension's service worker");
    let ws = attach(&cdp, &worker.0).await;
    let groups = eval(&cdp, &ws, "chrome.tabGroups.query({}).then(gs => gs.map(g => g.title + ':' + g.color))").await;
    assert_eq!(groups, json!(["nexo:blue"]));

    // Scripts run in the page, and the browser's own pages are off limits.
    assert_eq!(browser.eval("1 + 1").await.unwrap(), "2");
    let refused = browser.navigate("chrome://settings").await.expect_err("browser pages are refused");
    assert!(refused.to_string().contains("own pages"), "{refused}");

    // Closing ends the session; the tabs are the person's to keep or close.
    browser.close().await.unwrap();
    until("the session to be gone from the extension", || async {
        eval(
            &cdp,
            &ws,
            "chrome.debugger.getTargets().then(ts => ts.filter(t => t.attached && t.type === 'page').length)",
        )
        .await
            == json!(0)
    })
    .await;
    server.abort();
}
