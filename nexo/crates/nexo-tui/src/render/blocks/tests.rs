use super::*;

#[test]
fn the_goal_lines_become_a_badge_and_never_show_bare() {
    assert_eq!(goal_signal("All set.\n\nGOAL DONE\n", false), ("All set.", Some(GoalSignal::Done)));
    assert_eq!(goal_signal("Waiting on you.\n**GOAL WAIT**", false), ("Waiting on you.", Some(GoalSignal::Wait)));
    assert_eq!(goal_signal("GOAL WAIT", false), ("", Some(GoalSignal::Wait)));
    assert_eq!(goal_signal("text\nGOAL W", true), ("text", None), "held back while it streams");
    assert_eq!(goal_signal("text\nGOAL W", false), ("text\nGOAL W", None), "a finished reply keeps it");
    assert_eq!(goal_signal("the GOAL DONE marker", false), ("the GOAL DONE marker", None));
    let b =
        Block::Assistant { text: "Ok.\n\nGOAL WAIT".into(), streaming: false, at: None, started: None, took_ms: None };
    let shown = texts(&b.render(80, &Theme::nexo(), opts())).join("\n");
    assert!(!shown.contains("GOAL WAIT") && shown.contains("goal paused"), "{shown}");
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(markdown::text_of).collect()
}

#[test]
fn clocks_grow_into_minutes_and_hours() {
    let cases = [
        (0, "0s"),
        (59, "59s"),
        (60, "1m00s"),
        (185, "3m05s"),
        (3599, "59m59s"),
        (3600, "1h00m"),
        (3720, "1h02m"),
        (90_061, "25h01m"),
    ];
    for (s, want) in cases {
        assert_eq!(fmt_secs(s), want, "{s} s");
    }
    assert_eq!(fmt_ms(450), "450ms");
    assert_eq!(fmt_ms(12_340), "12.3s");
    assert_eq!(fmt_ms(185_000), "3m05s");
    assert_eq!(fmt_ms(5_400_000), "1h30m");
}

const CURATOR_NOTICE: &str = "the curator kept the view at 7.4k tokens (history 14.9k)";

/// The notice as the transcript paints it at `cols` columns, one string per row.
fn notice_rows(text: &str, cols: usize) -> Vec<String> {
    let theme = Theme::nexo();
    texts(&Block::Notice { level: Level::Info, text: text.into() }.render(cols, &theme, opts()))
}

#[test]
fn the_compaction_notice_does_not_name_the_curator_at_80x24() {
    let rows = notice_rows(CURATOR_NOTICE, 78);
    assert_eq!(rows, ["◇ context trimmed to 7.4k tokens (history 14.9k)"]);
}

#[test]
fn the_compaction_notice_does_not_name_the_curator_at_40x12() {
    let rows = notice_rows(CURATOR_NOTICE, 38);
    assert!(rows.len() > 1 && rows.iter().all(|r| !r.contains("curator") && width(r) <= 38), "{rows:?}");
    assert_eq!(rows[0], "◇ context trimmed to 7.4k tokens");
    assert_eq!(rows.join(" ").replace("   ", " "), "◇ context trimmed to 7.4k tokens (history 14.9k)");
    // other notices pass through untouched
    assert_eq!(notice_rows("hello", 38), ["◇ hello"]);
}

#[test]
fn the_usage_line_never_cuts_a_figure_in_a_narrow_pane() {
    let usage = nexo_core::stream::Usage { input: 1800, output: 412, cache_read: 12300, ..Default::default() };
    let b = Block::Usage {
        usage,
        cost_usd: 0.0042,
        elapsed_ms: 14300,
        tally: Default::default(),
        plan: false,
        folded: true,
    };
    // the line drops its least useful pieces first and stays on one row
    let rows = texts(&b.render(28, &Theme::dark(), opts()));
    assert!(rows.len() == 1 && width(&rows[0]) <= 28, "{rows:?}");
    assert!(rows[0].contains("14.3s") && rows[0].contains('↑') && rows[0].contains('↓'), "{rows:?}");
    // under its shortest form it wraps instead of cutting the last figure
    let rows = texts(&b.render(16, &Theme::dark(), opts()));
    assert!(rows.len() > 1 && rows.iter().all(|r| width(r) <= 16), "{rows:?}");
    let all = rows.join(" ");
    assert!(all.contains("14.3s") && all.contains("412"), "{rows:?}");
}

