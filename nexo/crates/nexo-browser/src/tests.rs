//! Headless and network-free: the page is a local file.

use super::*;

const PAGE: &str = r##"<!doctype html><html><body>
<h1>Nexo test page</h1>
<p>A paragraph of text.</p>
<button id="b" onclick="document.getElementById('out').textContent='clicked'">Press me</button>
<a href="#x">Plain link</a>
<div id="out">waiting</div>
</body></html>"##;

const FORM: &str = r##"<!doctype html><html><body>
<form id="f" onsubmit="event.preventDefault(); document.getElementById('res').textContent = 'sent ' + new URLSearchParams(new FormData(this)).toString();">
<label for="name">Full name</label><input id="name" name="name">
<label>Email <input id="email" name="email" type="email" placeholder="you@example.com"></label>
<textarea id="msg" name="msg" aria-label="Message"></textarea>
<label for="country">Country</label>
<select id="country" name="country"><option value="es">Spain</option><option value="us">United States</option></select>
<label><input type="checkbox" id="agree" name="agree"> I agree</label>
<button type="submit">Send</button>
</form>
<div id="res">idle</div>
<script>
window.log = [];
for (const t of ['input', 'change']) document.addEventListener(t, (e) => window.log.push(t + ':' + e.target.id));
</script>
</body></html>"##;

#[test]
fn ref_shape() {
    assert!(is_ref("e1") && is_ref("e42"));
    assert!(!is_ref("e") && !is_ref("e1x") && !is_ref("button") && !is_ref("E1"));
}

#[test]
fn render_is_one_line_per_element() {
    let s = Snapshot {
        elements: vec![
            Element { reference: "e1".into(), role: "button".into(), name: "Submit".into() },
            Element { reference: "e2".into(), role: "link".into(), name: "say \"hi\"".into() },
        ],
    };
    assert_eq!(s.render(), "e1 button \"Submit\"\ne2 link \"say \\\"hi\\\"\"");
    assert_eq!(s.find("e2").map(|e| e.role.as_str()), Some("link"));
}

#[tokio::test]
async fn drives_a_local_page() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("page.html");
    std::fs::write(&file, PAGE).unwrap();
    let url = format!("file://{}", file.display());

    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate(&url).await.unwrap();

    assert!(browser.read_text(None).await.unwrap().contains("Nexo test page"));
    assert_eq!(browser.read_text(Some("#out")).await.unwrap(), "waiting");
    assert!(matches!(browser.read_text(Some("#nope")).await, Err(Error::NotFound(_))));

    let snap = browser.snapshot().await.unwrap();
    let button = snap.elements.iter().find(|e| e.role == "button").expect("button in snapshot");
    assert_eq!(button.name, "Press me");
    assert!(snap.render().contains("button \"Press me\""));
    // A second snapshot keeps the refs.
    assert_eq!(browser.snapshot().await.unwrap(), snap);

    let shot = browser.screenshot(false).await.unwrap();
    assert_eq!(&shot[..4], b"\x89PNG");

    browser.click(&button.reference).await.unwrap();
    assert_eq!(browser.read_text(Some("#out")).await.unwrap(), "clicked");

    // Click by visible text (the link only changes the hash, so this must simply succeed).
    browser.click("Plain link").await.unwrap();
    assert!(matches!(browser.click("no such thing").await, Err(Error::NotFound(_))));

    browser.close().await.unwrap();
}

fn write_page(html: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("page.html");
    std::fs::write(&file, html).unwrap();
    let url = format!("file://{}", file.display());
    (dir, url)
}

