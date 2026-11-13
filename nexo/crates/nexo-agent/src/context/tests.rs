use super::*;

fn history() -> Vec<Message> {
    vec![Message::user("a"), Message::assistant("b"), Message::user("c"), Message::assistant("d"), Message::user("e")]
}

#[test]
fn only_the_curator_adds_its_directives() {
    assert_eq!(Plain::default().directives(&["note"]), None);
    // off by default (a pure view); `directives=1` turns them on
    assert_eq!(BuiltIn::new("").unwrap().directives(&["note"]), None);
    let c = BuiltIn::new("directives=1").unwrap();
    let d = c.directives(&["read", "note"]).unwrap();
    assert!(d.starts_with("[curator] Working efficiently") && d.contains("at most three short lines"));
    assert!(d.contains("beginning with \"Finding:\"") && d.contains("beginning with \"Plan:\""));
    assert!(d.contains("(7)") && d.contains("beginning with \"Rule:\""), "{d}");
    assert!(!d.contains("  "), "the line breaks of the source leave no double spaces");
    // without a `note` tool, the same directives ask for the lines in the reply
    let s = c.directives(&["read", "bash"]).unwrap();
    assert!(s.contains("as the first line of your next reply: one line beginning with \"Finding:\""), "{s}");
    assert!(s.contains("as a line of your reply: one line beginning with \"Rule:\""), "{s}");
    assert!(!s.contains("`note`") && d.contains("`note`"));
    assert!(!s.contains("  "));
    // `directives=0`: the system prompt stays as the other policies send it
    assert_eq!(BuiltIn::new("directives=0").unwrap().directives(&["note"]), None);
}

#[test]
fn the_curator_offers_recall_unless_told_not_to() {
    // only the curator leaves anything out; `recall=0` keeps the tool list of the other policies
    assert!(!Plain::default().recall());
    assert!(BuiltIn::new("").unwrap().recall());
    assert!(BuiltIn::new("directives=0").unwrap().recall());
    assert!(!BuiltIn::new("recall=0").unwrap().recall());
}

#[test]
fn working_set_overrides_the_window_of_the_spec() {
    assert_eq!(working_set_spec("eoq=1", "").unwrap(), "eoq=1");
    assert_eq!(working_set_spec("", "10000/20000/30000").unwrap(), "target=10000;cap=20000;hard=30000");
    assert_eq!(
        working_set_spec("eoq=0;cap=5", "10_000/20000/30000").unwrap(),
        "eoq=0;cap=5;target=10000;cap=20000;hard=30000"
    );
    for bad in ["20000", "1/2", "a/b/c", "1/2/3/4"] {
        assert!(working_set_spec("", bad).is_err(), "{bad}");
    }
    let mut cfg = ContextConfig { working_set: "10000/20000/30000".into(), ..Default::default() };
    assert!(BuiltIn::new(&working_set_spec(&cfg.curator_spec, &cfg.working_set).unwrap()).is_ok());
    // a window that cannot run is said, never silent
    cfg.working_set = "30000/20000/10000".into();
    let spec = working_set_spec(&cfg.curator_spec, &cfg.working_set).unwrap();
    assert!(BuiltIn::new(&spec).is_err());
}

#[test]
fn config_picks_the_policy() {
    let mut cfg = ContextConfig::default();
    assert_eq!(cfg.policy, "auto");
    cfg.policy = "plain".into();
    let p = from_config(&cfg);
    assert_eq!(p.name(), "plain");
    assert!(p.detail().is_none(), "asked for: nothing to explain");
    // auto and curator: the built-in curator
    for name in ["auto", "curator", ""] {
        cfg.policy = name.into();
        let p = from_config(&cfg);
        assert_eq!((p.name(), p.directives(&["note"])), ("curator", None));
        assert!(p.detail().is_none());
    }
    cfg.curator_spec = "directives=1".into();
    let p = from_config(&cfg);
    assert_eq!(p.directives(&["note"]), Some(CURATOR_DIRECTIVES));
    assert_eq!(p.directives(&[]), Some(CURATOR_DIRECTIVES_SAID));
    // a setting the curator cannot run: the curator runs without it, and says which
    cfg.curator_spec = "rule=lru".into();
    let p = from_config(&cfg);
    assert_eq!(p.name(), "curator");
    assert!(p.detail().is_none() && p.caveat().unwrap().contains("rule=lru"));
    cfg.curator_spec.clear();
    cfg.policy = "curatr".into();
    assert!(from_config(&cfg).detail().unwrap().contains("\"curatr\""));
    cfg.policy = "compact".into();
    assert_eq!(from_config(&cfg).compact_at(), Some(100_000));
}