fn opts() -> RenderOpts {
    RenderOpts {
        show_reasoning: true,
        fold_after: 3,
        spinner: 0,
        intro_ms: None,
        interpreted: false,
        timestamps: true,
        fx: false,
        detail: nexo_core::config::Detail::Full,
    }
}

#[test]
fn a_tool_card_folds_after_the_limit_and_expands_on_toggle() {
    let out = ToolOutput::ok((1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
    let mut b = Block::Tool {
        id: "c1".into(),
        name: "bash".into(),
        summary: "ls".into(),
        args: Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(out),
        started: None,
        elapsed_ms: Some(1500),
        folded: true,
    };
    let t = texts(&b.render(60, &Theme::dark(), opts()));
    let sh = nexo_core::shell::shell().label();
    assert!(t[0].starts_with(&format!("✓ {sh} ls")), "{}", t[0]);
    assert!(t[0].contains("1.5s"));
    assert_eq!(t.len(), 1, "a finished card folds to its head: {t:?}");
    assert!(t[0].contains("· 10 lines") && t[0].ends_with('▸'), "{}", t[0]);
    assert!(b.toggle_fold());
    let t = texts(&b.render(60, &Theme::dark(), opts()));
    assert_eq!(t.len(), 11);
    assert!(t[0].ends_with('▾'), "{}", t[0]);
}

#[test]
fn a_sub_agent_card_shows_a_robot_that_blinks_while_it_works() {
    let card = |status, beat| {
        let b = Block::Tool {
            id: "c9".into(),
            name: "task".into(),
            summary: "task [explore]: map the parser".into(),
            args: Value::Null,
            status,
            progress: vec![],
            output: Some(ToolOutput::ok("report")),
            started: None,
            elapsed_ms: Some(900),
            folded: true,
        };
        texts(&b.render(60, &Theme::dark(), RenderOpts { spinner: beat, ..opts() }))[0].clone()
    };
    let done = card(ToolStatus::Done, 0);
    assert!(done.starts_with("✓ [o_o] agent [explore]: map the parser"), "{done}");
    assert!(card(ToolStatus::Running, 22).contains("[-_-] agent"), "it blinks while it works");
    assert!(card(ToolStatus::Done, 22).contains("[o_o] agent"), "and rests with open eyes");
}

#[test]
fn an_open_running_shell_card_shows_the_whole_command_wrapped() {
    let cmd = format!("echo {}\nls -la", "x".repeat(70));
    let b = Block::Tool {
        id: "c2".into(),
        name: "bash".into(),
        summary: cmd.clone(),
        args: serde_json::json!({ "command": cmd }),
        status: ToolStatus::Running,
        progress: vec![],
        output: None,
        started: None,
        elapsed_ms: None,
        folded: false,
    };
    let t = texts(&b.render(40, &Theme::dark(), opts()));
    let body = t[1..].join("\n");
    assert!(body.contains("$ echo"), "{t:?}");
    assert!(body.contains("ls -la"), "{t:?}");
    assert!(t.iter().all(|l| l.chars().count() <= 40), "{t:?}");
}

#[test]
fn a_failed_card_keeps_a_short_preview_when_folded() {
    let out = ToolOutput::error((1..=10).map(|i| format!("e{i}")).collect::<Vec<_>>().join("\n"));
    let b = Block::Tool {
        id: "c3".into(),
        name: "bash".into(),
        summary: "false".into(),
        args: Value::Null,
        status: ToolStatus::Failed,
        progress: vec![],
        output: Some(out),
        started: None,
        elapsed_ms: Some(10),
        folded: true,
    };
    let t = texts(&b.render(60, &Theme::dark(), opts()));
    assert!(t.len() > 1 && t.len() <= 1 + 3 + 1, "{t:?}");
    assert!(t.last().unwrap().contains("more lines"), "{t:?}");
}

#[test]
fn a_long_shell_output_is_cut_with_a_count_but_the_system_prompt_is_whole() {
    let out = (1..=30).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
    let sh = |code| Block::Shell { command: "ls".into(), output: out.clone(), exit_code: code };
    let t = texts(&sh(Some(0)).render(60, &Theme::dark(), opts()));
    assert_eq!(t.len(), 1 + 6 + 1, "{t:?}");
    assert!(t.last().unwrap().contains("+24 lines"), "{t:?}");
    let t = texts(&sh(None).render(60, &Theme::dark(), opts()));
    assert_eq!(t.len(), 31, "{t:?}");
}

#[test]
fn a_long_notice_has_one_icon_and_wraps_under_its_text() {
    let b = Block::Notice { level: Level::Info, text: "word ".repeat(30) };
    let t = texts(&b.render(40, &Theme::dark(), opts()));
    assert!(t.len() > 1);
    assert!(t[0].starts_with("◇ "));
    assert!(t[1..].iter().all(|l| l.starts_with("  ") && !l.contains('◇')), "{t:?}");
}

#[test]
fn model_card_and_welcome_box() {
    let b = Block::Model {
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        pricing: Some(Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }),
        context_window: Some(128_000),
        max_output: Some(8192),
        source: "catalogue".into(),
    };
    let t = texts(&b.render(120, &Theme::nexo(), opts()));
    assert_eq!(
            t[0],
            "◈ model deepseek/deepseek-chat · 128k context · 8.2k max reply · $0.28 in · $0.42 out · $0.028 cached · from catalogue"
        );
    let b = Block::Model {
        provider: "p".into(),
        model: "m".into(),
        pricing: None,
        context_window: None,
        max_output: None,
        source: String::new(),
    };
    assert!(texts(&b.render(120, &Theme::nexo(), opts()))[0].contains("no prices known"));
    let w = Block::Welcome {
        provider: "p".into(),
        model: "m".into(),
        cwd: "~/x".into(),
        branch: Some("main".into()),
        permission_mode: "ask".into(),
    };
    let t = texts(&w.render(60, &Theme::nexo(), opts()));
    assert!(t[0].starts_with("╭──"));
    assert!(t[1].contains("●─╮") && t[1].contains("nexo"));
    assert!(t.iter().any(|l| l.contains("p/m · ask")));
    assert!(t.iter().any(|l| l.contains("~/x · main")));
    assert!(t.iter().any(|l| l.contains("/model sets up a model")));
    let narrow = texts(&w.render(30, &Theme::nexo(), opts()));
    assert!(narrow[1].contains("nexo") && !narrow[1].contains('●'), "a narrow box drops the mark");
    assert!(t.last().unwrap().starts_with("╰──"));
    assert_eq!(fmt_price(0.28), "0.28");
    assert_eq!(fmt_price(15.0), "15");
    assert_eq!(fmt_price(3.0), "3.00");
}

#[test]
fn the_caret_blinks_and_the_mark_lights_up() {
    let theme = Theme::nexo();
    let live = Block::Assistant { text: "Hello".into(), streaming: true, at: None, started: None, took_ms: None };
    let on = live.render(40, &theme, RenderOpts { spinner: 0, ..opts() });
    let last = on.last().unwrap().spans.last().unwrap().clone();
    assert_eq!(last.content, "▍");
    assert_eq!(last.style.fg, Some(theme.accent), "lit");
    let off = live.render(40, &theme, RenderOpts { spinner: 6, ..opts() });
    assert_eq!(off.last().unwrap().spans.last().unwrap().style.fg, Some(theme.dim2), "and dark half the time");
    let done = Block::Assistant { text: "Hello".into(), streaming: false, at: None, started: None, took_ms: None };
    assert!(!texts(&done.render(40, &theme, opts()))[0].contains('▍'), "no caret once the reply is done");
    // the first node is born as a dot, the last one is not there yet, the centre waits as a seed
    assert_eq!(intro_cell(&theme, '●', theme.accent2, 0, 0, 0), ('·', theme.fg));
    assert_eq!(intro_cell(&theme, '●', theme.accent2, 2, 0, 100).0, ' ', "the last node waits its turn");
    assert_eq!(intro_cell(&theme, '●', theme.accent, 1, 3, 100), ('·', theme.dim2));
    assert_eq!(intro_cell(&theme, '●', theme.accent2, 2, 0, INTRO_MS), ('●', theme.accent2));
    assert_eq!(intro_cell(&theme, '●', theme.accent, 1, 3, INTRO_MS), ('●', theme.accent), "the centre settles");
}

/// Every cell of `lines` with its style, so frames split into spans differently compare.
fn cells(lines: &[Line]) -> Vec<Vec<(String, Style)>> {
    lines
        .iter()
        .map(|l| l.spans.iter().flat_map(|s| s.content.chars().map(move |c| (c.to_string(), s.style))).collect())
        .collect()
}

#[test]
fn the_banner_intro_never_jumps_and_ends_on_the_still_banner() {
    for theme in [Theme::nexo(), Theme::mono()] {
        for w in [30, 44, 80, 120] {
            let at = |ms| {
                welcome(
                    w,
                    &theme,
                    "deepseek/deepseek-flash",
                    "~/work · main",
                    "auto",
                    ms,
                    nexo_core::config::Detail::Full,
                )
            };
            let still = at(None);
            let widths: Vec<usize> = still.iter().map(Line::width).collect();
            let mut ms = 0;
            while ms <= INTRO_MS {
                let frame = at(Some(ms));
                assert_eq!(frame.iter().map(Line::width).collect::<Vec<_>>(), widths, "w {w} at {ms} ms");
                ms += 16;
            }
            assert_eq!(cells(&at(Some(INTRO_MS))), cells(&still), "w {w}: the last frame is the still banner");
        }
    }
    // halfway the title is being typed with its caret, and the place is not there yet
    let theme = Theme::nexo();
    let mid = texts(&welcome(
        80,
        &theme,
        "m/m",
        "~/work",
        "auto",
        Some(TYPE_TITLE.0 + 2 * TYPE_TITLE.1),
        nexo_core::config::Detail::Full,
    ));
    assert!(mid[1].contains("nex▍") && !mid[3].contains("~/work"), "{mid:?}");
}

#[test]
fn a_todo_card_lists_the_tasks_with_their_glyphs_and_counts() {
    let items = vec![
        TodoItem { text: "read the test".into(), status: TodoStatus::Done },
        TodoItem { text: "fix the conversion".into(), status: TodoStatus::InProgress },
        TodoItem { text: "run the suite".into(), status: TodoStatus::Pending },
    ];
    assert_eq!(todo_counts(&items), (1, 3));
    let b = Block::Tool {
        id: "t".into(),
        name: "todo".into(),
        summary: "3 tasks".into(),
        args: Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(ToolOutput::ok("ok").with_display(ToolDisplay::Todo { items })),
        started: None,
        elapsed_ms: None,
        folded: false,
    };
    let t = texts(&b.render(60, &Theme::nexo(), opts()));
    assert!(t[0].contains("todo 3 tasks · 1/3 done"), "{}", t[0]);
    let mut said = b.clone();
    if let Block::Tool { summary, .. } = &mut said {
        *summary = "todo: 1/3 done".into();
    }
    let t = texts(&said.render(60, &Theme::nexo(), opts()));
    assert_eq!(t[0].matches("1/3 done").count(), 1, "said once: {}", t[0]);
    assert_eq!(t[1], "│ ● read the test");
    assert_eq!(t[2], "│ ◐ fix the conversion");
    assert_eq!(t[3], "└ ○ run the suite", "the guide ends in a corner on the last line");
    let done = b.render(60, &Theme::nexo(), opts())[1].spans[1].style;
    assert!(done.add_modifier.contains(Modifier::CROSSED_OUT));
}

#[test]
fn reasoning_notices_and_plain_results_read_as_markdown() {
    let th = Theme::dark();
    let r = Block::Reasoning {
        text: "**plan** the `fix`\n- one".into(),
        streaming: false,
        started: None,
        elapsed_ms: Some(10),
        folded: false,
    };
    let all = texts(&r.render(40, &th, opts())).join("\n");
    assert!(all.contains("plan the fix") && !all.contains("**") && !all.contains('`'), "{all}");
    let n = notice_rows("run `nexo login` first\n  **ok** done", 60).join("\n");
    assert!(n.contains("run nexo login first") && n.contains("ok done") && !n.contains("**"), "{n}");
    let plain = |content: &str| {
        let out = ToolOutput::ok(content).with_display(ToolDisplay::Plain);
        let b = Block::Tool {
            id: "w".into(),
            name: "web".into(),
            summary: "s".into(),
            args: Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(out),
            started: None,
            elapsed_ms: None,
            folded: false,
        };
        texts(&b.render(60, &th, opts())).join("\n")
    };
    let md = plain("## Findings\nit is **fast**");
    assert!(md.contains("Findings") && !md.contains("##") && !md.contains("**"), "{md}");
}

#[test]
fn reasoning_hides_when_asked_and_folds_to_one_line() {
    let b = Block::Reasoning {
        text: "a\nb\nc".into(),
        streaming: false,
        started: None,
        elapsed_ms: Some(2100),
        folded: true,
    };
    let t = texts(&b.render(40, &Theme::dark(), opts()));
    assert_eq!(t, vec!["▸ thinking · 3 lines · 2.1s"]);
    let hidden = b.render(40, &Theme::dark(), RenderOpts { show_reasoning: false, ..opts() });
    assert!(hidden.is_empty());
}

fn remembered(scope: &str, items: &[&str], path: &str) -> Block {
    let display = ToolDisplay::Remembered {
        scope: scope.into(),
        items: items.iter().map(|s| s.to_string()).collect(),
        path: path.into(),
    };
    Block::Tool {
        id: "n".into(),
        name: "note".into(),
        summary: "note: Rule: x".into(),
        args: Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(ToolOutput::ok("noted 1 line; 1 new rule saved").with_display(display)),
        started: None,
        elapsed_ms: Some(1),
        folded: true,
    }
}

#[test]
fn what_nexo_remembers_is_a_card_with_its_scope_the_line_and_a_link_to_the_file() {
    let b = remembered("project", &["the API runs on port 4100"], "/home/me/my app/AGENTS.md");
    for theme in [Theme::dark(), Theme::light()] {
        let t = texts(&b.render(60, &theme, opts()));
        assert_eq!(
            t,
            vec![
                "✦ remembered · this project",
                "│ • the API runs on port 4100",
                "└ file:///home/me/my%20app/AGENTS.md"
            ]
        );
        // the glyphs and the address are in colours of the theme, not the default
        let lines = b.render(60, &theme, opts());
        assert_eq!(lines[0].spans[0].style.fg, Some(theme.accent2));
        assert_eq!(lines[2].spans[1].style.fg, Some(theme.accent));
    }
    // folded or not, a finished save is not a tool card with a chevron
    let g =
        texts(&remembered("global", &["a", "b"], "C:\\Users\\me\\.nexo\\prompt.md").render(40, &Theme::dark(), opts()));
    assert_eq!(g[0], "✦ remembered · global");
    assert_eq!(&g[1..3], ["│ • a", "│ • b"]);
    assert_eq!(g[3], "└ file:///C:/Users/me/.nexo/prompt.md");
    // a long line wraps under its text; the address stays whole on its own row
    let long =
        texts(&remembered("global", &["one two three four five six seven eight nine ten"], "/h/prompt.md").render(
            20,
            &Theme::dark(),
            opts(),
        ));
    assert!(long.len() > 3 && long[1..long.len() - 1].iter().all(|l| width(l) <= 20), "{long:?}");
    assert!(long[2].starts_with("│   "), "{long:?}");
    assert_eq!(long.last().unwrap(), "└ file:///h/prompt.md");
}

#[test]
fn a_newer_nexo_shows_as_a_framed_card() {
    let t = texts(&Block::Update { behind: 4 }.render(70, &Theme::dark(), opts()));
    let all = t.join("\n");
    assert_eq!(t.len(), 4, "{all}");
    assert!(t[0].starts_with("╭─ ⬆ Update ready ") && t[0].ends_with('╮'), "{all}");
    assert_eq!(t[1].trim_end(), "│ 4 new commits, ready-built.         │", "{all}");
    assert!(t[2].contains("Run nexo update, then restart nexo.") && t[3].starts_with('╰'), "{all}");
    assert!(t.iter().all(|l| width(l) == width(&t[0])), "{all}");
    // too narrow for the frame: two plain lines that still say what to do
    let n = texts(&Block::Update { behind: 1 }.render(30, &Theme::dark(), opts()));
    assert!(n[0].trim_start().starts_with("⬆ Update ready · 1 new"), "{n:?}");
    assert!(n.iter().all(|l| width(l) <= 30), "{n:?}");
}

#[test]
fn ended_jobs_note_shows_as_a_compact_card() {
    let note = "[background jobs ended]\n- job 1 `cd /home/user/project && for t in a b c; do echo $t; done` done (exit 0) after 2 min 43 s, 40 lines of output\n- job 2 `false` done (exit 1) after 3 s, 1 lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id).";
    let b = Block::User { text: note.into(), at: None };
    let t = texts(&b.render(70, &Theme::dark(), opts()));
    let all = t.join("\n");
    assert!(!all.contains("[background jobs ended]") && !all.contains("Read the output"), "{all}");
    assert!(!all.contains('▌'), "{all}");
    assert_eq!(t.len(), 3, "{all}");
    assert!(t[1].starts_with("⚙ job 1  cd /home"), "{all}");
    assert!(t[1].contains("…") && t[1].ends_with("✓ exit 0 · 2 min 43 s · 40 lines"), "{all}");
    assert!(t[2].starts_with("⚙ job 2  false") && t[2].ends_with("✗ exit 1 · 3 s · 1 line"), "{all}");
    assert!(t.iter().all(|l| width(l) <= 70), "{all}");
    assert_eq!(b.copy_text().as_deref(), Some(note));
}

#[test]
fn a_long_diff_row_is_shortened_with_an_ellipsis_not_cut_by_the_edge() {
    let out = ToolOutput::ok("ok").with_display(ToolDisplay::Diff {
        path: "a.rs".into(),
        old: "a\n".into(),
        new: format!("a\n{}\n", "x".repeat(150)),
        start_line: 1,
    });
    let b = Block::Tool {
        id: "e".into(),
        name: "edit".into(),
        summary: "a.rs".into(),
        args: Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(out),
        started: None,
        elapsed_ms: None,
        folded: false,
    };
    let t = texts(&b.render(60, &Theme::dark(), opts()));
    let all = t.join("\n");
    assert!(t.iter().all(|l| width(l) <= 60), "{all}");
    assert!(t.iter().any(|l| l.contains("+ xxx") && l.ends_with('…')), "{all}");
}

#[test]
fn diff_cards_show_counts_and_user_lines_carry_the_bar() {
    let out = ToolOutput::ok("ok").with_display(ToolDisplay::Diff {
        path: "a.rs".into(),
        old: "x\ny\n".into(),
        new: "x\nz\nw\n".into(),
        start_line: 1,
    });
    let b = Block::Tool {
        id: "e".into(),
        name: "edit".into(),
        summary: "a.rs".into(),
        args: Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(out),
        started: None,
        elapsed_ms: None,
        folded: false,
    };
    let t = texts(&b.render(60, &Theme::dark(), opts()));
    assert!(t[0].contains("+2 −1"), "{}", t[0]);
    assert!(t.iter().any(|l| l.contains("- y")));
    let u = Block::User { text: "hello\nworld".into(), at: None };
    assert_eq!(texts(&u.render(20, &Theme::dark(), opts())), vec!["▌ hello", "▌ world"]);
    let u = Block::User { text: "hello".into(), at: Some("12:34".into()) };
    let t = texts(&u.render(20, &Theme::dark(), opts()));
    assert_eq!(t, vec!["▌ hello        12:34"]);
    let u = Block::User { text: "a line that fills everything".into(), at: Some("12:34".into()) };
    assert!(!texts(&u.render(30, &Theme::dark(), opts()))[0].contains("12:34"), "no room, no stamp");
    let off = RenderOpts { timestamps: false, ..opts() };
    let u = Block::User { text: "hello".into(), at: Some("12:34".into()) };
    assert_eq!(texts(&u.render(20, &Theme::dark(), off)), vec!["▌ hello"], "off: no stamp");
    let r = Block::Assistant {
        text: "hi".into(),
        streaming: false,
        at: Some("14:32".into()),
        started: None,
        took_ms: Some(64_000),
    };
    assert_eq!(texts(&r.render(30, &Theme::dark(), opts())), vec!["◆ hi             14:32 · 1m04s"]);
    assert_eq!(texts(&r.render(30, &Theme::dark(), off)), vec!["◆ hi"]);
    assert_eq!(fmt_k(1234), "1.2k");
    assert_eq!(fmt_k(2_500_000), "2.50M");
    assert_eq!(fmt_k(3_100_000_000), "3.10B");
    assert_eq!(fmt_ms(61_000), "1m01s");
}
