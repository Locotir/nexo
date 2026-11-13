use super::*;

fn read(id: &str, path: &str, from: usize, lines: &[&str]) -> [Message; 2] {
    let text: String =
        lines.iter().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", from + i)).collect::<Vec<_>>().concat();
    [
        Message::new(Role::Assistant, vec![Part::tool_call(id, "read", json!({"path": path}))]),
        Message::tool_results(vec![Part::tool_result(id, text, false)]),
    ]
}

fn bash(id: &str, cmd: &str, out: &str, err: bool) -> [Message; 2] {
    [
        Message::new(Role::Assistant, vec![Part::tool_call(id, "bash", json!({"command": cmd}))]),
        Message::tool_results(vec![Part::tool_result(id, out, err)]),
    ]
}

/// A recorded session: the person asks, the agent reads two files, runs the
/// tests (they fail), edits, plans, then the person moves on.
fn session(dir: &Path) -> (Vec<String>, Vec<String>, Vec<Message>) {
    let parser: Vec<String> =
        (0..60).map(|i| format!("fn parse_line_{i}(s: &str) -> Token {{ lex(s, {i}) }}")).collect();
    let config: Vec<String> = (0..40).map(|i| format!("pub retry_limit_{i}: u32,")).collect();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/parser.rs"), parser.join("\n")).unwrap();
    std::fs::write(dir.join("src/config.rs"), config.join("\n")).unwrap();
    let p: Vec<&str> = parser.iter().map(String::as_str).collect();
    let c: Vec<&str> = config.iter().map(String::as_str).collect();
    let mut h = vec![Message::user(
        "Look at the parser in src/parser.rs and the config in src/config.rs. Never touch Cargo.lock.",
    )];
    h.extend(read("r1", "src/parser.rs", 1, &p));
    h.extend(read("r2", "src/config.rs", 1, &c));
    h.extend(bash(
        "b1",
        "cargo test parser",
        "running 3 tests\ntest parser::tests::parse_line_7 ... FAILED\nassertion failed: lex(s, 7) == Token::Num\n\n[exit code 101]",
        true,
    ));
    h.extend(bash("b2", "cargo build", "error[E0425]: cannot find value `x`\n\n[exit code 1]", true));
    h.extend(bash("b3", "cargo build", "Finished", false));
    h.push(Message::new(
        Role::Assistant,
        vec![
            Part::text("parse_line_7 fails: lex returns the wrong token."),
            Part::tool_call("t1", "todo", json!({"items": [{"text": "fix parse_line_7", "status": "in_progress"}, {"text": "run the tests", "status": "pending"}]})),
        ],
    ));
    h.push(Message::tool_results(vec![Part::tool_result("t1", "ok", false)]));
    h.push(Message::user("Now something else: summarize the config."));
    h.push(Message::assistant("The config holds forty retry limits."));
    (parser, config, h)
}

#[test]
fn lexical_picks_the_passages_of_the_task_within_its_budget() {
    let d = tempfile::tempdir().unwrap();
    let (_, _, h) = session(d.path());
    let spans = lexical("Fix parse_line_7 in src/parser.rs so the parser test passes", &h, 600);
    let all: String = spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
    assert!(all.contains("fn parse_line_7("), "{all}");
    assert!(all.contains("FAILED"), "{all}");
    assert!(!all.contains("retry_limit_"), "{all}");
    assert!(spans.iter().map(|s| s.tokens).sum::<usize>() <= 600);
    let order: Vec<usize> = spans.iter().map(|s| s.msg).collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(order, sorted);
    // a long read keeps the lines around the task's words
    let read = spans.iter().find(|s| s.label == "read src/parser.rs").unwrap();
    assert!(read.text.lines().count() < 15, "{}", read.text);
    // deterministic
    assert_eq!(spans, lexical("Fix parse_line_7 in src/parser.rs so the parser test passes", &h, 600));
    assert!(lexical("zzz qqq", &h, 600).is_empty());
}

