use super::args::*;
use super::show::*;
use super::*;

fn ctx() -> ToolContext {
    ToolContext::new(std::env::temp_dir())
}

#[test]
fn every_action_parses_from_its_arguments() {
    let cases: Vec<(Value, Action)> = vec![
        (json!({"action": "navigate", "url": "https://a.b"}), Action::Navigate("https://a.b")),
        (json!({"action": "open"}), Action::Open(None, Want::default())),
        (
            json!({"action": "open", "url": "https://a.b", "profile": "Persistent", "visible": true}),
            Action::Open(Some("https://a.b"), Want { profile: Some(BrowserProfile::Persistent), visible: Some(true) }),
        ),
        (
            json!({"action": "open", "profile": "ephemeral", "visible": false}),
            Action::Open(None, Want { profile: Some(BrowserProfile::Ephemeral), visible: Some(false) }),
        ),
        (json!({"action": "snapshot"}), Action::Snapshot),
        (json!({"action": "read_page"}), Action::ReadPage(ReadPageArgs::default())),
        (
            json!({"action": "read_page", "filter": "interactive", "depth": 3, "ref_id": "e12", "max_chars": "900"}),
            Action::ReadPage(ReadPageArgs {
                interactive: true,
                depth: Some(3),
                ref_id: Some("e12"),
                max_chars: Some(900),
            }),
        ),
        (
            json!({"action": "read_page", "filter": "all", "target": "e4"}),
            Action::ReadPage(ReadPageArgs { ref_id: Some("e4"), ..Default::default() }),
        ),
        (json!({"action": "get_page_text"}), Action::ReadText(None)),
        (json!({"action": "read_text"}), Action::ReadText(None)),
        (json!({"action": "read_text", "selector": "main"}), Action::ReadText(Some("main"))),
        (json!({"action": "click", "target": "e3"}), Action::Click("e3")),
        (json!({"action": "fill", "target": "#q", "value": ""}), Action::Fill("#q", "")),
        (
            json!({"action": "fill_batch", "fields": [{"target": "e1", "value": "a"}, {"target": "Email", "value": "b"}]}),
            Action::FillBatch(vec![("e1", "a"), ("Email", "b")]),
        ),
        (json!({"action": "select", "target": "e4", "option": "Spain"}), Action::Select("e4", "Spain")),
        (json!({"action": "set_checkbox", "target": "e5", "checked": false}), Action::SetCheckbox("e5", false)),
        (json!({"action": "press", "keys": "Tab Tab Enter"}), Action::Press("Tab Tab Enter")),
        (json!({"action": "hover", "target": "Menu"}), Action::Hover("Menu")),
        (json!({"action": "scroll", "to": "bottom"}), Action::Scroll("bottom")),
        (json!({"action": "wait", "selector": "e9", "ms": 2000}), Action::Wait(Some("e9"), Some(2000))),
        (json!({"action": "wait", "ms": "300"}), Action::Wait(None, Some(300))),
        (json!({"action": "screenshot", "full": true}), Action::Screenshot(true)),
        (json!({"action": "screenshot"}), Action::Screenshot(false)),
        (json!({"action": "back"}), Action::Back),
        (json!({"action": "forward"}), Action::Forward),
        (json!({"action": "reload"}), Action::Reload),
        (json!({"action": "eval", "value": "document.title"}), Action::Eval("document.title")),
        (json!({"action": "console"}), Action::Console(None)),
        (json!({"action": "console", "selector": "error"}), Action::Console(Some("error"))),
        (json!({"action": "network", "selector": "api"}), Action::Network(Some("api"), None)),
        (json!({"action": "network", "to": "4"}), Action::Network(None, Some(4))),
        (json!({"action": "find", "target": "search box"}), Action::Find("search box")),
        (json!({"action": "double_click", "to": "3,4"}), Action::DoubleClick(3.0, 4.0, Modifiers::default())),
        (
            json!({"action": "right_click", "to": "3,4", "modifiers": "alt"}),
            Action::RightClick(3.0, 4.0, Modifiers::parse("alt").unwrap()),
        ),
        (
            json!({"action": "click_xy", "to": "1,2", "modifiers": "Ctrl+Shift"}),
            Action::ClickXy(1.0, 2.0, Modifiers::parse("ctrl shift").unwrap()),
        ),
        (json!({"action": "hover_xy", "to": "5,6"}), Action::HoverXy(5.0, 6.0)),
        (
            json!({"action": "drag", "to": "10,20,30,-40", "modifiers": "shift"}),
            Action::Drag([10.0, 20.0, 30.0, -40.0], Modifiers::parse("shift").unwrap()),
        ),
        (json!({"action": "drag", "to": "10,20 30,40"}), Action::Drag([10.0, 20.0, 30.0, 40.0], Modifiers::default())),
        (json!({"action": "scroll_xy", "to": "400,300,0,500"}), Action::ScrollXy([400.0, 300.0, 0.0, 500.0])),
        (json!({"action": "tabs"}), Action::Tabs),
        (json!({"action": "tab_new", "url": "https://a.b"}), Action::TabNew("https://a.b")),
        (json!({"action": "tab_switch", "to": 1}), Action::TabSwitch(1)),
        (json!({"action": "tab_close", "to": "2"}), Action::TabClose(2)),
        (json!({"action": "upload", "target": "e7", "value": "cv.pdf"}), Action::Upload("e7", "cv.pdf")),
        (json!({"action": "resize", "to": "390x844"}), Action::Resize(390, 844)),
        (json!({"action": "click_xy", "to": "10, 20.5"}), Action::ClickXy(10.0, 20.5, Modifiers::default())),
        (json!({"action": "type_text", "value": "hi"}), Action::TypeText("hi")),
        (json!({"action": "press", "keys": "Ctrl+A"}), Action::Press("Ctrl+A")),
        (
            json!({"action": "batch", "steps": [{"action": "click", "target": "e1"}, {"action": "tabs"}]}),
            Action::Batch(vec![("click", Action::Click("e1")), ("tabs", Action::Tabs)]),
        ),
        (json!({"action": "close"}), Action::Close),
        (json!({"action": "start_recording"}), Action::Gif(gif::GifCall::Start)),
        (json!({"action": "stop_recording"}), Action::Gif(gif::GifCall::Stop)),
        (json!({"action": "clear_recording"}), Action::Gif(gif::GifCall::Clear)),
        (
            json!({"action": "export_gif", "value": "a.gif", "gif": {"watermark": false}}),
            Action::Gif(gif::GifCall::Export {
                path: Some("a.gif"),
                opts: nexo_browser::ExportOptions { watermark: false, ..Default::default() },
            }),
        ),
        (json!({"action": "capture_start"}), Action::CaptureStart),
        (json!({"action": "capture_stop"}), Action::CaptureStop),
        (json!({"action": "capture_save", "path": "site.har"}), Action::CaptureSave("site.har")),
        (json!({"action": "browsers"}), Action::Browsers),
        (json!({"action": "select_browser", "id": "chrome-1"}), Action::SelectBrowser("chrome-1")),
        (json!({"action": "switch_browser"}), Action::SwitchBrowser),
    ];
    for (args, want) in cases {
        assert_eq!(parse(&args).unwrap(), want, "{args}");
    }
}

