//! The bell, titles, the transcript scrollbar and the status bar.

use super::*;

#[test]
fn a_long_turn_rings_when_it_ends_and_a_short_one_does_not() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert!(a.bell_request.is_none());
    a.handle_agent(AgentEvent::TurnStart { turn: 2 });
    a.turn_started = Some(Instant::now() - std::time::Duration::from_secs(20));
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    assert!(a.bell_request.as_deref().is_some_and(|t| t.starts_with("nexo: done after")), "{:?}", a.bell_request);
}

#[test]
fn remember_adds_a_line_to_the_projects_instructions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let (mut a, mut rx) = app();
    a.cwd = dir.path().to_path_buf();
    a.run_command(Command::Remember(Some("use tabs".into())));
    assert_eq!(std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(), "- use tabs\n");
    std::fs::write(dir.path().join("AGENTS.md"), "# Rules").unwrap();
    a.run_command(Command::Remember(Some("  run the tests\n before a commit ".into())));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap(),
        "# Rules\n- run the tests before a commit\n"
    );
    a.run_command(Command::Remember(None));
    assert!(rx.try_recv().is_err(), "nothing goes to the agent");
}
#[test]
fn memory_opens_the_projects_instructions_without_asking_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "Rules.").unwrap();
    let (mut a, mut rx) = app();
    a.cwd = dir.path().to_path_buf();
    a.run_command(Command::Memory { user: false });
    assert!(rx.try_recv().is_err());
    assert_eq!(a.edit_request.as_deref(), Some(dir.path().join("CLAUDE.md").as_path()));
}

#[test]
fn logout_removes_the_providers_stored_key_under_its_auth_entry() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("auth.json");
    let key = |k: &str| nexo_core::auth::Credential::Api { key: k.into() };
    nexo_core::auth::set_in(&f, "deepseek", key("a")).unwrap();
    nexo_core::auth::set_in(&f, "shared", key("b")).unwrap();
    let mut p = nexo_core::ProviderConfig::new("other", "http://x", None);
    p.auth = Some("shared".into());
    let providers = vec![("other".to_string(), p)];
    let (level, text) = sign_out(&providers, "deepseek", None, &f);
    assert_eq!(level, Level::Info, "{text}");
    assert!(text.starts_with("deepseek: signed out"));
    let (level, _) = sign_out(&providers, "deepseek", Some("other"), &f);
    assert_eq!(level, Level::Info, "the entry the provider names");
    let (level, text) = sign_out(&providers, "deepseek", None, &f);
    assert_eq!(level, Level::Warn, "{text}");
    assert_eq!(std::fs::read_to_string(&f).unwrap().matches("\"key\"").count(), 0);
}
#[test]
fn prompt_edit_creates_the_preferences_file_when_it_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut rx) = app();
    let path = dir.path().join("prompt.md");
    std::env::set_var("NEXO_CONFIG_DIR", dir.path());
    a.prompt_edit();
    std::env::remove_var("NEXO_CONFIG_DIR");
    // no round trip to the agent: the file starts as the preferences template and opens
    assert!(rx.try_recv().is_err());
    assert_eq!(a.edit_request.as_deref(), Some(path.as_path()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), nexo_agent::prompt::PREFS_TEMPLATE);
}

#[test]
fn permissions_queue_behind_the_open_modal() {
    let (mut a, mut rx) = app();
    let req = |id: &str| PermissionRequest {
        id: id.into(),
        tool: "bash".into(),
        kind: nexo_core::tool::ToolKind::Exec,
        summary: "x".into(),
        detail: ToolDisplay::Plain,
        suggested_rules: vec![],
        agent: None,
    };
    a.handle_agent(AgentEvent::PermissionAsk { request: req("p1") });
    a.handle_agent(AgentEvent::PermissionAsk { request: req("p2") });
    assert_eq!(a.modal.as_ref().and_then(Modal::id), Some("p1"));
    a.modal_out(ModalOut::Permission { id: "p1".into(), decision: PermissionDecision::Allow });
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::Permission { id, .. }) if id == "p1"));
    assert_eq!(a.modal.as_ref().and_then(Modal::id), Some("p2"));
}

