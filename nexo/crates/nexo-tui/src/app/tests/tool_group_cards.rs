//! Tool calls grouped under one header: folding, navigation, replay.

use super::*;

#[test]
fn the_last_code_block_is_found() {
    let text = "run this:\n```bash\ncargo test\n```\nthen\n````rust\nfn a() {}\n```\nx\n````\nend";
    assert_eq!(last_code_block(text).as_deref(), Some("fn a() {}\n```\nx"));
    assert_eq!(last_code_block("```\ncut off"), None);
    assert_eq!(last_code_block("no code"), None);
}

#[test]
fn ctrl_c_clears_a_draft_first_and_the_title_follows_the_turn() {
    let (mut a, _rx) = app();
    a.input.set_text("half a thought");
    a.ctrl_c();
    assert!(a.input.is_empty());
    assert_eq!(a.input.history().last().map(String::as_str), Some("half a thought"));
    assert!(!a.should_quit);
    a.ctrl_c();
    assert!(a.should_quit);

    let (mut a, _rx) = app();
    assert_eq!(a.terminal_title(), "nexo");
    a.session_title = "fix the parser".into();
    assert_eq!(a.terminal_title(), "nexo · fix the parser");
    a.busy = true;
    assert!(a.terminal_title().ends_with(" nexo · fix the parser"));
    assert_ne!(a.terminal_title(), "nexo · fix the parser");
}

#[test]
fn ctrl_d_while_a_turn_runs_needs_a_second_press_but_idle_quits_at_once() {
    let (mut a, mut rx) = app();
    a.busy = true;
    a.ctrl_d();
    assert!(!a.should_quit);
    assert!(rx.try_recv().is_err(), "no Shutdown on the first press");
    // an armed Ctrl+D must not turn a Ctrl+C meant to interrupt the turn into a quit
    a.ctrl_c();
    assert!(!a.should_quit);
    a.ctrl_d();
    assert!(a.should_quit);
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|c| matches!(c, AgentCommand::Shutdown)));

    let (mut a, _rx) = app();
    a.ctrl_d();
    assert!(a.should_quit);
}

#[test]
fn a_long_command_is_cut_so_the_cache_mark_and_price_still_show() {
    use crate::render::wrap::width;
    let theme = Theme::dark();
    let text = |l: &Line| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
    let head = || Line::from(vec![Span::raw("▸ bash  "), Span::raw("cargo test -p nexo-bench ".repeat(6))]);
    // too long for the line: the command ends in … and the mark and price close the line
    let mut l = head();
    mark_head(&mut l, CallMark::Cached, Some(0.00012), 60, &theme);
    let t = text(&l);
    assert!(t.ends_with("● same prefix ~0.00012 $") && t.contains('…'), "{t}");
    assert_eq!(width(&t), 60);
    // no price known: the word alone
    let mut l = head();
    mark_head(&mut l, CallMark::Sent, None, 50, &theme);
    assert!(text(&l).ends_with("● sent") && width(&text(&l)) == 50, "{}", text(&l));
    // a short command is left whole
    let mut l = Line::from(vec![Span::raw("▸ bash  "), Span::raw("ls")]);
    mark_head(&mut l, CallMark::Cached, Some(0.00012), 60, &theme);
    assert!(text(&l).starts_with("▸ bash  ls ") && !text(&l).contains('…'));
    // a window too narrow for any of it keeps the head as it was
    let mut l = head();
    mark_head(&mut l, CallMark::Cached, Some(0.00012), 20, &theme);
    assert_eq!(text(&l), text(&head()));
}

#[test]
fn repeated_prefix_cost_is_an_undiscounted_estimate_not_a_cache_hit() {
    use nexo_core::pricing::Pricing;
    let (mut a, _rx) = app();
    a.call_tokens.insert("c1".into(), 5000);
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "p".into(),
        model: "m".into(),
        context_window: None,
        max_output: None,
        pricing: Some(Pricing { input: 3.0, cache_read: 0.3, ..Default::default() }),
        source: "config".into(),
    });
    assert_eq!(a.call_cost("c1", CallMark::Cached), Some(0.015));
    assert_eq!(a.call_cost("c1", CallMark::Cached), a.call_cost("c1", CallMark::Sent));
    // Aggregate provider cache usage cannot attribute a hit to this individual output.
    a.last_call_cached = 5000;
    assert_eq!(a.call_cost("c1", CallMark::Cached), Some(0.015));
    for mark in [CallMark::Cut, CallMark::Out, CallMark::Back] {
        assert_eq!(a.call_cost("c1", mark), None);
    }
    assert_eq!(a.call_cost("unknown", CallMark::Cached), None);
    a.limits.as_mut().unwrap().pricing.as_mut().unwrap().input = f64::NAN;
    assert_eq!(a.call_cost("c1", CallMark::Cached), None);
}