#[tokio::test]
async fn auto_tells_once_why_it_is_plain() {
    let mut p = Plain::because("context: plain, since the curator cannot run this spec".into());
    let v = p.view(0, &history()).await.unwrap();
    assert!(v.note.unwrap().contains("cannot run this spec"));
    assert!(p.view(0, &history()).await.unwrap().note.is_none(), "said once");
    assert!(p.detail().is_some(), "and /context keeps saying it");
}

#[tokio::test]
async fn the_label_says_what_came_in_and_went_out() {
    let mut c = BuiltIn::new("").unwrap();
    c.view(0, &history()).await.unwrap();
    // the first view of a history puts nothing back and takes nothing out: the conversation's own messages are new
    assert_eq!(describe(&c, 100_000, 62_000).0, "curator");
    c.moved = (2, 5);
    // the history grew by 4k and the view fell by 8k: the curator took 12k off the window in use
    let (label, detail) = describe_since(&c, 100_000, 62_000, Some((96_000, 70_000)));
    assert_eq!(label, "curator -12k tok");
    assert!(label.is_ascii(), "{label}");
    assert!(detail.contains("This call:") && detail.contains("brought back"), "{detail}");
    assert!(detail.contains("12k tokens smaller"), "{detail}");
    // what it brought back outweighs what it took out: the window grew by more than the new messages
    assert_eq!(describe_since(&c, 100_000, 62_000, Some((99_000, 58_000))).0, "curator +3.0k tok");
    // nothing to compare with (the first call, a rewound history) or a change too small to name
    assert_eq!(describe(&c, 100_000, 62_000).0, "curator");
    assert_eq!(describe_since(&c, 100_000, 62_000, Some((0, 0))).0, "curator");
    assert_eq!(describe_since(&c, 100_000, 62_000, Some((120_000, 70_000))).0, "curator");
    assert_eq!(describe_since(&c, 100_000, 62_000, Some((99_000, 61_050))).0, "curator");
    c.moved = (0, 0);
    assert_eq!(describe(&c, 100_000, 62_000).0, "curator");
}

#[test]
fn the_label_says_what_the_policy_does() {
    let c = BuiltIn::new("").unwrap();
    let (label, detail) = describe(&c, 100_000, 62_000);
    assert_eq!(label, "curator");
    assert!(detail.contains("62k tokens of the 100k tokens"), "{detail}");
    let (label, detail) = describe(&c, 20_000, 19_900);
    assert_eq!(label, "curator idle");
    assert!(detail.contains("20k tokens"), "{detail}");
    let (label, _) = describe(&Plain::default(), 20_000, 20_000);
    assert_eq!(label, "plain");
    let (label, detail) = describe(&Compact { compact_at: 120_000 }, 1, 1);
    assert_eq!(label, "compact");
    assert!(detail.contains("120k"));
    assert_eq!(saving(0, 0), 0);
    assert_eq!(saving(1000, 980), 0, "squeezing alone is not worth a label");
    assert_eq!(saving(1000, 500), 50);
    assert_eq!(fmt_k(950), "950");
    assert_eq!(fmt_k(1_234), "1.2k");
    assert_eq!(fmt_k(1_500_000), "1.5M");
}

#[test]
fn a_setting_the_curator_cannot_run_leaves_only_that_setting_out() {
    // a typo in one setting keeps the curator and the rest of the spec, and is said once
    let cfg = ContextConfig { curator_spec: "dedup=0;rule=lru;apart=5000".into(), ..Default::default() };
    let p = from_config(&cfg);
    let (label, detail) = describe(p.as_ref(), 1000, 1000);
    assert!(label.starts_with("curator") && label != "curator off", "{label}");
    assert!(detail.contains("without [context] curator_spec rule=lru ("), "{detail}");
    assert!(!detail.contains("dedup=0") && !detail.contains("apart"), "{detail}");
}

#[tokio::test]
async fn the_setting_left_out_is_said_once_in_the_transcript() {
    let mut p = BuiltIn::lenient("rule=lru");
    assert!(p.view(0, &history()).await.unwrap().note.unwrap().contains("rule=lru"));
    assert!(p.view(0, &history()).await.unwrap().note.is_none(), "said once");
    // a spec that runs has nothing to say
    let mut ok = BuiltIn::lenient("dedup=3");
    assert!(ok.caveat().is_none() && ok.view(0, &history()).await.unwrap().note.is_none());
}