#[test]
fn recall_brings_the_notes_and_what_bears_on_the_focus_within_its_budget() {
    let d = tempfile::tempdir().unwrap();
    let (_, _, mut h) = session(d.path());
    h.push(Message::assistant("Finding: parse_line_7 skips the last field.\nnot a note"));
    h.push(Message::assistant("Finding: parse_line_7 skips the last field."));
    let (text, n) = recall("s-1", "the \"parser\" fix", "parse_line_7 in src/parser.rs", &h, d.path(), 1200).unwrap();
    assert!(text.starts_with("<memory from=\"s-1\" title=\"the  parser  fix\">"), "{text}");
    assert_eq!(text.matches("Finding: parse_line_7").count(), 1, "one copy of a repeated note: {text}");
    let notes = text.split("Notes its agent wrote:").nth(1).unwrap_or("").split("\n\n").next().unwrap_or("");
    assert!(notes.lines().take_while(|l| l.starts_with("- ")).all(|l| !l.contains("not a note")), "{text}");
    assert!(text.contains("fn parse_line_7("), "{text}");
    assert!(!text.contains("retry_limit_"), "what does not bear on the focus stays out: {text}");
    assert!(n <= 1300 && text.ends_with("</memory>"));
    // no focus: the title leads
    assert!(recall("s-1", "parser", "", &h, d.path(), 1200).is_some());
    assert!(recall("s-1", "t", "x", &[], d.path(), 1200).is_none());
}

#[test]
fn lexical_leaves_out_reads_a_later_write_made_stale() {
    let d = tempfile::tempdir().unwrap();
    let (_, _, mut h) = session(d.path());
    h.truncate(3);
    h.push(Message::new(
        Role::Assistant,
        vec![Part::tool_call("w1", "write", json!({"path": "src/parser.rs", "content": "fn new_parser() {}"}))],
    ));
    h.push(Message::tool_results(vec![Part::tool_result("w1", "wrote src/parser.rs", false)]));
    let spans = lexical("Fix parse_line_7 in src/parser.rs", &h, 2000);
    assert!(!spans.iter().any(|s| s.text.contains("fn parse_line_7(")), "{spans:?}");
}

#[test]
fn a_failed_edit_does_not_make_a_read_stale() {
    let d = tempfile::tempdir().unwrap();
    let (_, _, mut h) = session(d.path());
    h.truncate(3);
    let edit = |ok: bool| {
        vec![
            Message::new(
                Role::Assistant,
                vec![Part::tool_call(
                    "e1",
                    "edit",
                    json!({"path": "src/parser.rs", "old_string": "fn parse_line_7", "new_string": "fn parse_line_seven_new"}),
                )],
            ),
            Message::tool_results(vec![Part::tool_result(
                "e1",
                if ok { "edited src/parser.rs" } else { "edit: `old_string` not found in src/parser.rs" },
                !ok,
            )]),
        ]
    };
    let task = "Fix parse_line_7 in src/parser.rs";
    let mut failed = h.clone();
    failed.extend(edit(false));
    let mut done = h;
    done.extend(edit(true));
    let has_read = |spans: &[Span]| spans.iter().any(|s| s.text.contains("fn parse_line_7("));
    assert!(has_read(&lexical(task, &failed, 2000)));
    assert!(!has_read(&lexical(task, &done, 2000)));
    assert!(has_read(&curator(task, &failed, 2000)));
    assert!(!has_read(&curator(task, &done, 2000)));
    let l = Ledger::of(&failed, d.path(), true, task);
    assert!(l.files.iter().any(|f| f == "- src/parser.rs: read lines 1-60 (current)"), "{:?}", l.files);
    let l = Ledger::of(&done, d.path(), true, task);
    assert!(l.files.iter().any(|f| f.contains("(changed after)") && f.contains("edited 1x")), "{:?}", l.files);
}

#[test]
fn a_read_stub_is_never_an_excerpt() {
    let d = tempfile::tempdir().unwrap();
    let (parser, _, mut h) = session(d.path());
    h.truncate(3);
    let stub = "src/parser.rs: unchanged since shown above (60 lines); not repeated: lines 1-60 are inside your \
                earlier output of lines 1-60 above. If that is no longer in the conversation, ask again and you \
                get it in full. parse_line_7";
    assert!(crate::reads::is_unchanged_note(stub));
    h.extend([
        Message::new(Role::Assistant, vec![Part::tool_call("r9", "read", json!({"path": "src/parser.rs"}))]),
        Message::tool_results(vec![Part::tool_result("r9", stub, false)]),
    ]);
    let task = "Fix parse_line_7 in src/parser.rs";
    let line = parser[7].as_str();
    let join = |spans: &[Span]| spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
    for (name, spans) in [("lexical", lexical(task, &h, 2000)), ("curator", curator(task, &h, 2000))] {
        let all = join(&spans);
        assert!(!all.contains("unchanged since shown above"), "{name}: {all}");
        assert!(all.contains(line), "{name}: {all}");
    }
    let l = Ledger::of(&h, d.path(), true, task);
    assert!(!format!("{l:?}").contains("unchanged since"), "{l:?}");
    assert!(l.files.iter().any(|f| f.starts_with("- src/parser.rs: read lines 1-60")), "{:?}", l.files);
}