#[test]
fn a_failed_call_card_carries_no_cache_tag() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "read".into(),
        args: serde_json::json!({"path":"missing.rs"}),
        summary: "missing.rs".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
    a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: ToolOutput::error("no such file"), elapsed_ms: 5 });
    a.marks.insert("c1".into(), CallMark::Sent);
    let i = a.blocks.iter().position(|b| matches!(b, Block::Tool { .. })).unwrap();
    let head: String =
        a.lines(i, 80).iter().flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect::<Vec<_>>().join("|");
    assert!(matches!(a.blocks[i], Block::Tool { status: ToolStatus::Failed, .. }), "{head}");
    assert!(!head.contains("● sent"), "{head}");
}

#[test]
fn a_head_never_ends_in_a_separator_and_an_ellipsis_or_a_cut_figure() {
    use crate::render::wrap::width;
    let theme = Theme::dark();
    let text = |l: &Line| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
    // the facts after the text are whole pieces: for every room the mark leaves, none is cut
    for w in 66..=79 {
        let mut l = Line::from(vec![
            Span::raw("✓ "),
            Span::raw("read "),
            Span::raw("src/components/deeply/some-file-name.rs"),
            Span::raw(" · 40 lines"),
            Span::raw("  44ms"),
        ]);
        mark_head(&mut l, CallMark::Sent, None, w, &theme);
        let t = text(&l);
        assert!(!t.contains("· …") && !t.contains("· 4…") && !t.contains("lin…") && !t.contains(" 4…"), "{w}: {t}");
        assert!(t.ends_with("● sent") && width(&t) == w, "{w}: {t}");
    }
}

#[test]
fn an_update_shows_its_card_once_and_a_later_check_only_when_more_came_out() {
    let (mut a, _rx) = app();
    let cards = |a: &App| a.blocks.iter().filter(|b| matches!(b, Block::Update { .. })).count();
    a.update_ready(2);
    a.update_ready(2);
    a.update_ready(1);
    assert_eq!((cards(&a), a.update_behind), (1, Some(2)));
    a.update_ready(5);
    assert_eq!((cards(&a), a.update_behind), (1, Some(5)), "a toast, not a second card");
}

#[test]
fn two_tool_calls_form_one_group_marker_and_a_single_call_does_not() {
    let (mut a, _rx) = bare();
    a.push(Block::User { text: "go".into(), at: None });
    let one = a.push(card("a", 2));
    assert_eq!(groups(&a), 0);
    assert_eq!(one, 1);
    let two = a.push(card("b", 2));
    assert_eq!(groups(&a), 1);
    assert!(matches!(a.blocks[1], Block::Group { folded: true, len: 2 }));
    assert_eq!(two, 3, "the call is returned where it now is");
    a.push(card("c", 2));
    assert!(matches!(a.blocks[1], Block::Group { len: 3, .. }), "a third call joins the same group");
    assert_eq!(groups(&a), 1);
    assert_eq!(a.blocks.len(), 5);
    assert!(!a.hidden(1) && a.hidden(2) && a.hidden(3) && a.hidden(4));
}

#[test]
fn reasoning_between_calls_joins_the_group_but_trailing_reasoning_does_not() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(thinking());
    a.push(card("b", 1));
    assert!(matches!(a.blocks[0], Block::Group { len: 3, .. }), "{:?}", a.blocks.len());
    a.push(thinking());
    assert!(matches!(a.blocks[0], Block::Group { len: 3, .. }), "thinking after the last call is outside");
    assert!(!a.hidden(5));
    a.push(Block::Assistant { text: "x".into(), streaming: false, at: None, started: None, took_ms: None });
    a.push(card("c", 1));
    assert_eq!(groups(&a), 1, "an answer breaks the run");
}

#[test]
fn the_agreed_plan_and_opened_thinking_stay_visible_in_a_folded_group() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    let mut open = thinking();
    if let Block::Reasoning { folded, .. } = &mut open {
        *folded = false;
    }
    a.push(open);
    let mut plan = card("p", 1);
    if let Block::Tool { name, .. } = &mut plan {
        *name = "exit_plan".into();
    }
    a.push(plan);
    a.push(card("b", 1));
    assert!(a.hidden(1) && !a.hidden(2), "thinking the user opened is not hidden");
    assert!(!a.hidden(3), "the plan is not hidden");
    assert!(a.hidden(4));
}