/// A user's standing instruction outlives a compaction of the history.
#[tokio::test]
async fn a_compacted_history_keeps_the_rules() {
    let mut c = BuiltIn::new("").unwrap();
    let mut h = vec![Message::user("Never use unwrap in library code."), Message::assistant("Understood.")];
    c.view(1000, &h).await.unwrap();
    h = vec![Message::user("Summary of the work so far: the parser is done."), Message::assistant("Ok.")];
    c.compacted();
    let v = c.view(1000, &h).await.unwrap();
    let text: String = v.messages.iter().map(|m| m.text()).collect();
    assert!(text.contains("Never use unwrap in library code."), "{text}");
}

/// A session closed and resumed: the notes come back from the saved history, nothing else holds them.
#[tokio::test]
async fn notes_come_back_when_a_session_is_resumed() {
    use nexo_core::message::{Part, Role};
    let mut h = vec![
        Message::user("Find where the key is kept."),
        Message::new(
            Role::Assistant,
            vec![Part::tool_call("n0", "note", serde_json::json!({"text": "Finding: the key lives at 0x4f2a"}))],
        ),
        Message::new(Role::User, vec![Part::tool_result("n0", "noted 1 line", false)]),
    ];
    for i in 0..40 {
        h.push(Message::new(
            Role::Assistant,
            vec![Part::tool_call(format!("c{i}"), "read", serde_json::json!({"path": format!("m{i}.rs")}))],
        ));
        let body: String = (0..300).map(|l| format!("{l:>4}  fn f_{i}_{l}() {{}} // module {i}\n")).collect();
        h.push(Message::new(Role::User, vec![Part::tool_result(format!("c{i}"), body, false)]));
    }
    // the open session saw the history grow; the resumed one gets it whole, with a fresh curator
    let mut open = BuiltIn::new("").unwrap();
    for n in (4..=h.len()).step_by(2) {
        open.view(3000, &h[..n]).await.unwrap();
    }
    let open = open.view(3000, &h).await.unwrap();
    let resumed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
    for v in [&open, &resumed] {
        let sent = serde_json::to_string(&v.messages).unwrap();
        assert!(sent.contains("0x4f2a"), "the note is in the view");
        assert!(saving(estimate_tokens(&h), estimate_tokens(&v.messages)) > 0, "and the view was cut");
    }
}

/// A long session, as it was sent while it ran (one view per model reply) and as a resumed
/// session sees it (the whole history at once): the view is the same, cuts and marks alike, so
/// a restart neither changes what the model sees nor misses the provider's cache.
#[tokio::test]
async fn a_resumed_session_sends_the_view_the_open_one_would() {
    use nexo_core::message::{Part, Role};
    let mut h = vec![Message::user("Go through every module, then fix the one that panics.")];
    for i in 0..60 {
        let path = format!("m{}.rs", i % 23);
        h.push(Message::new(
            Role::Assistant,
            vec![
                Part::text(format!("Step {i}.")),
                Part::tool_call(format!("c{i}"), "read", serde_json::json!({"path": path})),
            ],
        ));
        let body: String =
            (0..120 + 7 * (i % 5)).map(|l| format!("{l:>4}  fn f_{i}_{l}(x: u64) -> u64 {{ x + {l} }}\n")).collect();
        h.push(Message::new(Role::User, vec![Part::tool_result(format!("c{i}"), body, i % 11 == 7)]));
        if i % 17 == 16 {
            h.push(Message::assistant(format!("Module batch {i} done.")));
            h.push(Message::user(format!("Now batch {}.", i + 1)));
        }
    }
    let mut open = BuiltIn::new("").unwrap();
    for p in 0..h.len() {
        if h[p].role == Role::Assistant {
            open.view(3000, &h[..p]).await.unwrap();
        }
    }
    let open = open.view(3000, &h).await.unwrap();
    let resumed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
    assert!(saving(estimate_tokens(&h), estimate_tokens(&open.messages)) > 0, "the open session was cut");
    assert_eq!(serde_json::to_string(&resumed.messages).unwrap(), serde_json::to_string(&open.messages).unwrap());
    assert_eq!(resumed.tokens, open.tokens);
}