#[test]
fn a_read_of_several_files_is_listed_in_the_ledger_and_not_made_stale_by_an_edit_of_one() {
    let d = tempfile::tempdir().unwrap();
    let h = vec![
        Message::user("Compare parse_line_7 in src/a.rs with src/b.rs"),
        Message::new(Role::Assistant, vec![Part::tool_call("m", "read", json!({"paths": ["src/a.rs", "src/b.rs"]}))]),
        Message::tool_results(vec![Part::tool_result(
            "m",
            "=== src/a.rs ===\n     1│ fn parse_line_7() {}\n=== src/b.rs ===\n     1│ fn other_b() {}\n",
            false,
        )]),
        Message::new(
            Role::Assistant,
            vec![Part::tool_call("e", "edit", json!({"path": "src/a.rs", "old_string": "x", "new_string": "y"}))],
        ),
        Message::tool_results(vec![Part::tool_result("e", "edited src/a.rs", false)]),
    ];
    let l = Ledger::of(&h, d.path(), false, "Compare parse_line_7 in src/a.rs with src/b.rs");
    assert!(l.files.iter().any(|f| f.starts_with("- src/b.rs: read")), "{:?}", l.files);
    assert!(
        l.files.iter().any(|f| f.starts_with("- src/a.rs:") && f.contains("read") && f.contains("edited")),
        "{:?}",
        l.files
    );
    // the result is no one file's, so the edit of a.rs does not drop it as stale
    let spans = lexical("Compare parse_line_7 in src/a.rs with src/b.rs", &h, 2000);
    assert!(spans.iter().any(|s| s.text.contains("other_b")), "{spans:?}");
}

#[test]
fn the_ledger_holds_files_failures_constraints_and_the_plan() {
    let d = tempfile::tempdir().unwrap();
    let (_, config, h) = session(d.path());
    // the config changes on disk after it was read
    let mut changed = config.clone();
    changed[3] = "pub retry_limit_3: u64,".into();
    std::fs::write(d.path().join("src/config.rs"), changed.join("\n")).unwrap();
    let l = Ledger::of(&h, d.path(), true, "Fix parse_line_7 in src/parser.rs");
    assert_eq!(l.request.as_deref(), Some("Now something else: summarize the config."));
    assert!(l.constraints.iter().any(|c| c.contains("Never touch Cargo.lock")), "{:?}", l.constraints);
    assert!(l.files.iter().any(|f| f == "- src/parser.rs: read lines 1-60 (current)"), "{:?}", l.files);
    assert!(l.files.iter().any(|f| f == "- src/config.rs: read lines 1-40 (changed on disk since)"), "{:?}", l.files);
    // `cargo build` failed, then passed: only the test is still failing
    assert_eq!(l.failing.len(), 1, "{:?}", l.failing);
    assert!(
        l.failing[0].starts_with("- `cargo test parser` (exit 101)")
            && l.failing[0].contains("parse_line_7 ... FAILED")
    );
    assert_eq!(l.plan, vec!["- [~] fix parse_line_7".to_string(), "- [ ] run the tests".into()]);
    let (text, facts) = l.render(10_000);
    assert_eq!(facts, 1 + 2 + 1 + 2);
    assert!(text.contains("Failing commands:"));
    // a small budget keeps what fits
    let (small, few) = l.render(40);
    assert!(tokens(&small) <= 40 && few < facts, "{small}");
}

#[tokio::test]
async fn a_lexical_brief_keeps_its_budget() {
    let d = tempfile::tempdir().unwrap();
    let (_, _, h) = session(d.path());
    let cfg = BriefConfig { tokens: 800, engine: Engine::Lexical, ..BriefConfig::default() };
    let b = build("Fix parse_line_7 in src/parser.rs", &h, d.path(), &cfg).await.unwrap();
    assert_eq!(b.engine, "lexical");
    assert!(b.tokens <= 800, "{}", b.tokens);
    assert!(b.text.starts_with("<briefing>") && b.text.ends_with("</briefing>"));
    assert!(b.text.contains("--- read src/parser.rs lines "), "{}", b.text);
    assert!(b.text.contains("(current) ---"), "{}", b.text);
    assert!(b.line().starts_with("briefed: ") && b.line().ends_with("· lexical"), "{}", b.line());
    let off = BriefConfig { tokens: 0, ..cfg.clone() };
    assert!(build("Fix parse_line_7", &h, d.path(), &off).await.is_none());
    assert!(build("Fix parse_line_7", &[], d.path(), &cfg).await.is_none());
}

