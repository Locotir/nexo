use super::describe::fmt_k;
use super::*;
use nexo_core::config::ContextConfig;
use nexo_core::message::{estimate_tokens, Role};

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

/// A session of reads, some failing, with a new request now and then.
fn read_session(steps: usize) -> Vec<Message> {
    use nexo_core::message::Part;
    let mut h = vec![Message::user("Go through every module, then fix the one that panics.")];
    for i in 0..steps {
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
    h
}

/// The curator of an open session that sent one view per model reply up to message `upto`.
async fn ran_until(h: &[Message], upto: usize) -> BuiltIn {
    let mut open = BuiltIn::new("").unwrap();
    for p in 0..upto {
        if h[p].role == Role::Assistant {
            open.view(3000, &h[..p]).await.unwrap();
        }
    }
    open
}

/// A session saved halfway (`ContextPolicy::save`) and resumed later from that state (`restore`), with the replies
/// after it taken one by one: the view is the one the open session sends, and the one a full replay sends.
#[tokio::test]
async fn a_session_resumed_from_its_saved_state_sends_the_open_view() {
    let h = read_session(60);
    let half = (h.len() / 2..h.len()).find(|&p| h[p].role == Role::Assistant).unwrap();
    let mut open = ran_until(&h, half + 1).await;
    let bytes = open.save().expect("the curator saves its state")();
    assert!(open.save().is_none(), "nothing new to save");
    for p in half + 1..h.len() {
        if h[p].role == Role::Assistant {
            open.view(3000, &h[..p]).await.unwrap();
        }
    }
    let open = open.view(3000, &h).await.unwrap();
    let mut resumed = BuiltIn::new("").unwrap();
    resumed.restore(bytes);
    let resumed = resumed.view(3000, &h).await.unwrap();
    let replayed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
    assert!(saving(estimate_tokens(&h), estimate_tokens(&open.messages)) > 0, "the open session was cut");
    let sent = |v: &View| serde_json::to_string(&v.messages).unwrap();
    assert_eq!(sent(&resumed), sent(&open));
    assert_eq!(sent(&replayed), sent(&open));
    assert_eq!(resumed.tokens, open.tokens);
}

/// A run that does not know the window (another provider or model than the one that saved the state) starts from
/// the defaults of a new curator with a restored state, as a full replay does, not from the old run's.
#[tokio::test]
async fn a_resumed_run_that_knows_no_window_starts_from_the_default_settings() {
    let h = read_session(60);
    let half = (h.len() / 2..h.len()).find(|&p| h[p].role == Role::Assistant).unwrap();
    let sent = |v: &View| serde_json::to_string(&v.messages).unwrap();
    let unlimited = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
    // the run that saved it had a window the view at the end does not fit
    let mut open = BuiltIn::new("").unwrap();
    open.window(unlimited.tokens / 2 + 3000);
    open.calls_left(Some(2));
    for p in 0..=half {
        if h[p].role == Role::Assistant {
            open.view(3000, &h[..p]).await.unwrap();
        }
    }
    let bytes = open.save().expect("the curator saves its state")();
    let resume = |said: bool| {
        let mut c = BuiltIn::new("").unwrap();
        c.restore(bytes.clone());
        if said {
            c.window(0);
            c.calls_left(None);
        }
        c
    };
    let (silent, said) = (resume(false).view(3000, &h).await.unwrap(), resume(true).view(3000, &h).await.unwrap());
    assert_eq!(sent(&silent), sent(&said), "a run that says there is no window sends what one that says nothing does");
    assert_eq!(silent.tokens, unlimited.tokens, "the old run's window cuts a run that knows none");
}

/// A resume writes the state again only when it took replies again: a state saved after the last reply is as good
/// to start from next time, and writing it over would only cost the write.
#[tokio::test]
async fn a_resume_saves_the_state_again_only_after_taking_replies_again() {
    let h = read_session(40);
    let last = (0..h.len()).rev().find(|&p| h[p].role == Role::Assistant).unwrap();
    let half = (h.len() / 2..h.len()).find(|&p| h[p].role == Role::Assistant).unwrap();
    for (upto, again) in [(last + 1, false), (half + 1, true)] {
        let bytes = ran_until(&h, upto).await.save().unwrap()();
        let mut resumed = BuiltIn::new("").unwrap();
        resumed.restore(bytes);
        resumed.view(3000, &h).await.unwrap();
        assert_eq!(resumed.save().is_some(), again, "saved up to message {upto} of {}", h.len());
    }
}

/// A saved state that does not fit the session any more (cut short, or saved before the history was edited) is
/// not used: the curator is rebuilt from the history, as without one.
#[tokio::test]
async fn a_saved_state_that_does_not_fit_is_ignored() {
    let h = read_session(30);
    let mut open = ran_until(&h, h.len()).await;
    let bytes = open.save().unwrap()();
    let replayed = BuiltIn::new("").unwrap().view(3000, &h).await.unwrap();
    let sent = |v: &View| serde_json::to_string(&v.messages).unwrap();
    let mut edited = h.clone();
    edited[0] = Message::user("Go through every module.");
    let edited_replay = BuiltIn::new("").unwrap().view(3000, &edited).await.unwrap();
    for (bad, hist, want) in [
        (bytes[..bytes.len() / 3].to_vec(), &h, &replayed),
        (b"not a state".to_vec(), &h, &replayed),
        (bytes.clone(), &edited, &edited_replay),
    ] {
        let mut c = BuiltIn::new("").unwrap();
        c.restore(bad);
        assert_eq!(sent(&c.view(3000, hist).await.unwrap()), sent(want));
    }
}

/// The state is written next to the session file and read back from there; a session in the trash takes it along.
#[test]
fn the_saved_state_sits_next_to_the_session_and_moves_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("20261116-080000-ab12.jsonl");
    std::fs::write(&session, "{}\n").unwrap();
    saved::write(&session, Box::new(|| b"state".to_vec()));
    assert_eq!(saved::path_for(&session), dir.path().join("20261116-080000-ab12.jsonl.curator"));
    assert_eq!(saved::read(&session).as_deref(), Some(&b"state"[..]));
    let names: Vec<String> =
        std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(names.len(), 2, "no file of the write is left behind: {names:?}");
    let to = crate::session::trash_in(dir.path(), "20261116-080000-ab12").unwrap();
    assert_eq!(saved::read(&to).as_deref(), Some(&b"state"[..]));
    assert!(saved::read(&session).is_none());
    crate::session::restore_in(dir.path(), "20261116-080000-ab12").unwrap();
    assert_eq!(saved::read(&session).as_deref(), Some(&b"state"[..]));
}

