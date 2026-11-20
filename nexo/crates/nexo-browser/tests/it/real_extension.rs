//! The REAL extension in a headless Chrome the test starts itself (own empty profile in a temporary folder, killed at
//! the end), talking to the REAL host program through Chrome's native messaging, driven by `RelayBrowser`. The
//! person's own Chrome is never involved: nothing here attaches to it, reads its profile or kills it.
//!
//! It needs three things, so it is `#[ignore]`d and runs on request:
//!
//! ```text
//! cargo build -p nexo-cli                      # or -p nexo-browser-host: the program Chrome starts as the host
//! cargo test -p nexo-browser --test it real_extension -- --ignored --nocapture
//! ```
//!
//! `NEXO_HOST_BIN` names the host program (default: `nexo` or `nexo-browser-host` in the target folder). Chrome
//! must be a build that still allows `--enable-unsafe-extension-debugging` with `Extensions.loadUnpacked` (Chrome for
//! Testing, Chromium, or branded Chrome from version 137 with that flag).

use crate::support;

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
async fn the_real_extension_connects_without_prompt_and_drives_its_own_tabs() {
    let Some(chrome_exe) = nexo_browser::find_browser() else {
        eprintln!("skipped: no chrome");
        return;
    };
    let Some(host) = host_program() else {
        eprintln!("skipped: build the host program first (cargo build -p nexo-cli)");
        return;
    };
    let tmp = support::ChromeDir::new();
    let profile = tmp.path().join("chrome-profile");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&profile).unwrap();
    write_native_manifest(&profile, &host);
    // The host Chrome starts inherits this environment, so it keeps its token and socket inside the temporary folder.
    let dirs = Dirs::new(data.clone(), data.join("browser").join("run"));
    let ext =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../nexo-tools/assets/chrome-extension").canonicalize().unwrap();
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

    // Connection completes without any consent window or click.
    let browser = tokio::time::timeout(Duration::from_secs(30), RelayBrowser::connect(&dirs, Some(&id), "real-test"))
        .await
        .expect("automatic connection timed out")
        .expect("automatic connection failed");
    assert!(browser.is_alive());
    assert!(!targets(&cdp).await.iter().any(|t| t.2.contains("consent.html")));

    let (port, server) = serve().await;
    let base = format!("http://127.0.0.1:{port}");

    // A second session sees only its own tabs, not the first session's or the person's.
    let other = RelayBrowser::connect(&dirs, Some(&id), "other-test").await.unwrap();
    browser.navigate(&format!("{base}/?first-session")).await.unwrap();
    other.navigate(&format!("{base}/?second-session")).await.unwrap();
    let first_tabs = browser.tabs().await.unwrap();
    let other_tabs = other.tabs().await.unwrap();
    assert_eq!(first_tabs.len(), 1);
    assert_eq!(other_tabs.len(), 1);
    assert!(first_tabs[0].url.contains("first-session"), "{first_tabs:?}");
    assert!(other_tabs[0].url.contains("second-session"), "{other_tabs:?}");
    other.close().await.unwrap();

    // It drives a page of its own, in a tab group named nexo, and the person's tab stays out of it.
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

    // Simulate Chrome reporting Cancel on the debugging bar. A repeated grant must not revive
    // the revoked session, and requests to it remain blocked.
    let revoked = eval(
        &cdp,
        &ws,
        r#"(async () => {
      const [sid, session] = [...sessions].find(([, s]) => s.granted);
      revoke(sid);
      askConsent(sid);
      return { revoked: session.revoked, granted: session.granted };
    })()"#,
    )
    .await;
    assert_eq!(revoked, json!({"revoked": true, "granted": false}));
    assert!(browser.navigate("about:blank").await.is_err());

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