#[test]
fn bad_arguments_name_what_is_missing() {
    let err = |v: Value| parse(&v).unwrap_err();
    assert!(err(json!({})).contains("`action` is required"));
    assert!(err(json!({"action": "dance"})).contains("unknown action `dance`"));
    assert!(err(json!({"action": "navigate"})).contains("`url` is required"));
    assert!(err(json!({"action": "click"})).contains("`target` is required"));
    assert!(err(json!({"action": "fill", "target": "e1"})).contains("`value` is required"));
    assert!(err(json!({"action": "fill_batch", "fields": []})).contains("non-empty list"));
    assert!(err(json!({"action": "fill_batch", "fields": [{"target": "e1"}]})).contains("(field 1)"));
    assert!(err(json!({"action": "set_checkbox", "target": "e1"})).contains("`checked`"));
    assert!(err(json!({"action": "wait"})).contains("`selector`"));
    assert!(err(json!({"action": "read_page", "filter": "some"})).contains("`all` or `interactive`"));
    assert!(err(json!({"action": "read_page", "depth": 0})).contains("`depth` must be a whole number"));
    assert!(err(json!({"action": "read_page", "max_chars": "lots"})).contains("`max_chars`"));
    assert!(err(json!({"action": "read_page", "max_chars": 9_999_999})).contains("from 1 to 200000"));
    assert!(err(json!({"action": "eval"})).contains("`value`"));
    assert!(err(json!({"action": "eval", "value": " "})).contains("JavaScript"));
    assert!(err(json!({"action": "tab_switch"})).contains("`to`"));
    assert!(err(json!({"action": "resize", "to": "10"})).contains("WIDTHxHEIGHT"));
    assert!(err(json!({"action": "click_xy", "to": "1,2,3"})).contains("X,Y"));
    assert!(err(json!({"action": "click_xy", "to": "1,2", "modifiers": "hyper"})).contains("unknown modifier"));
    assert!(err(json!({"action": "double_click", "to": "1"})).contains("X,Y"));
    assert!(err(json!({"action": "drag", "to": "1,2,3"})).contains("X1,Y1,X2,Y2"));
    assert!(err(json!({"action": "drag", "to": "1,2,3,NaN"})).contains("X1,Y1,X2,Y2"));
    assert!(err(json!({"action": "scroll_xy", "to": "1,2"})).contains("X,Y,DX,DY"));
    assert!(err(json!({"action": "find"})).contains("`target` is required"));
    assert!(err(json!({"action": "batch"})).contains("`steps`"));
    assert!(err(json!({"action": "batch", "steps": [{"action": "click"}]})).contains("(batch step 1)"));
    assert!(
        err(json!({"action": "batch", "steps": [{"action": "tabs"}, {"action": "close"}]})).contains("step 2 `close`")
    );
    assert!(err(json!({"action": "batch", "steps": [{"action": "batch", "steps": [{"action": "tabs"}]}]}))
        .contains("cannot be part"));
    for read in ["read_page", "read_text", "get_page_text"] {
        let e = err(json!({"action": "batch", "steps": [{"action": "tabs"}, {"action": read}]}));
        assert!(e.contains(&format!("step 2 `{read}`")) && e.contains("on its own"), "{e}");
    }
    assert!(err(json!({"action": "select_browser"})).contains("`id` is required"));
    assert!(err(json!({"action": "capture_save"})).contains("`path` is required"));
    assert!(err(json!({"action": "capture_save", "path": " "})).contains("`path` is required"));
    for a in ["browsers", "switch_browser"] {
        assert!(err(json!({"action": "batch", "steps": [{"action": a}]})).contains("cannot be part"), "{a}");
    }
}