/// A write cut between its file and the rename leaves a file nothing else removes: loading, trashing and
/// forgetting the session remove the ones of its own that no live write can still need, and only those.
#[test]
fn the_files_of_cut_writes_are_removed_for_their_own_session_only() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("20261116-080000-ab12.jsonl");
    let other = dir.path().join("20261116-090000-cd34.jsonl");
    std::fs::write(&session, "{}\n").unwrap();
    // a pid no process has: far above the pid limit of every system
    let dead = |s: &str, n: u32| dir.path().join(format!("{s}.jsonl.curator.2000000000.{n}.tmp"));
    let mine = dir.path().join(format!("20261116-080000-ab12.jsonl.curator.{}.7.tmp", std::process::id()));
    for f in [dead("20261116-080000-ab12", 1), dead("20261116-090000-cd34", 1), mine.clone()] {
        std::fs::write(f, "half").unwrap();
    }
    std::fs::write(dir.path().join("20261116-080000-ab12.jsonl.curator.notes.tmp"), "x").unwrap();
    // 2000000000 is no pid on Unix; elsewhere a process cannot be asked for, so age decides (see below)
    let by_pid = cfg!(unix);
    assert_eq!(saved::sweep(&session), usize::from(by_pid), "the dead process's file of this session goes");
    assert_eq!(!dead("20261116-080000-ab12", 1).exists(), by_pid);
    assert!(dead("20261116-090000-cd34", 1).exists(), "another session's file stays");
    assert!(mine.exists(), "a write of this process may be running");
    assert!(dir.path().join("20261116-080000-ab12.jsonl.curator.notes.tmp").exists(), "other names stay");
    // a file older than the state that replaced it goes whoever wrote it
    let old = dir.path().join(format!("20261116-080000-ab12.jsonl.curator.{}.3.tmp", std::process::id() + 1));
    std::fs::write(&old, "half").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    saved::write(&session, Box::new(|| b"state".to_vec()));
    saved::sweep(&session);
    assert!(!old.exists());
    assert!(!dead("20261116-080000-ab12", 1).exists());
    assert_eq!(saved::read(&session).as_deref(), Some(&b"state"[..]));
    // trashing sweeps too, and never another session's files
    std::fs::write(&other, "{}\n").unwrap();
    let to = crate::session::trash_in(dir.path(), "20261116-080000-ab12").unwrap();
    assert!(dead("20261116-090000-cd34", 1).exists());
    assert!(saved::read(&to).is_some());
}