#[tokio::test]
async fn fills_and_submits_a_form() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let (_dir, url) = write_page(FORM);
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate(&url).await.unwrap();

    // By CSS selector, by label text and by placeholder.
    browser.fill("#name", "Ada Lovelace").await.unwrap();
    assert_eq!(browser.eval_as::<String>("document.getElementById('name').value").await.unwrap(), "Ada Lovelace");
    browser.fill("you@example.com", "ada@example.com").await.unwrap();
    assert_eq!(browser.eval_as::<String>("document.getElementById('email').value").await.unwrap(), "ada@example.com");
    let log = browser.eval_as::<String>("JSON.stringify(window.log)").await.unwrap();
    assert!(log.contains("input:name") && log.contains("change:name"), "events not fired: {log}");

    // Refs from a snapshot keep working after fills.
    let snap = browser.snapshot().await.unwrap();
    let name_ref = snap.elements.iter().find(|e| e.name == "Full name").expect("name field").reference.clone();
    browser.fill(&name_ref, "Grace Hopper").await.unwrap();
    assert_eq!(browser.snapshot().await.unwrap(), snap);

    // Two fields at once, one missing target, one field that cannot take a value.
    let report = browser
        .fill_batch(&[("Full name", "Ada"), ("Message", "hello there"), ("No such field", "x"), ("Send", "y")])
        .await
        .unwrap();
    assert_eq!(report.filled, vec!["Full name", "Message"]);
    assert_eq!(report.missing, vec!["No such field", "Send"]);
    assert!(!report.complete());
    assert_eq!(browser.eval_as::<String>("document.getElementById('name').value").await.unwrap(), "Ada");
    assert_eq!(browser.eval_as::<String>("document.getElementById('msg').value").await.unwrap(), "hello there");

    // A form in one call, mixing text, select and checkbox.
    let all = browser.fill_batch(&[("#name", "Ada"), ("#country", "United States"), ("#agree", "true")]).await.unwrap();
    assert!(all.complete(), "{all:?}");

    browser.select("Country", "Spain").await.unwrap();
    assert_eq!(browser.eval_as::<String>("document.getElementById('country').value").await.unwrap(), "es");
    browser.select("#country", "us").await.unwrap();
    assert_eq!(browser.eval_as::<String>("document.getElementById('country').value").await.unwrap(), "us");
    assert!(matches!(browser.select("#country", "Mars").await, Err(Error::Unusable { .. })));
    assert!(matches!(browser.select("#name", "x").await, Err(Error::Unusable { .. })));
    let log = browser.eval_as::<String>("JSON.stringify(window.log)").await.unwrap();
    assert!(log.contains("change:country"), "no change on select: {log}");

    browser.set_checkbox("I agree", false).await.unwrap();
    assert!(!browser.eval_as::<bool>("document.getElementById('agree').checked").await.unwrap());
    browser.set_checkbox("#agree", true).await.unwrap();
    assert!(browser.eval_as::<bool>("document.getElementById('agree').checked").await.unwrap());
    let log = browser.eval_as::<String>("JSON.stringify(window.log)").await.unwrap();
    assert!(log.contains("change:agree"), "no change on checkbox: {log}");

    // Enter in a focused field submits the form.
    browser.fill("#name", "Ada").await.unwrap();
    browser.press("Enter").await.unwrap();
    let res = browser.read_text(Some("#res")).await.unwrap();
    assert!(res.starts_with("sent ") && res.contains("name=Ada") && res.contains("country=us"), "{res}");

    // Wait for something that arrives 1.5 s after it is armed, then for something that never does. Armed here
    // and not at page load: on a loaded machine the steps above took longer than a load-time delay.
    browser
        .eval("setTimeout(() => { const p = document.createElement('p'); p.id = 'late'; p.textContent = 'arrived'; document.body.appendChild(p); }, 1500); 0")
        .await
        .unwrap();
    assert!(matches!(browser.read_text(Some("#late")).await, Err(Error::NotFound(_))));
    browser.wait(Wait::Selector("#late", 5_000)).await.unwrap();
    assert_eq!(browser.read_text(Some("#late")).await.unwrap(), "arrived");
    assert!(matches!(browser.wait(Wait::Selector("#never", 200)).await, Err(Error::NotFound(_))));
    browser.wait(Wait::Millis(10)).await.unwrap();

    browser.hover("Send").await.unwrap();
    browser.scroll("bottom").await.unwrap();
    browser.scroll("top").await.unwrap();
    browser.scroll("#res").await.unwrap();
    assert!(matches!(browser.scroll("#never").await, Err(Error::NotFound(_))));
    assert!(matches!(browser.fill("#never", "x").await, Err(Error::NotFound(_))));

    browser.close().await.unwrap();
}

#[tokio::test]
async fn persistent_profile_keeps_state() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let (_dir, url) = write_page(PAGE);
    let profile = tempfile::tempdir().unwrap();
    let opts = || LaunchOptions { profile: Profile::Persistent(profile.path().join("p")), ..LaunchOptions::default() };

    let first = Browser::launch(opts()).await.unwrap();
    first.navigate(&url).await.unwrap();
    first.eval_as::<String>("(localStorage.setItem('seen', 'yes'), 'ok')").await.unwrap();
    first.close().await.unwrap();
    assert!(profile.path().join("p").is_dir(), "persistent profile must survive close");

    let second = Browser::launch(opts()).await.unwrap();
    second.navigate(&url).await.unwrap();
    assert_eq!(second.eval_as::<String>("localStorage.getItem('seen') || 'none'").await.unwrap(), "yes");
    second.close().await.unwrap();
}