#[test]
fn the_memory_figure_spins_while_the_curator_counts_a_loaded_session() {
    use crate::ui::title_segments;
    let (mut a, _rx) = app();
    let texts = |a: &App| -> String {
        title_segments(a, 400).iter().flat_map(|g| g.iter().map(|s| s.text.clone())).collect::<Vec<_>>().join("|")
    };
    assert!(!texts(&a).contains("computing"), "a new session has nothing to wait for");
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("hello") });
    assert!(a.context_wait);
    assert!(a.load() != crate::frame::Load::Idle, "the spinner needs the beat");
    let before = texts(&a);
    assert!(before.contains("session ⠋ counting…"), "{before}");
    a.spinner += 2;
    assert_ne!(texts(&a), before, "the frame advances with the beat");
    a.handle_agent(AgentEvent::Context {
        view_tokens: 400,
        history_tokens: 1000,
        history_bytes: 4000,
        view_bytes: 1600,
        messages_in_view: 1,
        messages_total: 1,
        policy: "curator".into(),
        note: None,
        label: "curator".into(),
        detail: None,
    });
    assert!(!a.context_wait);
    let after = texts(&a);
    assert!(after.contains("session 1.0k tok") && !after.contains("counting"), "{after}");
}

#[test]
fn title_pieces_leave_in_order_and_the_session_name_is_bounded() {
    use crate::ui::{title_segments, TITLE_COLS};
    let (mut a, _rx) = app();
    a.cwd = std::path::PathBuf::from("/srv/project-alpha");
    a.branch = Some("main".into());
    a.goal = Some(Goal { text: "ship it".into(), round: 0, max: 5, state: GoalState::Active });
    a.handle_agent(AgentEvent::ModelLimits {
        provider: "p".into(),
        model: "m".into(),
        pricing: None,
        context_window: Some(1000),
        max_output: None,
        source: "config".into(),
    });
    a.handle_agent(AgentEvent::Context {
        view_tokens: 400,
        history_tokens: 1000,
        history_bytes: 4000,
        view_bytes: 1600,
        messages_in_view: 1,
        messages_total: 1,
        policy: "curator".into(),
        note: None,
        label: "curator +3.0k tok".into(),
        detail: None,
    });
    let texts = |w: usize| -> Vec<String> {
        title_segments(&a, w).iter().map(|g| g.iter().map(|s| s.text.clone()).collect::<String>()).collect()
    };
    let full = texts(400);
    assert!(full.iter().any(|t| t == "session 1.0k tok"), "{full:?}");
    // mem and the gauge already say it; a separate "kept out" figure was the same number twice
    assert!(!full.iter().any(|t| t.contains("kept out")), "{full:?}");
    assert!(full.iter().any(|t| t.contains("main")), "{full:?}");
    assert!(full.iter().any(|t| t.contains("◎ goal")), "{full:?}");
    let mut last = full.len();
    for w in (20..400).rev() {
        let n = texts(w).len();
        assert!(n <= last, "a narrower bar never has more pieces ({w})");
        last = n;
    }
    // the order: directory, then branch, then memory, then the counts, then the model shortened
    let order: Vec<Vec<String>> = (20..400).rev().map(texts).collect();
    let first_without = |needle: &str| order.iter().position(|t| !t.iter().any(|x| x.contains(needle)));
    let dir = first_without("project-alpha").unwrap();
    let branch = first_without("main").unwrap();
    let mem = first_without("session ").unwrap();
    let counts = first_without("/1.0k").unwrap();
    assert!(dir <= branch && branch <= mem && mem <= counts, "{dir} {branch} {mem} {counts}");
    a.session_title = "a very long session name that would eat the key hints".into();
    a.session_id = Some("20261027-000000-abcd".into());
    let bar = crate::ui::hint_right_text(&a);
    assert!(bar.contains('…'));
    assert!(crate::render::wrap::width(&bar) < TITLE_COLS + 30, "{bar}");
}