#[test]
fn the_schema_lists_every_action_and_its_parameters() {
    let spec = BrowserTool::new().spec();
    assert_eq!(spec.name, "browser");
    assert_eq!(spec.parameters["type"], "object");
    assert_eq!(spec.parameters["required"], json!(["action"]));
    let enum_: Vec<&str> = spec.parameters["properties"]["action"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(enum_.join(", "), ACTIONS);
    for p in [
        "url",
        "target",
        "value",
        "fields",
        "option",
        "checked",
        "keys",
        "to",
        "selector",
        "ms",
        "full",
        "steps",
        "filter",
        "depth",
        "ref_id",
        "max_chars",
        "profile",
        "visible",
        "id",
    ] {
        assert!(spec.parameters["properties"][p]["description"].is_string(), "{p} is described");
    }
    assert!(spec.description.contains("ref") && spec.description.contains("CSS selector"));
}

#[tokio::test]
async fn bad_calls_and_close_never_start_a_browser() {
    let t = BrowserTool::new();
    let out = t.run(json!({"action": "click"}), &ctx()).await;
    assert!(out.is_error && out.content.contains("`target` is required"));
    let out = t.run(json!({"action": "close"}), &ctx()).await;
    assert!(!out.is_error && out.content.contains("no browser is open"));
    for bad in [json!({"action": "open", "profile": "forever"}), json!({"action": "open", "visible": "maybe"})] {
        let out = t.run(bad, &ctx()).await;
        assert!(out.is_error && out.content.contains("browser open"), "{}", out.content);
    }
    let out = t.run(json!({"action": "batch", "steps": [{"action": "open"}]}), &ctx()).await;
    assert!(out.is_error, "open cannot run inside a batch: {}", out.content);
    assert!(t.browser.lock().await.is_none());
}

/// A page on 127.0.0.1 (cookies and localStorage need a real origin), answering every request the same way.
fn site() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let _ = s.read(&mut [0u8; 2048]);
            let body = "<!doctype html><title>site</title><body>page</body>";
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(reply.as_bytes());
        }
    });
    origin
}