/// Where a turn of `h` ends: the history up to there ends with a model reply and the person writes next, so a
/// session can be closed and resumed at that point.
fn turn_ends(h: &[Message]) -> Vec<usize> {
    (1..h.len())
        .filter(|&e| {
            h[e - 1].role == Role::Assistant
                && h[e].role == Role::User
                && !h[e].content.iter().any(|p| p.is_tool_result())
        })
        .collect()
}

/// A hash of what a view sends.
fn view_hash(v: &View) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut s = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&v.messages).unwrap().hash(&mut s);
    v.tokens.hash(&mut s);
    s.finish()
}

/// What a session resumed from its saved state sends, and what a full replay sends, as the agent runs them.
struct Resumed {
    /// The hashes of the views after the later turn end: the one shown at the resume and the requests after it,
    /// from the state saved at that turn end, from the one saved at the first resume (a nexo that died mid-way),
    /// with no saved state (a full replay), and in the session that was never closed there.
    saved: Vec<u64>,
    crashed: Vec<u64>,
    replayed: Vec<u64>,
    kept: Vec<u64>,
    /// The size of the file saved at the later turn end, and the time to encode it.
    bytes: usize,
    encode: std::time::Duration,
    /// The time from the resume's start to the view it shows, with the saved state and without it.
    resume: (std::time::Duration, std::time::Duration),
}

/// A session first resumed at message `k` with no saved state (the replay, the view shown, the state saved right
/// after them), run with one request per model reply up to message `m` (the first of them sent twice, as a retried
/// request is), closed there and resumed again; then at most `next` requests after `m`. `system_tokens` is the same
/// for every view: a full replay cannot know another.
async fn resumed_twice(h: &[Message], k: usize, m: usize, next: usize) -> Resumed {
    use std::time::Instant;
    let replies = |from: usize, to: usize| (from + 1..to).filter(move |&p| h[p].role == Role::Assistant);
    async fn after(c: &mut BuiltIn, h: &[Message], m: usize, next: usize) -> Vec<u64> {
        let mut out = Vec::new();
        for p in (m + 1..h.len()).filter(|&p| h[p].role == Role::Assistant).take(next) {
            out.push(view_hash(&c.view(3000, &h[..p]).await.unwrap()));
        }
        out
    }
    // the first resume, with no saved state
    let mut open = BuiltIn::new("").unwrap();
    open.view(3000, &h[..k]).await.unwrap();
    let at_k = open.save().expect("a resume that replayed replies saves the state")();
    let mut retried = false;
    for p in replies(k, m) {
        open.view(3000, &h[..p]).await.unwrap();
        if !retried {
            retried = true;
            open.view(3000, &h[..p]).await.unwrap();
        }
    }
    let t = Instant::now();
    let job = open.save().expect("the turns after the resume are saved");
    let at_m = job();
    let encode = t.elapsed();
    let mut kept = vec![view_hash(&open.view(3000, &h[..m]).await.unwrap())];
    kept.extend(after(&mut open, h, m, next).await);
    drop(open);
    // the second resume, from the state saved at its turn end
    let t = Instant::now();
    let mut c = BuiltIn::new("").unwrap();
    c.restore(at_m.clone());
    let mut saved = vec![view_hash(&c.view(3000, &h[..m]).await.unwrap())];
    let with = t.elapsed();
    saved.extend(after(&mut c, h, m, next).await);
    // from the state of the first resume: the turns after it are taken again
    let mut c = BuiltIn::new("").unwrap();
    c.restore(at_k);
    let mut crashed = vec![view_hash(&c.view(3000, &h[..m]).await.unwrap())];
    crashed.extend(after(&mut c, h, m, next).await);
    // no saved state at all
    let t = Instant::now();
    let mut c = BuiltIn::new("").unwrap();
    let mut replayed = vec![view_hash(&c.view(3000, &h[..m]).await.unwrap())];
    let without = t.elapsed();
    replayed.extend(after(&mut c, h, m, next).await);
    Resumed { saved, crashed, replayed, kept, bytes: at_m.len(), encode, resume: (with, without) }
}

