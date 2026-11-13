//! `read_page` and the article-first `page_text` against local fixtures in a headless Chrome of their own.

use super::*;

const SITE: &str = r##"<!doctype html><html><head><title>Report</title></head><body>
<header><nav aria-label="Main menu"><a href="/home">Home</a> <a href="/about">About us</a></nav></header>
<main>
<h1>Quarterly report</h1>
<p>Intro text with <b>bold</b> words.</p>
<h2>Details</h2>
<ul><li>First item</li><li>Second <a href="/x">linked</a> item</li></ul>
<img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" alt="Revenue chart">
<img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" alt="">
<table aria-label="Numbers"><tr><th>Region</th><th>Sales</th></tr><tr><td>North</td><td>10</td></tr></table>
<form><label for="q">Search</label><input id="q" type="search" placeholder="type here">
<label><input type="checkbox" id="agree" checked> I agree</label>
<select id="c" aria-label="Country"><option>Spain</option><option selected>Chile</option></select>
<button type="submit" disabled>Go</button></form>
<div id="secret" style="display:none"><p>Hidden promo text</p><button>Hidden button</button></div>
<script>document.title = 'x'; var junk = 'SCRIPTBODY';</script>
</main>
<footer>Footer text</footer>
</body></html>"##;

const NESTED: &str = r##"<!doctype html><html><body>
<section aria-label="Level one"><ul><li><a href="#a">Alpha</a><ul><li><a href="#b">Beta</a><ul><li><a href="#c">Gamma</a></li></ul></li></ul></li></ul></section>
</body></html>"##;

fn long(word: &str, n: usize) -> String {
    format!("{word} ").repeat(n)
}

fn article_page() -> String {
    format!(
        r##"<!doctype html><html><body>
<nav><a href="/1">Menu one</a><a href="/2">Menu two</a><a href="/3">Menu three</a></nav>
<div class="chrome">Cookie banner text</div>
<article><h1>The real story</h1><p>{}</p><p>{}</p></article>
<aside>Related links sidebar</aside>
<footer>Copyright footer</footer>
</body></html>"##,
        long("alpha", 40),
        long("bravo", 40)
    )
}

fn write_page(html: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("page.html");
    std::fs::write(&file, html).unwrap();
    let url = format!("file://{}", file.display());
    (dir, url)
}

/// A headless Chrome of its own on a local page; the directory holds the page until the test ends.
async fn open(html: &str) -> Option<(Browser, tempfile::TempDir)> {
    if find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return None;
    }
    let (dir, url) = write_page(html);
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    browser.navigate(&url).await.unwrap();
    Some((browser, dir))
}

fn ref_of(tree: &str, needle: &str) -> String {
    let line = tree.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no line with {needle} in:\n{tree}"));
    let r = line.split_whitespace().next().unwrap();
    assert!(is_ref(r), "line has no ref: {line}");
    r.to_string()
}

