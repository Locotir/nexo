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
setTimeout(() => { const p = document.createElement('p'); p.id = 'late'; p.textContent = 'arrived'; document.body.appendChild(p); }, 300);
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
    assert_eq!(browser.eval::<String>("document.getElementById('name').value").await.unwrap(), "Ada Lovelace");
    browser.fill("you@example.com", "ada@example.com").await.unwrap();
    assert_eq!(browser.eval::<String>("document.getElementById('email').value").await.unwrap(), "ada@example.com");
    let log = browser.eval::<String>("JSON.stringify(window.log)").await.unwrap();
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
    assert_eq!(browser.eval::<String>("document.getElementById('name').value").await.unwrap(), "Ada");
    assert_eq!(browser.eval::<String>("document.getElementById('msg').value").await.unwrap(), "hello there");

    // A form in one call, mixing text, select and checkbox.
    let all = browser.fill_batch(&[("#name", "Ada"), ("#country", "United States"), ("#agree", "true")]).await.unwrap();
    assert!(all.complete(), "{all:?}");

    browser.select("Country", "Spain").await.unwrap();
    assert_eq!(browser.eval::<String>("document.getElementById('country').value").await.unwrap(), "es");
    browser.select("#country", "us").await.unwrap();
    assert_eq!(browser.eval::<String>("document.getElementById('country').value").await.unwrap(), "us");
    assert!(matches!(browser.select("#country", "Mars").await, Err(Error::Unusable { .. })));
    assert!(matches!(browser.select("#name", "x").await, Err(Error::Unusable { .. })));
    let log = browser.eval::<String>("JSON.stringify(window.log)").await.unwrap();
    assert!(log.contains("change:country"), "no change on select: {log}");

    browser.set_checkbox("I agree", false).await.unwrap();
    assert!(!browser.eval::<bool>("document.getElementById('agree').checked").await.unwrap());
    browser.set_checkbox("#agree", true).await.unwrap();
    assert!(browser.eval::<bool>("document.getElementById('agree').checked").await.unwrap());
    let log = browser.eval::<String>("JSON.stringify(window.log)").await.unwrap();
    assert!(log.contains("change:agree"), "no change on checkbox: {log}");

    // Enter in a focused field submits the form.
    browser.fill("#name", "Ada").await.unwrap();
    browser.press("Enter").await.unwrap();
    let res = browser.read_text(Some("#res")).await.unwrap();
    assert!(res.starts_with("sent ") && res.contains("name=Ada") && res.contains("country=us"), "{res}");

    // Wait for something that arrives after 300 ms, then for something that never does.
    assert!(matches!(browser.read_text(Some("#late")).await, Err(Error::NotFound(_))));
    browser.wait(Wait::Selector("#late", 3_000)).await.unwrap();
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
    first.eval::<String>("(localStorage.setItem('seen', 'yes'), 'ok')").await.unwrap();
    first.close().await.unwrap();
    assert!(profile.path().join("p").is_dir(), "persistent profile must survive close");

    let second = Browser::launch(opts()).await.unwrap();
    second.navigate(&url).await.unwrap();
    assert_eq!(second.eval::<String>("localStorage.getItem('seen') || 'none'").await.unwrap(), "yes");
    second.close().await.unwrap();
}