fn tool_in(data: &std::path::Path) -> BrowserTool {
    BrowserTool { data_dir: Some(data.to_path_buf()), ..BrowserTool::default() }
}

/// Every test below passes `visible: false`: a window would open on the desk of whoever runs the tests.
#[tokio::test]
async fn a_kept_profile_keeps_a_login_across_close_and_reopen_and_a_changed_mode_restarts() {
    if nexo_browser::find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let (data, origin) = (tempfile::tempdir().unwrap(), site());
    let t = tool_in(data.path());
    // clean first: the way the model starts by navigating
    let out = t.run(json!({"action": "navigate", "url": origin}), &ctx()).await;
    assert!(!out.is_error && !out.content.contains("browser started"), "{}", out.content);
    // a change of profile restarts it, with the note
    let out =
        t.run(json!({"action": "open", "profile": "persistent", "visible": "false", "url": origin}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains("restarted the browser") && out.content.contains("persistent profile"),
        "{}",
        out.content
    );
    let out = t
        .run(
            json!({"action": "eval", "value": "localStorage.setItem('k', 'kept-1'); document.cookie = 'c=kept-2; max-age=3600'; 1"}),
            &ctx(),
        )
        .await;
    assert!(!out.is_error, "{}", out.content);
    // the same call again is a no-op
    let out = t.run(json!({"action": "open", "profile": "persistent", "visible": false}), &ctx()).await;
    assert!(out.content.contains("already open"), "{}", out.content);
    let out = t.run(json!({"action": "close"}), &ctx()).await;
    assert!(out.content.contains("closed"), "{}", out.content);
    assert!(data.path().join("browser").join("profile").is_dir(), "a kept profile stays after close");

    let out = t.run(json!({"action": "open", "profile": "persistent", "visible": false, "url": origin}), &ctx()).await;
    assert!(!out.is_error && !out.content.contains("restarted"), "{}", out.content);
    let out =
        t.run(json!({"action": "eval", "value": "localStorage.getItem('k') + '|' + document.cookie"}), &ctx()).await;
    assert!(out.content.contains("kept-1|c=kept-2"), "{}", out.content);
    // a visible flag alone changes how it runs, so it restarts with the kept profile's own lock released first
    let out = t.run(json!({"action": "open", "visible": false}), &ctx()).await;
    assert!(out.content.contains("already open"), "{}", out.content);
    t.run(json!({"action": "close"}), &ctx()).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_browser_killed_behind_the_tools_back_comes_back_the_way_it_was() {
    if nexo_browser::find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let t = tool_in(data.path());
    let out = t.run(json!({"action": "open", "profile": "persistent", "visible": false}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    let profile = data.path().join("browser").join("profile");
    let killed = std::process::Command::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg(format!("--user-data-dir={}", profile.display()))
        .status();
    if !killed.is_ok_and(|s| s.success()) {
        eprintln!("skipped: pkill could not stop the browser");
        t.run(json!({"action": "close"}), &ctx()).await;
        return;
    }
    let started = std::time::Instant::now();
    let out = t.run(json!({"action": "snapshot"}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("the browser had been closed"), "{}", out.content);
    assert!(started.elapsed() < std::time::Duration::from_secs(40), "the dead browser must not hang the call");
    let mode = *t.mode.lock().unwrap();
    assert_eq!(
        mode,
        Some(launch::Mode { profile: BrowserProfile::Persistent, headless: true }),
        "it comes back kept and hidden, as it was"
    );
    t.run(json!({"action": "close"}), &ctx()).await;
}

#[tokio::test]
async fn a_failed_open_leaves_the_running_browser_alone() {
    if nexo_browser::find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let (data, origin) = (tempfile::tempdir().unwrap(), site());
    // another browser holds the kept profile
    let holder = nexo_browser::Browser::launch(nexo_browser::LaunchOptions {
        profile: nexo_browser::Profile::Persistent(data.path().join("browser").join("profile")),
        ..Default::default()
    })
    .await
    .unwrap();
    let t = tool_in(data.path());
    let out = t.run(json!({"action": "navigate", "url": origin}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    let out = t.run(json!({"action": "open", "profile": "persistent", "visible": false}), &ctx()).await;
    assert!(out.is_error && out.content.contains("already in use"), "{}", out.content);
    assert!(out.content.contains("untouched"), "{}", out.content);
    // still the clean browser, on its page
    let out = t.run(json!({"action": "read_text"}), &ctx()).await;
    assert!(!out.is_error && out.content.contains("page"), "{}", out.content);
    assert_eq!(*t.mode.lock().unwrap(), Some(launch::Mode { profile: BrowserProfile::Ephemeral, headless: true }));
    t.run(json!({"action": "close"}), &ctx()).await;
    holder.close().await.unwrap();
}

#[tokio::test]
async fn open_starts_a_clean_headless_browser_on_the_address_and_close_ends_it() {
    if nexo_browser::find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let t = BrowserTool::new();
    let out =
        t.run(json!({"action": "open", "url": "data:text/html,<title>Hello</title><button>Go</button>"}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("Go"), "{}", out.content);
    let out = t.run(json!({"action": "open", "profile": "ephemeral", "visible": false}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    let out = t.run(json!({"action": "close"}), &ctx()).await;
    assert!(out.content.contains("closed"), "{}", out.content);
    assert!(t.browser.lock().await.is_none());
}

#[tokio::test]
async fn records_a_session_and_exports_a_gif_with_overlays() {
    if nexo_browser::find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("p.html");
    std::fs::write(
        &page,
        r#"<!doctype html><body style="margin:0"><button style="position:absolute;left:300px;top:200px;width:120px;height:40px" onclick="document.body.style.background='#ddf'">Press me</button>"#,
    )
    .unwrap();
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let t = BrowserTool::new();
    let run = |args: Value| t.run(args, &ctx);

    let out = run(json!({"action": "navigate", "url": format!("file://{}", page.display())})).await;
    assert!(!out.is_error, "{}", out.content);
    // exporting before any recording says what to do
    let out = run(json!({"action": "export_gif"})).await;
    assert!(out.is_error, "{}", out.content);

    let out = run(json!({"action": "start_recording"})).await;
    assert!(!out.is_error && out.content.contains("stop_recording"), "{}", out.content);
    run(json!({"action": "wait", "ms": 250})).await;
    let out = run(json!({"action": "click", "target": "Press me"})).await;
    assert!(!out.is_error, "{}", out.content);
    run(json!({"action": "wait", "ms": 300})).await;
    let out = run(json!({"action": "stop_recording"})).await;
    assert!(!out.is_error && out.content.contains("1 actions logged"), "{}", out.content);

    let out = run(json!({"action": "export_gif", "value": "out/demo.gif", "gif": {"quality": 20}})).await;
    assert!(!out.is_error, "{}", out.content);
    let file = dir.path().join("out").join("demo.gif");
    let bytes = std::fs::read(&file).unwrap();
    assert!(bytes.starts_with(b"GIF8"));
    assert!(
        out.content.contains(&file.display().to_string()) && out.content.contains(&format!("{} bytes", bytes.len())),
        "{}",
        out.content
    );
    // the default name is recording-<n>.gif in the session directory
    let out = run(json!({"action": "export_gif"})).await;
    assert!(!out.is_error && dir.path().join("recording-1.gif").is_file(), "{}", out.content);

    let out = run(json!({"action": "clear_recording"})).await;
    assert!(!out.is_error && out.content.contains("discarded"), "{}", out.content);
    run(json!({"action": "close"})).await;
}

#[test]
fn the_compact_text_names_the_persons_own_browsers() {
    let tool = BrowserTool::default();
    let c = tool.compact().expect("compact");
    assert!(c.description.contains("person's logged-in") && c.description.contains("`browsers`"));
}

#[test]
fn the_lists_are_compact_and_capped() {
    let tabs = vec![
        TabInfo { index: 0, title: "A".into(), url: "https://a.b".into(), active: false },
        TabInfo { index: 1, title: "B".into(), url: "https://c.d".into(), active: true },
    ];
    assert_eq!(show_tabs(&tabs), "0 A  https://a.b\n1* B  https://c.d");
    assert_eq!(show_console(&[]), "(none)");
    let many: Vec<ConsoleEntry> =
        (0..60).map(|i| ConsoleEntry { level: "error".into(), text: format!("boom {i}"), ts: 0.0 }).collect();
    let shown = show_console(&many);
    assert!(shown.starts_with("… 10 earlier entries left out\nerror  boom 10") && shown.ends_with("error  boom 59"));
    assert_eq!(shown.lines().count(), 51);
}

#[test]
fn saving_a_capture_is_a_write_and_the_other_capture_actions_are_not() {
    let t = BrowserTool::new();
    assert_eq!(t.call_kind(&json!({"action": "capture_save", "path": "a.har"})), ToolKind::Write);
    assert_eq!(
        t.call_kind(
            &json!({"action": "batch", "steps": [{"action": "tabs"}, {"action": "capture_save", "path": "a.har"}]})
        ),
        ToolKind::Write
    );
    for a in ["capture_start", "capture_stop", "navigate", "export_gif"] {
        assert_eq!(t.call_kind(&json!({"action": a})), ToolKind::Network, "{a}");
    }
    assert_eq!(t.summary(&json!({"action": "capture_save", "path": "a.har"})), "browser capture_save a.har");
    let spec = t.spec();
    assert!(spec.parameters["properties"]["path"]["description"].is_string());
    assert!(spec.parameters["properties"]["action"]["enum"].to_string().contains("capture_save"));
}

#[test]
fn the_summary_names_the_action_and_its_object() {
    let t = BrowserTool::new();
    assert_eq!(t.summary(&json!({"action": "navigate", "url": "https://a.b"})), "browser navigate https://a.b");
    assert_eq!(t.summary(&json!({"action": "snapshot"})), "browser snapshot");
    assert_eq!(t.summary(&json!({"action": "read_page", "ref_id": "e3"})), "browser read_page e3");
    assert_eq!(t.summary(&json!({"action": "select_browser", "id": "chrome-1"})), "browser select_browser chrome-1");
}

/// The whole path in a real headless Chrome: the full tree on request, the compact snapshot after an action.
#[tokio::test]
async fn read_page_is_opt_in_and_actions_still_answer_with_the_compact_snapshot() {
    if nexo_browser::find_browser().is_none() {
        eprintln!("skipped: no chrome");
        return;
    }
    let html = "<main><h1>Shop</h1><p>Fresh bread daily.</p><button onclick=\"document.title='bought'\">Buy</button>\
                <div style='display:none'>secret offer</div></main><footer>Imprint</footer>";
    let t = BrowserTool::new();
    let c = ctx();
    let out = t.run(json!({"action": "navigate", "url": format!("data:text/html,{html}")}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    // after a page-changing action: the compact list only
    let page = out.content.split_once("Page:\n").expect("the page follows").1;
    assert_eq!(page, "e1 button \"Buy\"", "{}", out.content);

    let out = t.run(json!({"action": "read_page"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    for want in ["heading \"Shop\" level=1", "text \"Fresh bread daily.\"", "button \"Buy\"", "(not visible)"] {
        assert!(out.content.contains(want), "{want}: {}", out.content);
    }
    assert!(!out.content.starts_with("Page:") && !out.content.contains("\n\nPage:"), "read-only: no snapshot after it");
    let buy = out
        .content
        .lines()
        .find(|l| l.contains("button \"Buy\""))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    let out = t.run(json!({"action": "read_page", "filter": "interactive"}), &c).await;
    assert_eq!(out.content, format!("{buy} button \"Buy\""));
    let out = t.run(json!({"action": "read_page", "ref_id": "e9999"}), &c).await;
    assert!(out.is_error && out.content.contains("e9999"), "{}", out.content);

    let out = t.run(json!({"action": "click", "target": buy}), &c).await;
    assert!(!out.is_error && !out.content.contains("heading"), "{}", out.content);
    let out = t.run(json!({"action": "get_page_text"}), &c).await;
    assert!(out.content.contains("Fresh bread daily.") && !out.content.contains("Imprint"), "{}", out.content);
    assert!(out.content.contains("[main content only (main)"), "{}", out.content);
    let out = t.run(json!({"action": "read_text", "selector": "body"}), &c).await;
    assert!(out.content.contains("Imprint") && !out.content.contains("main content only"), "{}", out.content);
    let out = t.run(json!({"action": "close"}), &c).await;
    assert!(!out.is_error);
}

#[test]
fn read_text_says_when_it_read_only_the_main_part() {
    let page = |text: &str, source: &str| PageText { text: text.into(), source: source.into() };
    assert_eq!(show_text(&page("all of it", "body")), "all of it");
    assert_eq!(show_text(&page("part", "selector")), "part");
    assert_eq!(show_text(&page("  ", "article")), "(no text)");
    let main = show_text(&page("story", "article"));
    assert!(main.starts_with("story\n\n[main content only (article)") && main.contains("selector \"body\""), "{main}");
    assert!(show_tree("").contains("nothing on the page") && show_tree("e1 button") == "e1 button");
}

#[test]
fn read_page_args_become_browser_options() {
    let o = ReadPageArgs::default().options();
    assert_eq!(o, ReadPageOptions::default());
    assert_eq!((o.depth, o.max_chars, o.filter), (15, 50_000, Filter::All));
    let o = ReadPageArgs { interactive: true, depth: Some(2), ref_id: Some("e1"), max_chars: Some(10) }.options();
    assert_eq!((o.depth, o.max_chars, o.filter, o.ref_id.as_deref()), (2, 10, Filter::Interactive, Some("e1")));
}

#[test]
fn a_request_is_shown_with_its_headers_timing_and_body() {
    let d = NetDetail { entry: NetEntry::sample(3), body: Some("{\"ok\":true}".into()), body_note: None };
    let text = show_request(&d);
    assert!(text.starts_with("POST https://a.b/api\n201 Created  application/json\n"), "{text}");
    assert!(text.contains("time: 120 ms total, first byte after 80 ms"), "{text}");
    assert!(text.contains("request headers:\n  Accept: */*"), "{text}");
    assert!(text.contains("response headers:\n  Content-Type: application/json"), "{text}");
    assert!(text.ends_with("body:\n{\"ok\":true}"), "{text}");
    let cut = NetDetail { body_note: Some("cut: 8000 of 9000 characters".into()), ..d.clone() };
    assert!(show_request(&cut).contains("body (cut: 8000 of 9000 characters):"));
    let none = NetDetail { body: None, body_note: Some("binary (image/png, about 3 bytes)".into()), ..d };
    assert!(show_request(&none).ends_with("body: binary (image/png, about 3 bytes)"));
    assert_eq!(show_network(&[NetEntry::sample(3)]), "3 POST 201 https://a.b/api");
    assert_eq!(held(Modifiers::parse("shift ctrl").unwrap()), " with ctrl+shift held");
    assert_eq!(held(Modifiers::default()), "");
}

#[test]
fn find_answers_with_lines_or_a_hint() {
    use nexo_browser::Element;
    let el = |r: &str, role: &str, name: &str| Element { reference: r.into(), role: role.into(), name: name.into() };
    let hits = vec![el("e2", "searchbox", "Search"), el("e5", "button", "Search")];
    assert_eq!(show_found("search box", &hits, 2), "e2 searchbox \"Search\"\ne5 button \"Search\"");
    assert!(show_found("search box", &hits, 5).ends_with("… 3 weaker matches; use more specific words"));
    assert!(show_found("nope", &[], 0).starts_with("(nothing on the page matches \"nope\""));
}

#[test]
fn a_fill_report_says_what_did_not_take() {
    let r = FillReport {
        filled: vec!["e1".into()],
        missing: vec!["Phone".into()],
        failed: vec![("e2".into(), "disabled".into())],
    };
    let text = fill_report(&r);
    assert!(
        text.contains("filled 1: e1")
            && text.contains("NOT FOUND (1): Phone")
            && text.contains("FAILED (1): e2 (disabled)")
    );
}