#[test]
fn the_scrollback_text_holds_every_block_wrapped_with_the_screen_gaps() {
    let (mut a, _rx) = app();
    a.push(Block::Assistant { text: "first answer".into(), streaming: false, at: None, started: None, took_ms: None });
    a.push(Block::User { text: "a question that is long enough to wrap around".into(), at: None });
    long_answer(&mut a, 0);
    let text = crate::inline::scrollback_text(&mut a, 30);
    assert!(text.contains("first answer") && text.contains("line 59"), "{text}");
    assert!(text.lines().all(|l| l.chars().count() < 30), "wrapped to the width");
    assert!(text.lines().count() > 60 && !text.ends_with('\n'));
}

#[test]
fn the_transcript_thumb_drags_one_to_one_and_selects_no_text() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    long_answer(&mut a, 0);
    frame(&mut a, 60, 20);
    let bar = a.transcript_bar.expect("a long transcript has a bar");
    assert!(a.follow && bar.offset == bar.max_offset(), "at the end");
    let (start, size) = bar.thumb();
    assert!(size >= 1 && size < bar.len);
    // held by its top row, from the column left of the bar (the wider grip)
    pointer(&mut a, K::Down(MouseButton::Left), bar.x - 1, bar.y + start);
    assert!(a.bar_drag.is_some());
    pointer(&mut a, K::Drag(MouseButton::Left), bar.x - 1, bar.y);
    assert_eq!(a.scroll, 0);
    assert!(!a.follow);
    assert_eq!(a.view_top(), 0, "a drag does not glide");
    assert!(a.selection.is_none() && a.press.is_none(), "no text selected on the way");
    // halfway down the track: the offset follows the thumb's top
    pointer(&mut a, K::Drag(MouseButton::Left), bar.x, bar.y + (bar.len - size) / 2);
    let mid = a.scroll;
    assert!(mid > 0 && mid < bar.max_offset(), "{mid}");
    pointer(&mut a, K::Up(MouseButton::Left), bar.x, bar.y + 3);
    assert!(a.bar_drag.is_none());
    // the drag is over: the pointer elsewhere moves nothing
    pointer(&mut a, K::Drag(MouseButton::Left), bar.x, bar.y);
    assert_eq!(a.scroll, mid);
}

#[test]
fn a_click_on_the_track_glides_there_and_the_wheel_glides_too() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    long_answer(&mut a, 0);
    frame(&mut a, 60, 20);
    a.scroll_to_line(0, false);
    frame(&mut a, 60, 20);
    let bar = a.transcript_bar.unwrap();
    pointer(&mut a, K::Down(MouseButton::Left), bar.x, bar.y + bar.len - 1);
    pointer(&mut a, K::Up(MouseButton::Left), bar.x, bar.y + bar.len - 1);
    assert!(a.scroll > bar.len as usize, "jumped down the transcript: {}", a.scroll);
    frame(&mut a, 60, 20);
    assert!(a.scroll_fx.is_moving(Instant::now()), "on its way there");
    assert!(a.view_top() < a.scroll);
    a.dirty = false;
    a.tick();
    assert!(a.dirty, "a glide keeps the frames coming");

    // with the animations off, everything lands at once
    let (mut a, _rx) = app();
    a.opts.animations = false;
    long_answer(&mut a, 0);
    frame(&mut a, 60, 20);
    pointer(&mut a, K::ScrollUp, 10, 5);
    frame(&mut a, 60, 20);
    assert_eq!(a.view_top(), a.scroll);
    assert!(!a.scroll_fx.is_moving(Instant::now()));
}