#[tokio::test]
async fn a_brief_keeps_its_budget_with_many_labels_and_text_of_several_bytes_a_letter() {
    let d = tempfile::tempdir().unwrap();
    let mut h = vec![Message::user("Исправь разбор_строки во всех модулях: проверка_значения падает.")];
    for i in 0..24 {
        let lines: Vec<String> = (0..30)
            .map(|k| format!("функция разбор_строки_{k} проверка_значения модуль {i} строка {k} без изменений"))
            .collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = format!("src/очень_длинный_модуль_номер_{i}/подмодуль_разбора/файл_разбора_строки_{i}.rs");
        h.extend(read(&format!("r{i}"), &path, 1, &lines));
    }
    let task = "Исправь разбор_строки_7 и проверка_значения";
    for engine in [Engine::Lexical, Engine::Curator] {
        for budget in [600, 1500, 3000] {
            let cfg = BriefConfig { tokens: budget, engine, check_disk: false };
            let b = build(task, &h, d.path(), &cfg).await.unwrap();
            // at 600 the ledger (request and files) takes what a read with its long label would need
            assert!(b.spans > 0 || budget == 600, "{engine:?} {budget}\n{}", b.text);
            assert!(b.tokens <= budget, "{engine:?} {budget}: {} tokens", b.tokens);
        }
    }
}

#[tokio::test]
async fn a_brief_comes_from_the_curator_by_default() {
    let d = tempfile::tempdir().unwrap();
    let (_, _, h) = session(d.path());
    for engine in [Engine::Auto, Engine::Curator] {
        let cfg = BriefConfig { tokens: 1500, engine, ..BriefConfig::default() };
        let b = build("Fix parse_line_7 in src/parser.rs", &h, d.path(), &cfg).await.unwrap();
        assert_eq!(b.engine, "curator", "{}", b.text);
        assert!(b.tokens <= 1500, "{}", b.tokens);
        assert!(b.text.contains("parse_line_7"), "{}", b.text);
        assert!(!b.text.contains("retry_limit_"), "{}", b.text);
    }
}

#[tokio::test]
async fn the_latest_request_the_ledger_states_is_not_an_excerpt_too() {
    let d = tempfile::tempdir().unwrap();
    let (_, _, mut h) = session(d.path());
    h.push(Message::user("Check that every retry_limit in src/config.rs stays under 10 and report the ones over it."));
    let task = "List each retry_limit of src/config.rs with its value";
    for engine in [Engine::Lexical, Engine::Curator] {
        let cfg = BriefConfig { tokens: 1500, engine, ..BriefConfig::default() };
        let b = build(task, &h, d.path(), &cfg).await.unwrap();
        assert_eq!(b.text.matches("stays under 10").count(), 1, "{engine:?}\n{}", b.text);
        assert!(b.text.contains("The person's latest request: Check that every retry_limit"), "{}", b.text);
        assert!(b.text.contains("retry_limit_"), "{engine:?}: the budget goes to the reads\n{}", b.text);
    }
    // a request too long for the ledger keeps its excerpt: the ledger holds only its start
    let long =
        format!("Check that every retry_limit in src/config.rs stays under 10. {}", "Keep the order. ".repeat(40));
    h.push(Message::user(long));
    let cfg = BriefConfig { tokens: 3000, engine: Engine::Lexical, ..BriefConfig::default() };
    let b = build(task, &h, d.path(), &cfg).await.unwrap();
    assert!(b.text.matches("stays under 10").count() >= 2, "{}", b.text);
}

#[test]
fn the_report_drops_what_the_briefing_holds_and_is_capped() {
    let brief =
        "<briefing>\n--- read src/parser.rs ---\n     8│ fn parse_line_7(s: &str) -> Token { lex(s, 7) }\n</briefing>";
    let report = "The bug is at src/parser.rs:8:\n     8│ fn parse_line_7(s: &str) -> Token { lex(s, 7) }\nIt should call lex_num.";
    let out = compact_report(report, Some(brief), REPORT_TOKENS);
    assert_eq!(out, "The bug is at src/parser.rs:8:\n(… 1 line you already have)\nIt should call lex_num.");
    let long = "a line of the report that goes on\n".repeat(400);
    let cut = compact_report(&long, None, 200);
    assert!(tokens(&cut) <= 220 && cut.ends_with("cut at about 200 tokens)"), "{}", tokens(&cut));
}

#[test]
fn words_split_identifiers_and_keep_names() {
    let t = terms("Fix parseLine in src/parser.rs and retry_limit");
    for w in ["parse", "line", "src/parser.rs", "retry", "limit", "retry_limit", "parseLine"] {
        assert!(t.contains(w), "{w}: {t:?}");
    }
    assert!(!t.contains("and"));
    assert!(names("cfg.epoch_tokens json.dumps v2").contains("epoch_tokens"));
}