/// A session resumed twice, with turns, a retried request and the view each resume shows in between, sends from
/// its saved state what a full replay of its history sends: the views the open session made and a replay never
/// makes leave nothing behind that changes a later view.
#[tokio::test]
async fn a_session_resumed_twice_from_its_saved_state_sends_what_a_full_replay_sends() {
    let h = read_session(120);
    let ends = turn_ends(&h);
    let (k, m) = (ends[ends.len() / 3], ends[2 * ends.len() / 3]);
    let r = resumed_twice(&h, k, m, usize::MAX).await;
    assert!(r.replayed.len() > 20, "{} views after the resume", r.replayed.len());
    assert_eq!(r.saved, r.replayed, "resumed from the state saved at the turn end");
    assert_eq!(r.crashed, r.replayed, "resumed from the state saved at the first resume");
    assert_eq!(r.kept, r.replayed, "never closed");
}

/// The check above on a large session file, to run by hand with
/// `NEXO_RESUME_CHECK=<session.jsonl> cargo test -p nexo-agent --lib -- --ignored --nocapture a_large_session`
/// (`NEXO_RESUME_CHECK_AT=50,75`: the two resumes, in percent of the history; `NEXO_RESUME_CHECK_NEXT`: the
/// requests compared after the second, 400 by default). It prints the size of the saved state and the times.
/// The saved state always sends what the session never closed sends. A full replay sends the same only while it
/// takes every reply one by one: past `REPLAY` replies it takes the older ones in one step, and its decisions on
/// them are not the ones the open session took.
#[tokio::test]
#[ignore = "minutes on a large session; reads the session file NEXO_RESUME_CHECK names"]
async fn a_large_session_resumed_from_its_saved_state_sends_what_a_full_replay_sends() {
    let Ok(path) = std::env::var("NEXO_RESUME_CHECK") else { return };
    let h = crate::session::Session::open_path(std::path::Path::new(&path)).unwrap().messages().to_vec();
    let at: Vec<usize> = std::env::var("NEXO_RESUME_CHECK_AT")
        .unwrap_or_else(|_| "50,75".into())
        .split(',')
        .map(|s| s.trim().parse().unwrap())
        .collect();
    let next = std::env::var("NEXO_RESUME_CHECK_NEXT").map_or(400, |s| s.parse().unwrap());
    let ends = turn_ends(&h);
    // the turn end nearest to a share of the history
    let end_at = |pc: usize| *ends.iter().min_by_key(|&&e| (e * 100).abs_diff(h.len() * pc)).unwrap();
    let (k, m) = (end_at(at[0]), end_at(at[1]));
    assert!(k < m, "the two resumes fall on one turn end ({k}): other NEXO_RESUME_CHECK_AT");
    let replies = |to: usize| (1..to).filter(|&p| h[p].role == Role::Assistant).count();
    let r = resumed_twice(&h, k, m, next).await;
    let differ = |a: &[u64]| a.iter().zip(&r.kept).position(|(x, y)| x != y);
    eprintln!(
        "messages {} resumes at {k} and {m} ({} and {} replies, {} replayed at most) | saved state {} bytes, \
         encoded in {} ms | resume with it {} ms, without {} ms | views compared {} | first that differs from the \
         session never closed: saved {:?}, crashed {:?}, full replay {:?}",
        h.len(),
        replies(k),
        replies(m),
        super::policies::REPLAY,
        r.bytes,
        r.encode.as_millis(),
        r.resume.0.as_millis(),
        r.resume.1.as_millis(),
        r.replayed.len(),
        differ(&r.saved),
        differ(&r.crashed),
        differ(&r.replayed),
    );
    assert_eq!(r.saved, r.kept, "resumed from the state saved at the turn end");
    assert_eq!(r.crashed, r.kept, "resumed from the state saved at the first resume");
    if replies(m) <= super::policies::REPLAY {
        assert_eq!(r.replayed, r.kept, "a full replay that took every reply");
    }
}

/// The `context` lines of a session file, as JSON.
fn context_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["type"] == "context")
        .collect()
}

fn new_session(dir: &std::path::Path) -> crate::session::Session {
    crate::session::Session::create_in(dir, std::path::Path::new("/p"), "mock", "m1").unwrap()
}

#[test]
fn a_setting_the_curator_cannot_run_is_on_the_record_with_the_spec_that_runs() {
    let d = tempfile::tempdir().unwrap();
    let mut s = new_session(d.path());
    let cfg = ContextConfig {
        policy: " curator ".into(),
        curator_spec: "directives=1;rule=lru".into(),
        ..Default::default()
    };
    let p = from_config(&cfg);
    record(&mut s, p.as_ref(), &cfg.policy);
    let lines = context_lines(s.path());
    assert_eq!(lines.len(), 1, "{lines:?}");
    let l = &lines[0];
    assert_eq!((l["policy"].as_str(), l["asked"].as_str()), (Some("curator"), Some("curator")));
    assert_eq!(l["spec"], "directives=1", "the setting it cannot run is not in the spec that runs");
    assert!(l["caveat"].as_str().unwrap().contains("rule=lru"), "{l}");
}