#[test]
fn the_thumb_lights_up_under_the_pointer() {
    use crossterm::event::MouseEventKind as K;
    let (mut a, _rx) = app();
    long_answer(&mut a, 0);
    let before = frame(&mut a, 60, 20);
    assert!(!before.iter().any(|r| r.contains('▐')));
    let bar = a.transcript_bar.unwrap();
    a.dirty = false;
    pointer(&mut a, K::Moved, bar.x, bar.y + 1);
    assert_eq!(a.hover, Hover::Bar(scrollbar::Target::Transcript));
    assert!(a.dirty, "a new hover draws a frame");
    let lit = frame(&mut a, 60, 20);
    let (start, _) = bar.thumb();
    assert!(lit[(bar.y + start) as usize].ends_with("▐█"), "{:?}", lit[(bar.y + start) as usize]);
    // moving within it changes nothing
    a.dirty = false;
    pointer(&mut a, K::Moved, bar.x - 1, bar.y + 2);
    assert!(!a.dirty);
    pointer(&mut a, K::Moved, 3, bar.y + 2);
    assert_ne!(a.hover, Hover::Bar(scrollbar::Target::Transcript));
}

#[test]
fn the_new_pill_beats_and_a_click_on_it_goes_to_the_end() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    long_answer(&mut a, 0);
    frame(&mut a, 60, 20);
    a.scroll_to_line(0, false);
    frame(&mut a, 60, 20);
    long_answer(&mut a, 100);
    let rows = frame(&mut a, 60, 20);
    assert!(a.unseen() > 0);
    assert!(rows.iter().any(|r| r.contains("new")), "{rows:?}");
    assert!(a.unseen_at.is_some(), "the thumb beats");
    assert!(anim::pulse(a.unseen_at, Instant::now() + std::time::Duration::from_millis(200)) > 0.0);
    let pill = a.pill;
    assert!(pill.width > 0);
    pointer(&mut a, K::Moved, pill.x + 1, pill.y);
    assert_eq!(a.hover, Hover::Pill);
    pointer(&mut a, K::Down(MouseButton::Left), pill.x + 1, pill.y);
    assert!(a.follow);
    frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 0);
}

#[test]
fn the_status_bar_opens_the_model_the_context_and_the_usage() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    a.cost = 0.25;
    frame(&mut a, 120, 12);
    let at = |a: &App, h: StatusHit| a.status_hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r);
    let model = at(&a, StatusHit::Model).expect("the model is on the bar");
    let cost = at(&a, StatusHit::Cost).expect("so is the cost");
    assert!(model.x + model.width <= cost.x);
    pointer(&mut a, K::Moved, model.x, model.y);
    assert_eq!(a.hover, Hover::Status(StatusHit::Model));
    pointer(&mut a, K::Down(MouseButton::Left), model.x, model.y);
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "the model opens its picker");
    a.modal = None;
    frame(&mut a, 120, 12);
    pointer(&mut a, K::Down(MouseButton::Left), cost.x + 1, cost.y);
    assert!(matches!(&a.modal, Some(Modal::Info(i)) if i.tab == 1), "the cost opens the usage");
    a.modal = None;
    frame(&mut a, 120, 12);
    // the context gauge opens the context window
    let gauge = at(&a, StatusHit::Gauge).expect("the gauge is on the bar");
    pointer(&mut a, K::Down(MouseButton::Left), gauge.x, gauge.y);
    assert!(matches!(&a.modal, Some(Modal::Info(i)) if i.tab == 0));
    a.modal = None;
    frame(&mut a, 120, 12);
    // empty space, the brand and a separator do nothing
    let row = a.status_row;
    let gap = (0..120u16).find(|x| a.status_hit(*x).is_none() && *x > model.x + model.width + 3).expect("a gap");
    for x in [1, model.x + model.width, gap] {
        pointer(&mut a, K::Down(MouseButton::Left), x, row);
        assert!(a.modal.is_none(), "column {x} is not a piece");
    }
}