#[test]
fn group_tools_off_creates_no_marker() {
    let (mut a, _rx) = bare();
    a.opts.group_tools = false;
    a.push(card("a", 1));
    a.push(card("b", 1));
    assert_eq!(groups(&a), 0);
    assert_eq!(a.blocks.len(), 2);
}

#[test]
fn a_running_member_stays_visible_under_a_folded_header() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(running("b"));
    a.layout(80);
    assert!(a.hidden(1) && !a.hidden(2));
    let h = head(&mut a, 0);
    assert!(h.contains("Running"), "{h}");
    assert_eq!(a.offsets[1], a.offsets[2], "the finished call takes no room");
}

#[test]
fn group_header_changes_tense_when_the_last_call_finishes() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(running("b"));
    a.layout(80);
    assert!(head(&mut a, 0).contains("Running"), "{}", head(&mut a, 0));
    if let Block::Tool { status, output, .. } = &mut a.blocks[2] {
        *status = ToolStatus::Done;
        *output = Some(ToolOutput::ok("ok"));
    }
    a.invalidate(2);
    a.layout(80);
    let h = head(&mut a, 0);
    assert!(h.contains("Ran 2 shell commands"), "{h}");
    assert!(a.hidden(2), "folded again once everything finished");
}

#[test]
fn enter_on_a_group_toggles_it_and_inner_cards_keep_their_own_fold() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(card("a", 3));
    a.push(card("b", 3));
    a.layout(80);
    a.selected = Some(0);
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }));
    a.layout(80);
    assert!(!a.hidden(1) && !a.hidden(2));
    assert!(matches!(a.blocks[1], Block::Tool { folded: true, .. }), "the cards stay folded inside");
    assert!(a.offsets[2] > a.offsets[1]);
    press(&mut a, KeyCode::Enter);
    a.layout(80);
    assert!(a.hidden(1) && a.hidden(2));
    assert_eq!(a.selected, Some(0));
}

#[test]
fn click_on_the_header_selects_then_toggles() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(card("b", 1));
    a.layout(80);
    a.click_block(0, 0);
    assert_eq!(a.selected, Some(0));
    assert!(a.is_folded(0));
    a.click_block(0, 0);
    assert!(!a.is_folded(0));
}

#[test]
fn arrow_navigation_skips_hidden_members() {
    let (mut a, _rx) = bare();
    a.push(Block::User { text: "a long\nmessage\nof lines".into(), at: None });
    a.push(card("a", 3));
    a.push(card("b", 3));
    a.push(Block::Assistant {
        text: "done\n\nmore\n\nlines".into(),
        streaming: false,
        at: None,
        started: None,
        took_ms: None,
    });
    a.layout(80);
    a.selected = None;
    for _ in 0..4 {
        a.select_next(true);
        if let Some(s) = a.selected {
            assert!(!a.hidden(s), "{s} is hidden");
        }
    }
}

#[test]
fn collapse_keeps_scroll_inside_the_transcript() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(Block::User { text: "go".into(), at: None });
    for id in ["a", "b", "c", "d"] {
        a.push(card(id, 20));
    }
    a.transcript_area = Rect::new(0, 1, 80, 6);
    a.layout(80);
    a.toggle_block(1);
    for k in 2..6 {
        a.blocks[k].set_fold(false);
        a.invalidate(k);
    }
    a.layout(80);
    a.follow = false;
    a.scroll = a.offsets[5] + 2;
    a.toggle_block(1);
    a.layout(80);
    a.settle(6);
    assert!(a.scroll <= a.max_scroll(6), "{} > {}", a.scroll, a.max_scroll(6));
    assert!(a.scroll <= a.offsets[1] + 1);
}

#[test]
fn ctrl_o_flips_every_group_and_card() {
    let (mut a, _rx) = bare();
    a.push(card("a", 2));
    a.push(card("b", 2));
    a.fold_all();
    assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }), "everything was folded: it opens");
    assert!(matches!(a.blocks[1], Block::Tool { folded: false, .. }));
    a.fold_all();
    assert!(matches!(a.blocks[0], Block::Group { folded: true, .. }));
    assert!(matches!(a.blocks[2], Block::Tool { folded: true, .. }));
}

#[test]
fn search_hit_in_a_folded_group_counts_once_and_unfolds_on_jump() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(card("a", 2));
    a.push(card("b", 2));
    a.transcript_area = Rect::new(0, 1, 80, 10);
    a.layout(80);
    a.open_search();
    a.search.as_mut().unwrap().query = "line".into();
    a.rematch();
    assert_eq!(a.search.as_ref().unwrap().matches.len(), 1, "one hit for the whole folded group");
    a.search_first();
    assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }));
    a.layout(80);
    assert!(!a.search.as_ref().unwrap().matches.is_empty());
}