#[test]
fn plain_has_no_spec_and_no_caveat_and_an_unknown_name_runs_plain_with_one() {
    let d = tempfile::tempdir().unwrap();
    let mut s = new_session(d.path());
    let mut cfg = ContextConfig { policy: "plain".into(), ..Default::default() };
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    let l = context_lines(s.path()).pop().unwrap();
    assert_eq!(
        (l["policy"].as_str(), l["asked"].as_str(), l["spec"].as_str()),
        (Some("plain"), Some("plain"), Some(""))
    );
    assert!(l.get("caveat").is_none(), "{l}");
    cfg.policy = "nonsense".into();
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    let lines = context_lines(s.path());
    assert_eq!(lines.len(), 2);
    let l = &lines[1];
    assert_eq!((l["policy"].as_str(), l["asked"].as_str()), (Some("plain"), Some("nonsense")));
    assert!(l["caveat"].as_str().unwrap().contains("nonsense"), "{l}");
    cfg.policy = "compact".into();
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    let l = context_lines(s.path()).pop().unwrap();
    assert_eq!((l["policy"].as_str(), l["spec"].as_str()), (Some("compact"), Some("")));
}

#[test]
fn the_same_setup_is_written_once_and_a_changed_spec_again() {
    let d = tempfile::tempdir().unwrap();
    let mut s = new_session(d.path());
    let mut cfg = ContextConfig { curator_spec: "directives=1".into(), ..Default::default() };
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    assert_eq!(context_lines(s.path()).len(), 1, "the same setup twice");
    // a resume that sets up the same policy adds nothing, and reads the last line back
    let mut again = crate::session::Session::open_path(s.path()).unwrap();
    assert_eq!(again.context(), s.context());
    record(&mut again, from_config(&cfg).as_ref(), &cfg.policy);
    assert_eq!(context_lines(s.path()).len(), 1, "a resume with the same setup");
    cfg.curator_spec = "directives=1;eoq=1".into();
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    let lines = context_lines(s.path());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["spec"], "directives=1;eoq=1");
    // a rewrite of the file keeps each one, in its place
    s.reset_with(&[Message::user("a")]).unwrap();
    let lines = context_lines(s.path());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["spec"], "directives=1");
    assert_eq!(lines[1]["spec"], "directives=1;eoq=1");
}

/// The `context` and `usage` lines of a session file, in file order, as `context:<policy>:<spec>` and `usage`.
fn context_and_usage_order(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| match v["type"].as_str() {
            Some("context") => Some(format!("context:{}:{}", v["policy"].as_str()?, v["spec"].as_str()?)),
            Some("usage") => Some("usage".to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_rewrite_keeps_each_context_line_before_the_calls_it_ran() {
    use crate::session::{UsageKind, UsageMeta};
    let d = tempfile::tempdir().unwrap();
    let mut s = new_session(d.path());
    let call = nexo_core::stream::Usage { input: 10, output: 1, ..Default::default() };
    let meta = UsageMeta { model: Some("m1".into()), kind: Some(UsageKind::Main), ..Default::default() };
    let mut cfg = ContextConfig { policy: "plain".into(), ..Default::default() };
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    s.record_usage_meta(call, 0.0, meta.clone()).unwrap();
    s.record_usage_meta(call, 0.0, meta.clone()).unwrap();
    cfg = ContextConfig { policy: "curator".into(), curator_spec: "directives=1".into(), ..Default::default() };
    record(&mut s, from_config(&cfg).as_ref(), &cfg.policy);
    s.record_usage_meta(call, 0.0, meta).unwrap();
    s.append(&Message::user("a")).unwrap();
    let want = ["context:plain:", "usage", "usage", "context:curator:directives=1", "usage"];
    assert_eq!(context_and_usage_order(s.path()), want, "as written");
    s.reset_with(&[Message::user("a")]).unwrap();
    assert_eq!(context_and_usage_order(s.path()), want, "after a compaction");
    s.rewind(0).unwrap();
    assert_eq!(context_and_usage_order(s.path()), want, "after a rewind");
    // a session opened again reads the last one back
    let again = crate::session::Session::open_path(s.path()).unwrap();
    assert_eq!(again.context(), s.context());
}