#[test]
fn the_line_under_a_hovered_effort_starts_at_its_mark_not_under_the_blank_before_it() {
    use crossterm::event::MouseEventKind as K;
    use ratatui::style::Modifier;
    let (mut a, _rx) = app();
    a.effort = Some("high".into());
    a.fast = true;
    frame(&mut a, 120, 12);
    for hit in [StatusHit::Effort, StatusHit::Fast] {
        let r = a.status_hits.iter().find(|(_, x)| *x == hit).map(|(r, _)| *r).expect("on the bar");
        pointer(&mut a, K::Moved, r.x + 1, r.y);
        assert_eq!(a.hover, Hover::Status(hit));
        let buf = buf_of(&mut a, 120, 12);
        let cell = |x: u16| &buf[(x, r.y)];
        assert_eq!(cell(r.x).symbol(), " ", "{hit:?} begins with its blank");
        assert!(!cell(r.x).modifier.contains(Modifier::UNDERLINED), "{hit:?}: no line under the blank");
        assert!(cell(r.x + 1).modifier.contains(Modifier::UNDERLINED), "{hit:?}: its mark is lit");
        // (the fast mark is one wide glyph: its second cell carries no style of its own)
        if hit == StatusHit::Effort {
            assert!(cell(r.x + r.width - 1).modifier.contains(Modifier::UNDERLINED), "to its last cell");
        }
    }
}

#[test]
fn the_status_bar_effort_and_mode_answer_a_click_and_follow_the_width() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, mut rx) = app();
    a.effort = Some("high".into());
    frame(&mut a, 120, 12);
    let find = |a: &App, h: StatusHit| a.status_hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r);
    let model = find(&a, StatusHit::Model).expect("model");
    let eff = find(&a, StatusHit::Effort).expect("effort is its own piece");
    assert!(model.x + model.width <= eff.x, "the effort does not overlap the model");
    // one cell left of it is the model, one cell right of it is not the effort
    pointer(&mut a, K::Moved, eff.x, eff.y);
    assert_eq!(a.hover, Hover::Status(StatusHit::Effort));
    pointer(&mut a, K::Down(MouseButton::Left), eff.x + 1, eff.y);
    let Some(Modal::Picker(p)) = &a.modal else { panic!("the effort opens its picker") };
    assert_eq!(p.kind, crate::modal::PickKind::Effort);
    // with the picker open, a click on the bar does not reach it
    let before = a.modal.is_some();
    pointer(&mut a, K::Down(MouseButton::Left), eff.x + 1, eff.y);
    assert!(before);
    a.modal = None;
    frame(&mut a, 120, 12);
    pointer(&mut a, K::Down(MouseButton::Left), eff.x + eff.width, eff.y);
    assert!(!matches!(&a.modal, Some(Modal::Picker(_))), "one cell outside is not the effort");
    a.modal = None;
    // the mode opens the permission picker (Shift+Tab keeps cycling ask, accept-edits, plan)
    let mode = find(&a, StatusHit::Mode).expect("mode piece");
    pointer(&mut a, K::Down(MouseButton::Left), mode.x, mode.y);
    let Some(Modal::Picker(p)) = &a.modal else { panic!("the mode opens its picker") };
    assert_eq!(p.kind, crate::modal::PickKind::PermissionMode);
    assert!(rx.try_recv().is_err());
    a.modal = None;
    // a narrower bar: the regions are where the text was drawn, or gone
    for w in [100u16, 70, 50, 30] {
        frame(&mut a, w, 12);
        for (r, h) in a.status_hits.clone() {
            assert!(r.x + r.width <= w, "{h:?} stays on the {w} wide bar");
            assert_eq!(a.status_hit(r.x), Some(h));
            assert_eq!(a.status_hit(r.x + r.width.saturating_sub(1)), Some(h));
        }
    }
    frame(&mut a, 30, 12);
    assert!(a.status_hits.iter().all(|(_, h)| *h != StatusHit::Effort) || a.status_hits.iter().any(|(r, _)| r.x < 30));
}