#[test]
fn a_folded_group_is_one_copyable_line() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(card("b", 1));
    a.layout(80);
    let t = a.line_text(0).unwrap();
    assert!(t.contains("Ran 2 shell commands"), "{t}");
}

#[test]
fn replayed_session_groups_like_a_live_one() {
    let (mut a, _rx) = bare();
    a.push(card("a", 1));
    a.push(thinking());
    a.push(card("b", 1));
    a.push(card("c", 1));
    assert!(matches!(a.blocks[0], Block::Group { len: 4, .. }));
    assert_eq!(a.blocks.len(), 5);
}

#[test]
fn animations_off_opens_instantly() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(card("a", 4));
    a.push(card("b", 4));
    a.layout(80);
    a.toggle_block(0);
    a.layout(80);
    let full = a.height(1, 80);
    assert_eq!(a.shown(1, 80), full);
}

#[test]
fn a_long_finished_group_costs_nothing_per_frame() {
    let (mut a, _rx) = bare();
    for n in 0..200 {
        a.push(card(&format!("c{n}"), 3));
    }
    a.layout(80);
    a.layout(80);
    let rendered = a.cache.iter().filter(|c| matches!(c, Some((_, _, Some(_), _)))).count();
    assert!(rendered <= 2, "hidden calls are not rendered: {rendered}");
    assert!(a.total_lines <= 2, "one line and its gap: {}", a.total_lines);
}

#[test]
fn skill_cards_stay_visible_between_grouped_tools_and_follow_real_events() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    a.push(card("before", 1));
    a.handle_agent(AgentEvent::ToolCall {
        id: "skill".into(),
        name: "skill".into(),
        args: serde_json::json!({"name":"Verify"}),
        summary: "skill Verify".into(),
    });
    a.push(card("after", 1));
    assert!(!a.blocks.iter().any(|b| matches!(b, Block::Group { .. })));
    let i = a.tool_index("skill").unwrap();
    let text = |a: &mut App| a.lines(i, 80).iter().map(crate::render::markdown::text_of).collect::<Vec<_>>().join("\n");
    assert!(text(&mut a).contains("| queued"));
    a.handle_agent(AgentEvent::ToolStart { id: "skill".into() });
    assert!(text(&mut a).contains("| loading"));
    a.handle_agent(AgentEvent::ToolDone {
        id: "skill".into(),
        output: ToolOutput::ok("SECRET INSTRUCTIONS"),
        elapsed_ms: 1,
    });
    let shown = text(&mut a);
    assert!(shown.contains("Skill · Verify | loaded") && !shown.contains("SECRET"), "{shown}");
    // A replayed summary updates the existing card, not its status or fold state.
    a.handle_agent(AgentEvent::ToolCall {
        id: "skill".into(),
        name: "skill".into(),
        args: serde_json::json!({"name":"Verify"}),
        summary: "skill Verify".into(),
    });
    assert!(text(&mut a).contains("| loaded"));
    a.tick_frame(true);
    assert!(!text(&mut a).contains("SECRET"));
    a.toggle_block(i);
    assert!(text(&mut a).contains("SECRET INSTRUCTIONS"));
}

#[test]
fn skill_history_replay_is_folded_static_and_preserves_errors() {
    for error in [false, true] {
        let (mut a, _rx) = bare();
        a.opts.animations = true;
        a.handle_agent(AgentEvent::History {
            index: 0,
            message: Message::new(
                Role::Assistant,
                vec![Part::tool_call("s", "skill", serde_json::json!({"name":"Verify"}))],
            ),
        });
        a.handle_agent(AgentEvent::History {
            index: 1,
            message: Message::tool_results(vec![Part::tool_result("s", "PRIVATE INSTRUCTIONS", error)]),
        });
        let i = a.tool_index("s").unwrap();
        assert!(!a.blocks[i].is_live());
        assert!(matches!(&a.blocks[i], Block::Tool { started: None, folded: true, .. }));
        let rows = a.lines(i, 80).iter().map(crate::render::markdown::text_of).collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains(if error { "| error" } else { "| loaded" }));
        assert!(!rows[0].contains("PRIVATE"));
        // No extra retained motion state is attached to old skill loads.
        assert!(a.looks[i].done_at.is_none());
        assert!(!a.looks[i].was_live);
    }
}