#[tokio::test]
async fn full_tree_has_structure_text_and_marks_hidden() {
    let Some((browser, _dir)) = open(SITE).await else { return };
    let tree = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;

    for want in [
        "navigation \"Main menu\"",
        "link \"Home\" href=\"/home\"",
        "main",
        "heading \"Quarterly report\" level=1",
        "heading \"Details\" level=2",
        "text \"Intro text with bold words.\"",
        "list",
        "listitem \"First item\"",
        "link \"linked\" href=\"/x\"",
        "img \"Revenue chart\"",
        "table \"Numbers\"",
        "columnheader \"Region\"",
        "cell \"North\"",
        "searchbox \"Search\" placeholder=\"type here\"",
        "checkbox \"I agree\" checked",
        "combobox \"Country\" value=\"Chile\"",
        "option \"Spain\"",
        "option \"Chile\" selected",
        "button \"Go\" disabled",
        "contentinfo",
        "text \"Footer text\"",
    ] {
        assert!(tree.contains(want), "missing {want:?} in:\n{tree}");
    }
    // the decorative image and the script text are not there; the hidden part is, marked
    assert!(!tree.contains("img \"\""), "{tree}");
    assert!(!tree.contains("SCRIPTBODY"), "{tree}");
    assert!(tree.contains("text \"Hidden promo text\" (not visible)"), "{tree}");
    assert!(tree.contains("button \"Hidden button\" (not visible)"), "{tree}");
    // the label text is the name of its control, not a line of its own
    assert!(!tree.contains("text \"Search\""), "{tree}");
    // levels: the heading sits one level under main
    let main = tree.lines().position(|l| l.split_whitespace().nth(1) == Some("main")).unwrap();
    assert!(tree.lines().nth(main + 1).unwrap().starts_with("  e6 heading"), "{tree}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn interactive_filter_is_flat_and_leaves_out_what_is_not_actionable() {
    let Some((browser, _dir)) = open(SITE).await else { return };
    let opts = ReadPageOptions { filter: Filter::Interactive, ..Default::default() };
    let tree = browser.read_page(&opts).await.unwrap().text;
    assert!(tree.contains("link \"Home\" href=\"/home\""), "{tree}");
    assert!(tree.contains("searchbox \"Search\""), "{tree}");
    assert!(tree.contains("button \"Go\""), "{tree}");
    for gone in ["heading", "text \"", "listitem", "img", "navigation", "Hidden button", "not visible"] {
        assert!(!tree.contains(gone), "{gone:?} should not be in the interactive view:\n{tree}");
    }
    assert!(tree.lines().all(|l| !l.starts_with(' ')), "interactive view is flat:\n{tree}");

    // the compact snapshot that follows every action is this same list
    let snap = browser.snapshot().await.unwrap();
    let compact = snap.render();
    assert_eq!(compact.lines().count(), tree.lines().count());
    for (c, t) in compact.lines().zip(tree.lines()) {
        assert!(t.starts_with(c), "{c:?} vs {t:?}");
    }
    assert!(snap.elements.iter().any(|e| e.role == "link" && e.name == "Home"));
    assert!(!snap.render().contains("heading"));
    browser.close().await.unwrap();
}

#[tokio::test]
async fn refs_from_the_full_tree_are_stable_and_drive_actions() {
    let Some((browser, _dir)) = open(SITE).await else { return };
    let first = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;
    let again = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;
    assert_eq!(first, again, "refs must not change between reads");

    // the interactive snapshot uses the very same refs
    let snap = browser.snapshot().await.unwrap();
    let go = snap.elements.iter().find(|e| e.name == "Home").unwrap();
    assert_eq!(ref_of(&first, "link \"Home\""), go.reference);

    // a ref taken from the full tree fills and clicks
    let q = ref_of(&first, "searchbox \"Search\"");
    browser.fill(&q, "hello").await.unwrap();
    assert_eq!(browser.eval_as::<String>("document.getElementById('q').value").await.unwrap(), "hello");
    let agree = ref_of(&first, "checkbox \"I agree\"");
    browser.set_checkbox(&agree, false).await.unwrap();
    assert!(!browser.eval_as::<bool>("document.getElementById('agree').checked").await.unwrap());
    // and the walk keeps them after the page changed
    let after = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;
    assert_eq!(ref_of(&after, "searchbox \"Search\""), q);
    assert!(after.contains("searchbox \"Search\" value=\"hello\""), "{after}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn ref_id_reads_one_subtree() {
    let Some((browser, _dir)) = open(SITE).await else { return };
    let full = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;
    let table = ref_of(&full, "table \"Numbers\"");
    let sub =
        browser.read_page(&ReadPageOptions { ref_id: Some(table.clone()), ..Default::default() }).await.unwrap().text;
    assert!(sub.starts_with(&format!("{table} table \"Numbers\"")), "{sub}");
    assert!(sub.contains("cell \"North\"") && sub.contains("columnheader \"Sales\""), "{sub}");
    assert!(!sub.contains("Quarterly report") && !sub.contains("Footer"), "{sub}");

    // a leaf element answers with itself; the interactive view of a subtree keeps only what can be acted on
    let nav = ref_of(&full, "navigation \"Main menu\"");
    let opts = ReadPageOptions { ref_id: Some(nav), filter: Filter::Interactive, ..Default::default() };
    let links = browser.read_page(&opts).await.unwrap().text;
    assert_eq!(links.lines().count(), 2, "{links}");

    // unknown refs and non-refs are errors that say so
    let miss = ReadPageOptions { ref_id: Some("e9999".into()), ..Default::default() };
    assert!(matches!(browser.read_page(&miss).await, Err(Error::NotFound(r)) if r == "e9999"));
    let bad = ReadPageOptions { ref_id: Some("#q".into()), ..Default::default() };
    assert!(matches!(browser.read_page(&bad).await, Err(Error::Unusable { .. })));
    browser.close().await.unwrap();
}

#[tokio::test]
async fn depth_limits_the_levels_and_marks_what_is_below() {
    let Some((browser, _dir)) = open(NESTED).await else { return };
    let one = ReadPageOptions { depth: 1, ..Default::default() };
    let tree = browser.read_page(&one).await.unwrap().text;
    assert!(tree.contains("region \"Level one\" (+deeper)"), "{tree}");
    assert!(!tree.contains("Alpha") && !tree.contains("list"), "{tree}");
    assert!(tree.contains("(+deeper) marks content"), "{tree}");

    // levels: region 0, list 1, listitem 2, Alpha 3, the nested list 3, its listitem 4, Beta 5
    let four = browser.read_page(&ReadPageOptions { depth: 4, ..Default::default() }).await.unwrap().text;
    assert!(four.contains("listitem") && four.contains("link \"Alpha\""), "{four}");
    assert!(!four.contains("Beta") && !four.contains("Gamma"), "{four}");

    let all = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;
    assert!(all.contains("link \"Gamma\"") && !all.contains("(+deeper)"), "{all}");
    // the deepest link is indented one level more than the one above it
    let indent = |needle: &str| {
        all.lines().find(|l| l.contains(needle)).unwrap().len()
            - all.lines().find(|l| l.contains(needle)).unwrap().trim_start().len()
    };
    assert!(indent("Gamma") > indent("Beta") && indent("Beta") > indent("Alpha"), "{all}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn max_chars_cuts_at_a_line_and_says_how_to_narrow() {
    let items: String = (0..300).map(|i| format!("<li>Item number {i} of the list</li>")).collect();
    let Some((browser, _dir)) =
        open(&format!("<!doctype html><body><h1>Big</h1><ul>{items}</ul><button>Last</button></body>")).await
    else {
        return;
    };
    let whole = browser.read_page(&ReadPageOptions { max_chars: 1_000_000, ..Default::default() }).await.unwrap();
    assert!(!whole.truncated && whole.total_chars > 5_000);
    let cut = browser.read_page(&ReadPageOptions { max_chars: 2_000, ..Default::default() }).await.unwrap();
    assert!(cut.truncated);
    assert_eq!(cut.total_chars, whole.total_chars);
    let (body, note) = cut.text.split_once("\n... cut after").expect("a note after the cut");
    assert!(body.chars().count() <= 2_000, "{}", body.chars().count());
    // every kept line is a whole line of the full tree
    for l in body.lines() {
        assert!(whole.text.lines().any(|w| w == l), "cut mid-line: {l:?}");
    }
    assert!(note.contains(&format!("of {} characters", whole.total_chars)), "{note}");
    assert!(note.contains("filter=interactive") && note.contains("ref_id") && note.contains("max_chars"), "{note}");
    assert!(!cut.text.contains("button \"Last\""));
    browser.close().await.unwrap();
}

#[tokio::test]
async fn page_text_prefers_the_article_over_navigation_and_footer() {
    let Some((browser, _dir)) = open(&article_page()).await else { return };
    let got = browser.page_text(None).await.unwrap();
    assert_eq!(got.source, "article");
    assert!(got.text.contains("The real story") && got.text.contains("alpha alpha") && got.text.contains("bravo"));
    for noise in ["Menu one", "Cookie banner", "Related links", "Copyright footer"] {
        assert!(!got.text.contains(noise), "{noise} leaked into: {}", got.text);
    }
    // read_text answers the same; a selector still reads exactly that element; body gives everything
    assert_eq!(browser.read_text(None).await.unwrap(), got.text);
    assert!(browser.read_text(Some("nav")).await.unwrap().contains("Menu one"));
    let body = browser.page_text(Some("body")).await.unwrap();
    assert_eq!(body.source, "selector");
    assert!(body.text.contains("Copyright footer") && body.text.contains("Menu two"));
    assert!(matches!(browser.read_text(Some("#nope")).await, Err(Error::NotFound(_))));
    assert!(matches!(browser.read_text(Some("###")).await, Err(Error::NotFound(_))));
    browser.close().await.unwrap();
}

/// The main text of a page, in a browser that is closed again before the next case.
async fn main_text(html: &str) -> Option<PageText> {
    let (browser, _hold) = open(html).await?;
    let got = browser.page_text(None).await.unwrap();
    browser.close().await.unwrap();
    Some(got)
}

#[tokio::test]
async fn page_text_falls_back_to_main_the_densest_block_and_the_body() {
    let main = format!(
        "<!doctype html><body><nav>Top menu</nav><div role='main'><p>{}</p></div><div>Side note</div></body>",
        long("charlie", 60)
    );
    let Some(got) = main_text(&main).await else { return };
    assert_eq!(got.source, "main");
    assert!(got.text.contains("charlie") && !got.text.contains("Top menu"), "{}", got.text);

    // no article, no main: the block holding the paragraphs wins over the link-heavy parts
    let blog = format!(
        "<!doctype html><body><div id='links'>{}</div><div id='post' class='entry'><p>{}</p><p>{}</p></div><div>Footer line</div></body>",
        (0..20).map(|i| format!("<a href='/{i}'>link number {i}</a> ")).collect::<String>(),
        long("delta, echo", 30),
        long("foxtrot, golf", 30)
    );
    let got = main_text(&blog).await.unwrap();
    assert!(got.source.starts_with("densest block") && got.source.contains("div#post"), "{}", got.source);
    assert!(got.text.contains("delta") && !got.text.contains("link number") && !got.text.contains("Footer line"));

    // a short page: the whole body
    let got = main_text("<!doctype html><body><h1>Hi</h1><p>Short.</p></body>").await.unwrap();
    assert_eq!(got.source, "body");
    assert!(got.text.contains("Hi") && got.text.contains("Short."));
}

#[tokio::test]
async fn an_empty_page_reads_as_empty() {
    let Some((browser, _dir)) = open("<!doctype html><body></body>").await else { return };
    let tree = browser.read_page(&ReadPageOptions::default()).await.unwrap();
    assert_eq!(tree.text, "");
    assert!(browser.snapshot().await.unwrap().elements.is_empty());
    browser.close().await.unwrap();
}

#[test]
fn render_cuts_on_a_line_boundary_and_notes_the_size() {
    let node = |d: usize, role: &str, name: &str| snapshot::Node {
        depth: d,
        reference: None,
        role: role.into(),
        name: name.into(),
        attrs: vec![],
        hidden: false,
        more: false,
        noref: false,
    };
    let reply = snapshot::TreeReply {
        found: true,
        nodes: (0..50).map(|i| node(i % 3, "text", &format!("line {i} {}", "x".repeat(40)))).collect(),
        capped: false,
    };
    let opts = ReadPageOptions { max_chars: 300, ..Default::default() };
    let out = snapshot::render_tree(&reply, &opts);
    assert!(out.truncated);
    let (body, note) = out.text.split_once("\n... cut after").unwrap();
    assert!(body.chars().count() <= 300 && body.lines().count() >= 4);
    assert!(note.contains("of 50 lines") && note.contains(&format!("of {} characters", out.total_chars)));
    // under the budget nothing is cut and there is no note
    let big = snapshot::render_tree(&reply, &ReadPageOptions::default());
    assert!(!big.truncated && !big.text.contains("cut after"));
    assert_eq!(big.text.lines().count(), 50);
    // a name with quotes is escaped like in the snapshot
    let mut q = node(0, "button", "say \"hi\"");
    q.reference = Some("e1".into());
    q.attrs = vec!["disabled".into()];
    q.hidden = true;
    let one = snapshot::render_tree(&snapshot::TreeReply { found: true, nodes: vec![q], capped: false }, &opts);
    assert_eq!(one.text, "e1 button \"say \\\"hi\\\"\" disabled (not visible)");
}

#[tokio::test]
async fn hidden_form_fields_are_never_listed_and_passwords_are_masked() {
    let page = r##"<!doctype html><body><form><input type="hidden" name="csrf" value="SECRET-TOKEN-123">
<label for="pw">Password</label><input id="pw" type="password" value="hunter2"><button>Go</button></form></body>"##;
    let Some((browser, _dir)) = open(page).await else { return };
    for filter in [Filter::All, Filter::Interactive] {
        let tree = browser.read_page(&ReadPageOptions { filter, ..Default::default() }).await.unwrap().text;
        assert!(!tree.contains("SECRET-TOKEN") && !tree.contains("hidden"), "{tree}");
        assert!(!tree.contains("hunter2") && tree.contains("textbox \"Password\" value=\"•••\""), "{tree}");
    }
    browser.close().await.unwrap();
}

#[tokio::test]
async fn aria_hidden_parts_keep_their_visible_controls_but_not_their_text() {
    let page = r##"<!doctype html><body><h1>Shop</h1>
<div aria-hidden="true"><p>Decorative words</p><ul><li>Skipped item</li></ul><button>Behind the modal</button></div>
<button>Front</button></body>"##;
    let Some((browser, _dir)) = open(page).await else { return };
    let all = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;
    assert!(all.contains("heading \"Shop\"") && all.contains("button \"Front\""), "{all}");
    assert!(all.contains("button \"Behind the modal\""), "a visible control keeps its ref:\n{all}");
    assert!(!all.contains("Decorative") && !all.contains("Skipped") && !all.contains("listitem"), "{all}");
    // the compact snapshot lists both buttons, as it did before the tree walker
    let snap = browser.snapshot().await.unwrap();
    let names: Vec<&str> = snap.elements.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Behind the modal", "Front"]);
    browser.close().await.unwrap();
}

#[tokio::test]
async fn depth_in_the_interactive_view_does_not_count_the_structure_left_out() {
    let page =
        r##"<!doctype html><body><nav><ul><li><ul><li><a href="/deep">Deep link</a></li></ul></li></ul></nav></body>"##;
    let Some((browser, _dir)) = open(page).await else { return };
    let opts = ReadPageOptions { filter: Filter::Interactive, depth: 1, ..Default::default() };
    let tree = browser.read_page(&opts).await.unwrap().text;
    assert!(tree.contains("link \"Deep link\"") && !tree.contains("deeper"), "{tree}");
    // the full tree does count them: navigation 0, list 1, listitem 2, list 3, listitem 4, link 5
    let all = browser.read_page(&ReadPageOptions { depth: 3, ..Default::default() }).await.unwrap().text;
    assert!(!all.contains("Deep link") && all.contains("(+deeper)"), "{all}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn open_shadow_roots_are_read_but_their_elements_have_no_ref() {
    let page = r##"<!doctype html><body><h1>Light heading</h1><my-card id="host"><span slot="note">Slotted note</span></my-card><my-plain id="plain"></my-plain>
<script>
document.getElementById('plain').attachShadow({ mode: 'open' }).innerHTML = '<span>Plain shadow text</span>';
const root = document.getElementById('host').attachShadow({ mode: 'open' });
root.innerHTML = '<h2>Shadow heading</h2><p>Shadow words</p><button>Shadow buy</button><slot name="note"></slot>';
</script></body>"##;
    let Some((browser, _dir)) = open(page).await else { return };
    let tree = browser.read_page(&ReadPageOptions::default()).await.unwrap().text;
    assert!(tree.contains("heading \"Shadow heading\" level=2 (shadow DOM: no ref)"), "{tree}");
    assert!(tree.contains("button \"Shadow buy\" (shadow DOM: no ref)"), "{tree}");
    assert!(tree.contains("text \"Shadow words\"") && tree.contains("text \"Slotted note\""), "{tree}");
    assert!(tree.contains("text \"Plain shadow text\""), "{tree}");
    // light DOM content keeps its refs; nothing in the shadow root got one
    assert!(tree.lines().any(|l| l.contains("heading \"Light heading\"") && l.trim_start().starts_with('e')), "{tree}");
    for l in tree.lines().filter(|l| l.contains("Shadow buy") || l.contains("Shadow heading")) {
        assert!(!l.trim_start().starts_with('e'), "{l}");
    }
    let snap = browser.snapshot().await.unwrap();
    assert!(snap.elements.iter().all(|e| e.name != "Shadow buy"), "{snap:?}");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn page_text_does_not_narrow_to_one_card_or_a_small_main() {
    // a feed of cards: all of them, not the longest one
    let cards: String = (0..6)
        .map(|i| format!("<article><h2>Card {i}</h2><p>{}</p></article>", long(&format!("word{i}"), 20)))
        .collect();
    let Some(got) =
        main_text(&format!("<!doctype html><body><nav>Menu</nav>{cards}<footer>Foot</footer></body>")).await
    else {
        return;
    };
    assert_eq!(got.source, "articles (6)", "{}", got.text);
    for i in 0..6 {
        assert!(got.text.contains(&format!("Card {i}")), "card {i} missing: {}", got.text);
    }
    assert!(!got.text.contains("Menu") && !got.text.contains("Foot"));

    // one article that holds most of the article text still wins alone (related cards are not mixed in)
    let story = format!(
        "<!doctype html><body><article><h1>Story</h1><p>{}</p></article><article>See also this</article></body>",
        long("lead", 80)
    );
    let got = main_text(&story).await.unwrap();
    assert_eq!(got.source, "article");
    assert!(!got.text.contains("See also"));

    // a small <main> beside much longer content is not the page: the body is read
    let page = format!(
        "<!doctype html><body><main><p>{}</p></main><div class='rest'>{}</div></body>",
        long("tiny", 50),
        format!("<span>{}</span> ", long("other", 100)).repeat(4)
    );
    let got = main_text(&page).await.unwrap();
    assert_eq!(got.source, "body", "{}", got.source);
    assert!(got.text.contains("tiny") && got.text.contains("other"));
}

#[tokio::test]
async fn a_page_with_no_rendered_text_is_empty_not_its_script_source() {
    let page = "<!doctype html><body><script>window.cfg = {\"token\": \"SCRIPT-JSON-SOURCE\"};</script><div id='app'></div></body>";
    let Some(got) = main_text(page).await else { return };
    assert_eq!(got.text, "", "{}", got.text);
    assert_eq!(got.source, "body");
}

#[test]
fn a_first_line_longer_than_the_budget_is_cut_not_dropped() {
    let node = |name: String| snapshot::Node {
        depth: 0,
        reference: Some("e1".into()),
        role: "text".into(),
        name,
        attrs: vec![],
        hidden: false,
        more: false,
        noref: false,
    };
    let reply =
        snapshot::TreeReply { found: true, nodes: vec![node("y".repeat(900)), node("z".into())], capped: false };
    let out = snapshot::render_tree(&reply, &ReadPageOptions { max_chars: 1, ..Default::default() });
    assert!(out.truncated);
    assert!(out.text.starts_with("e1 text \"yyy") && !out.text.starts_with('\n'), "{}", out.text);
    let (body, note) = out.text.split_once("\n... cut after").unwrap();
    assert!(body.ends_with('…') && body.chars().count() <= 200, "{}", body.chars().count());
    assert!(note.contains("1 of 2 lines"), "{note}");
}