/// Many generated sessions (reads, rereads, edits, commands, notes, failures, new requests):
/// the view a resumed session sends is the one the open session would have sent.
#[tokio::test]
async fn resumed_sessions_of_every_shape_send_the_open_view() {
    use nexo_core::message::{Part, Role};
    let mut differ = Vec::new();
    for seed in 0..40u64 {
        let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut next = |m: u64| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) % m
        };
        let mut h = vec![Message::user(format!("Task {seed}: look through the project and fix what is broken."))];
        let steps = 30 + next(120);
        for i in 0..steps {
            let f = format!("src/m{}.rs", next(15));
            let (name, args, body): (&str, serde_json::Value, String) = match next(6) {
                0 | 1 => (
                    "read",
                    serde_json::json!({"path": f}),
                    (0..40 + next(300)).map(|l| format!("{l:>4}  let v{l} = f{i}({l});\n")).collect(),
                ),
                2 => (
                    "edit",
                    serde_json::json!({"path": f, "old_string": "a", "new_string": "b"}),
                    format!("edited {f}: +1 -1 lines"),
                ),
                3 => (
                    "bash",
                    serde_json::json!({"command": format!("cargo test m{i}")}),
                    (0..20 + next(200))
                        .map(|l| format!("test t{l} ... {}\n", if l % 9 == 0 { "FAILED" } else { "ok" }))
                        .collect(),
                ),
                4 => (
                    "note",
                    serde_json::json!({"text": format!("Finding {i}: value {} matters", next(9999))}),
                    "noted 1 line".into(),
                ),
                _ => (
                    "grep",
                    serde_json::json!({"pattern": format!("f{}", next(50))}),
                    (0..next(60)).map(|l| format!("{f}:{l}: f{l}()\n")).collect(),
                ),
            };
            h.push(Message::new(
                Role::Assistant,
                vec![Part::text(format!("Step {i}.")), Part::tool_call(format!("c{i}"), name, args)],
            ));
            h.push(Message::new(Role::User, vec![Part::tool_result(format!("c{i}"), body, next(13) == 0)]));
            if next(25) == 0 {
                h.push(Message::assistant(format!("Done with part {i}.")));
                h.push(Message::user(format!("Next: part {}.", i + 1)));
            }
        }
        let mut open = BuiltIn::new("").unwrap();
        for p in 0..h.len() {
            if h[p].role == Role::Assistant {
                open.view(3000, &h[..p]).await.unwrap();
            }
        }
        let open = open.view(3000, &h).await.unwrap();
        let resumed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
        let (a, b) =
            (serde_json::to_string(&open.messages).unwrap(), serde_json::to_string(&resumed.messages).unwrap());
        if a != b {
            differ.push((seed, h.len(), open.tokens, resumed.tokens));
        }
    }
    assert!(differ.is_empty(), "seed, messages, open tokens, resumed tokens: {differ:?}");
}
/// The curator on a long session of large tool results.
#[tokio::test]
async fn the_curator_cuts_a_long_session() {
    let mut c = BuiltIn::new("").unwrap();
    let mut h = vec![Message::user("Go through every module and summarise what each does.")];
    let mut first_cut = None;
    let mut last = (0, 0);
    for i in 0..40 {
        h.push(Message::new(
            nexo_core::message::Role::Assistant,
            vec![
                nexo_core::message::Part::text(format!("Step {i}: reading module {i}.")),
                nexo_core::message::Part::tool_call(
                    format!("c{i}"),
                    "read",
                    serde_json::json!({"path": format!("m{i}.rs")}),
                ),
            ],
        ));
        let body: String = (0..300)
            .map(|l| format!("{l:>4}  fn f_{i}_{l}(x: u64) -> u64 {{ x * {l} + {i} }} // module {i}\n"))
            .collect();
        h.push(Message::new(
            nexo_core::message::Role::User,
            vec![nexo_core::message::Part::tool_result(format!("c{i}"), body, false)],
        ));
        let v = c.view(3000, &h).await.unwrap();
        assert!(v.memory.is_some());
        let (full, sent) = (estimate_tokens(&h), estimate_tokens(&v.messages));
        if first_cut.is_none() && saving(full, sent) > 0 {
            first_cut = Some(i + 1);
        }
        last = (full, sent);
    }
    assert!(first_cut.is_some(), "the curator never cut anything in 40 calls");
    assert!(saving(last.0, last.1) >= 30, "{last:?}");
    // a rewritten history is taken as it is
    h.truncate(3);
    let v = c.view(3000, &h).await.unwrap();
    assert_eq!(v.messages.len(), 3);
}