const POWER: &str = r##"<!doctype html><html><body>
<input id="file" type="file">
<input id="txt" value="select all of this">
<div id="flags">none</div>
<canvas id="cv" width="200" height="100" style="position:absolute;left:0;top:200px"></canvas>
<script>
console.log('hello from the page', 42);
console.error('something broke');
window.clicks = [];
document.getElementById('cv').addEventListener('click', (e) => window.clicks.push([e.offsetX, e.offsetY]));
document.getElementById('txt').addEventListener('keydown', (e) => {
  if (e.key === 'Tab' && e.shiftKey) document.getElementById('flags').textContent = 'shift-tab';
});
</script>
</body></html>"##;

fn skip_without_chrome() -> bool {
    let none = find_browser().is_none();
    if none {
        eprintln!("skipped: no chrome");
    }
    none
}

/// Retries `read` for up to 3 seconds; events cross a channel and may land a moment after the page ran.
async fn eventually<T>(mut read: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..60 {
        if let Some(v) = read().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not met in time");
}

#[tokio::test]
async fn eval_console_network_and_resize() {
    if skip_without_chrome() {
        return;
    }
    let (_dir, url) = write_page(POWER);
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate(&url).await.unwrap();

    assert_eq!(browser.eval("1 + 2").await.unwrap(), "3");
    assert_eq!(browser.eval("({a: [1, 'x']})").await.unwrap(), r#"{"a":[1,"x"]}"#);
    assert_eq!(browser.eval("Promise.resolve('later')").await.unwrap(), "\"later\"");
    assert_eq!(browser.eval("void 0").await.unwrap(), "undefined");
    assert!(browser.eval("throw new Error('boom')").await.is_err());

    let all = eventually(async || Some(browser.console(None).await.unwrap()).filter(|c| c.len() >= 2)).await;
    let log = all.iter().find(|c| c.level == "log").expect("console.log captured");
    assert_eq!(log.text, "hello from the page 42");
    assert!(log.ts > 0.0);
    let errors = browser.console(Some("^error$")).await.unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].text, "something broke");
    assert_eq!(browser.console(Some("hello")).await.unwrap().len(), 1);
    assert!(matches!(browser.console(Some("(")).await, Err(Error::Unusable { .. })));

    let net = eventually(async || {
        Some(browser.network(None).await.unwrap()).filter(|n| n.iter().any(|e| e.status.is_some()))
    })
    .await;
    assert!(net.iter().any(|e| e.url == url && e.method == "GET"), "{net:?}");
    assert!(browser.network(Some("no-such-url")).await.unwrap().is_empty());

    browser.resize(640, 480).await.unwrap();
    assert_eq!(browser.eval("window.innerWidth").await.unwrap(), "640");
    assert_eq!(browser.eval("window.innerHeight").await.unwrap(), "480");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn history_reload_and_tabs() {
    if skip_without_chrome() {
        return;
    }
    let (_a, one) = write_page("<html><body><p id=p>page one</p></body></html>");
    let (_b, two) = write_page("<html><body><p id=p>page two</p></body></html>");
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate(&one).await.unwrap();
    browser.navigate(&two).await.unwrap();

    browser.back().await.unwrap();
    assert_eq!(browser.read_text(Some("#p")).await.unwrap(), "page one");
    // The first history entry is the blank page the browser started on; nothing lies before it.
    browser.back().await.unwrap();
    assert!(matches!(browser.back().await, Err(Error::Unusable { .. })));
    browser.forward().await.unwrap();
    browser.forward().await.unwrap();
    assert!(matches!(browser.forward().await, Err(Error::Unusable { .. })));
    assert_eq!(browser.read_text(Some("#p")).await.unwrap(), "page two");
    browser.eval("document.title = 'changed'").await.unwrap();
    browser.reload().await.unwrap();
    assert_eq!(browser.eval("document.title").await.unwrap(), "\"\"");

    let before = browser.tabs().await.unwrap();
    assert_eq!(before.len(), 1);
    assert!(before[0].active && before[0].url == two);
    let index = browser.tab_new(&one).await.unwrap();
    assert_eq!(index, 1);
    let tabs = browser.tabs().await.unwrap();
    assert_eq!(tabs.len(), 2);
    assert!(tabs[1].active && !tabs[0].active && tabs[1].url == one);
    assert_eq!(browser.read_text(Some("#p")).await.unwrap(), "page one");
    browser.tab_switch(0).await.unwrap();
    assert_eq!(browser.read_text(Some("#p")).await.unwrap(), "page two");
    assert!(matches!(browser.tab_switch(5).await, Err(Error::Unusable { .. })));
    browser.tab_close(1).await.unwrap();
    assert_eq!(browser.tabs().await.unwrap().len(), 1);
    assert!(matches!(browser.tab_close(0).await, Err(Error::Unusable { .. })));
    assert_eq!(browser.read_text(Some("#p")).await.unwrap(), "page two");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn upload_keys_and_raw_input() {
    if skip_without_chrome() {
        return;
    }
    let (dir, url) = write_page(POWER);
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate(&url).await.unwrap();

    let file = dir.path().join("note.txt");
    std::fs::write(&file, "hello").unwrap();
    browser.upload("#file", &file).await.unwrap();
    assert_eq!(browser.eval("document.getElementById('file').files.length").await.unwrap(), "1");
    assert_eq!(browser.eval("document.getElementById('file').files[0].name").await.unwrap(), "\"note.txt\"");
    assert!(matches!(browser.upload("#txt", &file).await, Err(Error::Unusable { .. })));
    assert!(matches!(browser.upload("#nope", &file).await, Err(Error::NotFound(_))));
    assert!(matches!(browser.upload("#file", dir.path().join("missing")).await, Err(Error::Io(_))));

    // Ctrl+A selects the whole text of the focused field.
    browser.fill("#txt", "select all of this").await.unwrap();
    browser.eval("(() => { const t = document.getElementById('txt'); t.setSelectionRange(0, 0); })()").await.unwrap();
    browser.press("Ctrl+A").await.unwrap();
    let selected = "(() => { const t = document.getElementById('txt'); return t.selectionEnd - t.selectionStart; })()";
    assert_eq!(browser.eval(selected).await.unwrap(), "18");
    // Typing replaces the selection.
    browser.type_text("typed").await.unwrap();
    assert_eq!(browser.eval("document.getElementById('txt').value").await.unwrap(), "\"typed\"");
    // Shift+Tab reaches the page as a keydown with shiftKey set.
    browser.press("Shift+Tab").await.unwrap();
    assert_eq!(browser.read_text(Some("#flags")).await.unwrap(), "shift-tab");
    assert!(matches!(browser.press("Ctrl+NoSuchKey").await, Err(Error::Unusable { .. })));
    assert!(matches!(browser.press("Hyper+A").await, Err(Error::Unusable { .. })));

    // A raw click lands on the canvas at the given viewport point.
    browser.click_xy(50.0, 250.0).await.unwrap();
    assert_eq!(browser.eval("JSON.stringify(window.clicks)").await.unwrap(), r#""[[50,50]]""#);

    browser.close().await.unwrap();
}

const ANIMATED: &str = r##"<!doctype html><html><body style="margin:0">
<div id="box" style="width:200px;height:120px"></div>
<script>
let n = 0;
setInterval(() => { document.getElementById('box').style.background = ['#e22', '#2e2', '#22e'][n++ % 3]; }, 40);
</script>
</body></html>"##;

#[tokio::test]
async fn records_a_gif() {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("anim.html");
    std::fs::write(&file, ANIMATED).unwrap();
    let out = dir.path().join("rec.gif");

    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate(&format!("file://{}", file.display())).await.unwrap();
    assert!(matches!(browser.record_stop_gif(&out).await, Err(Error::Unusable { .. })));
    browser.record_start(RecordOptions::default()).await.unwrap();
    assert!(matches!(browser.record_start(RecordOptions::default()).await, Err(Error::Unusable { .. })));
    browser.wait(Wait::Millis(500)).await.unwrap();
    let summary = browser.record_stop_gif(&out).await.unwrap();

    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"GIF8"));
    assert_eq!(summary.bytes, bytes.len() as u64);
    let mut decoder = gif::DecodeOptions::new().read_info(std::io::Cursor::new(&bytes)).unwrap();
    let mut frames = 0;
    while decoder.read_next_frame().unwrap().is_some() {
        frames += 1;
    }
    assert!(frames > 1, "expected several frames, got {frames}");
    assert_eq!(frames, summary.frames);

    browser.close().await.unwrap();
}