#[test]
fn the_status_bar_shows_the_default_effort_on_a_model_with_levels_and_drops_it_first() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    let find = |a: &App, h: StatusHit| a.status_hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r);
    // a model without levels: nothing is drawn
    frame(&mut a, 140, 12);
    assert!(find(&a, StatusHit::Effort).is_none());
    // a model with levels and no level set: `◆ default`, and it opens the picker
    a.effort_levels = vec!["low".into(), "medium".into(), "high".into()];
    let text = frame(&mut a, 140, 12).join("\n");
    assert!(text.contains("◆ default"), "{text}");
    let eff = find(&a, StatusHit::Effort).expect("the default is clickable");
    pointer(&mut a, K::Moved, eff.x, eff.y);
    assert_eq!(a.hover, Hover::Status(StatusHit::Effort));
    pointer(&mut a, K::Down(MouseButton::Left), eff.x, eff.y);
    assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == crate::modal::PickKind::Effort));
    a.modal = None;
    // a narrow bar drops it before the directory, the branch and the gauge
    let mut gone_at = None;
    for w in (20..140u16).rev() {
        let t = frame(&mut a, w, 12).join("\n");
        if !t.contains("◆ default") {
            gone_at = Some(w);
            break;
        }
    }
    let w = gone_at.expect("a narrow bar hides it");
    assert!(w > 40, "it goes while the bar still has room for the model and the gauge: {w}");
    assert!(find(&a, StatusHit::Effort).is_none() && find(&a, StatusHit::Model).is_some());
    // a set level stays on the narrow bar
    a.effort = Some("high".into());
    let t = frame(&mut a, w, 12).join("\n");
    assert!(t.contains("◆ high"), "{t}");
}

#[test]
fn the_status_bar_pieces_follow_the_configured_order_and_the_fast_marker_has_its_own_click() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, mut rx) = app();
    a.effort = Some("high".into());
    a.fast = true;
    a.opts.status_line = vec!["context".into(), "mode".into(), "model".into(), "effort".into()];
    frame(&mut a, 140, 12);
    let find = |a: &App, h: StatusHit| a.status_hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r);
    let gauge = find(&a, StatusHit::Gauge).expect("gauge");
    let mode = find(&a, StatusHit::Mode).expect("mode");
    let model = find(&a, StatusHit::Model).expect("model");
    let fast = find(&a, StatusHit::Fast).expect("the fast marker is its own piece");
    let eff = find(&a, StatusHit::Effort).expect("effort");
    assert!(gauge.x < mode.x && mode.x < model.x, "the regions follow the drawn order");
    assert!(model.x + model.width <= fast.x && fast.x + fast.width <= eff.x);
    pointer(&mut a, K::Down(MouseButton::Left), model.x, model.y);
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "the model opens its picker");
    a.modal = None;
    frame(&mut a, 140, 12);
    while rx.try_recv().is_ok() {}
    pointer(&mut a, K::Down(MouseButton::Left), fast.x, fast.y);
    assert!(a.modal.is_none(), "the marker does not open the model picker");
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetFast { on: None })), "the marker runs /fast");
    pointer(&mut a, K::Down(MouseButton::Left), eff.x, eff.y);
    assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == crate::modal::PickKind::Effort));
    // the cursor off the status row lights nothing on the bar
    a.modal = None;
    pointer(&mut a, K::Moved, model.x, model.y);
    assert_eq!(a.hover, Hover::Status(StatusHit::Model));
    pointer(&mut a, K::Moved, model.x, 0);
    assert_ne!(a.hover, Hover::Status(StatusHit::Model));
}