fn picture() -> nexo_core::message::Part {
    // a 1000 by 800 PNG as `read` returns it; the estimate never looks at the bytes
    let data = "iVBORw0KGgo".repeat(2000);
    nexo_core::message::Part::Image {
        source: nexo_core::message::ImageSource::Base64 { media_type: "image/png".into(), data },
    }
}

fn images_in(m: &Message) -> usize {
    use nexo_core::message::Part;
    fn count(p: &Part) -> usize {
        match p {
            Part::Image { .. } => 1,
            Part::ToolResult { content, .. } => content.iter().map(count).sum(),
            _ => 0,
        }
    }
    m.content.iter().map(count).sum()
}

/// One read step: the model's call and the tool's answer, a screenshot when `shot`, else a long file.
fn read_step(h: &mut Vec<Message>, i: usize, shot: bool) {
    use nexo_core::message::Part;
    h.push(Message::new(
        Role::Assistant,
        vec![
            Part::text(format!("Step {i}: reading module {i}.")),
            Part::tool_call(format!("c{i}"), "read", serde_json::json!({"path": format!("m{i}.rs")})),
        ],
    ));
    let mut content = if shot {
        vec![Part::text("PNG image, 1000x800")]
    } else {
        let body: String = (0..300)
            .map(|l| format!("{l:>4}  fn f_{i}_{l}(x: u64) -> u64 {{ x * {l} + {i} }} // module {i}\n"))
            .collect();
        vec![Part::text(body)]
    };
    if shot {
        content.push(picture());
    }
    let result = Part::ToolResult { call_id: format!("c{i}"), name: None, content, is_error: false };
    h.push(Message::new(Role::User, vec![result]));
}

/// The view's tokens rise by the nominal size of the picture in the newest tool result, and not by its bytes.
#[tokio::test]
async fn the_view_counts_an_image_in_a_tool_result() {
    let tokens = |bytes: Option<usize>| {
        let mut h = vec![Message::user("Look at the screenshot.")];
        read_step(&mut h, 0, true);
        if let nexo_core::message::Part::ToolResult { content, .. } = &mut h[2].content[0] {
            match bytes {
                Some(n) => {
                    content[1] = nexo_core::message::Part::Image {
                        source: nexo_core::message::ImageSource::Base64 {
                            media_type: "image/png".into(),
                            data: "A".repeat(n),
                        },
                    }
                }
                None => drop(content.pop()),
            }
        }
        async move {
            let v = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
            assert_eq!(images_in(&v.messages[2]), usize::from(bytes.is_some()), "the newest turn keeps its picture");
            (v.tokens, estimate_tokens(&v.messages))
        }
    };
    let (bare, bare_est) = tokens(None).await;
    let (small, small_est) = tokens(Some(100)).await;
    let (big, big_est) = tokens(Some(3_000_000)).await;
    // 1500 for the picture, a few tokens for the characters of its block
    assert!((1500..1600).contains(&(small - bare)), "{bare} {small}");
    assert!((1500..1600).contains(&(small_est - bare_est)), "{bare_est} {small_est}");
    assert_eq!((small, small_est), (big, big_est), "the bytes are not counted");
}

/// An old tool result left out of the view takes its picture with it; the newest turn keeps its picture.
#[tokio::test]
async fn a_stubbed_result_has_no_picture_and_the_newest_turn_keeps_its_own() {
    let mut c = BuiltIn::new("").unwrap();
    let mut h = vec![Message::user("Go through every module and summarise what each does.")];
    read_step(&mut h, 0, true);
    for i in 1..40 {
        read_step(&mut h, i, false);
        c.view(3000, &h).await.unwrap();
    }
    let v = c.view(3000, &h).await.unwrap();
    let old = |m: &[Message]| m[..m.len() - 2].iter().map(images_in).sum::<usize>();
    assert_eq!(old(&h), 1);
    assert!(saving(estimate_tokens(&h), estimate_tokens(&v.messages)) > 0, "the curator cut nothing");
    assert_eq!(old(&v.messages), 0, "the old picture is still in the view");
    read_step(&mut h, 40, true);
    let v = c.view(3000, &h).await.unwrap();
    assert_eq!(images_in(v.messages.last().unwrap()), 1, "the newest picture was cut");
    assert_eq!(old(&v.messages), 0);
}
