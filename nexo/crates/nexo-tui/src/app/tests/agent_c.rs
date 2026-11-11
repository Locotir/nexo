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

#[test]
fn up_on_an_empty_draft_takes_the_last_queued_message_back_to_edit_it() {
    let (mut a, mut rx) = app();
    queue_two(&mut a, &mut rx);
    assert_eq!((a.queued, a.queued_msgs.len()), (2, 2));
    let rows = frame(&mut a, 100, 20);
    assert!(rows.iter().any(|r| r.contains("◷ first request")), "{rows:?}");
    assert!(rows.iter().any(|r| r.contains("◷ second request")), "{rows:?}");
    press(&mut a, KeyCode::Up);
    assert_eq!(a.input.full_text(), "second request");
    assert_eq!(a.queued_msgs, vec!["first request".to_string()]);
    assert_eq!(a.queued, 1);
    assert_eq!(drain(&mut rx), vec![AgentCommand::Unqueue { index: 1, text: "second request".into() }]);
    let rows = frame(&mut a, 100, 20);
    assert!(!rows.iter().any(|r| r.contains("◷ second request")), "the row went with it");
    // Enter sends the edited text and queues it again
    a.input.set_text("second request, edited");
    a.submit();
    assert!(matches!(&drain(&mut rx)[..], [AgentCommand::Submit { text, .. }] if text == "second request, edited"));
    assert_eq!(a.queued_msgs, vec!["first request".to_string(), "second request, edited".to_string()]);
    // the agent starts the first one: the count and the texts follow
    a.handle_agent(AgentEvent::Queued { pending: 1 });
    assert_eq!(a.queued_msgs, vec!["second request, edited".to_string()]);
    a.handle_agent(AgentEvent::TurnStart { turn: 2 });
    assert_eq!((a.queued, a.queued_msgs.len()), (0, 0));
}

#[test]
fn a_click_on_a_queued_row_takes_that_one_back_and_a_full_draft_is_kept() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, mut rx) = app();
    queue_two(&mut a, &mut rx);
    frame(&mut a, 100, 20);
    let (row, i) = a.queue_rows[0];
    assert_eq!(i, 0);
    a.input.set_text("half typed");
    pointer(&mut a, K::Down(MouseButton::Left), row.x + 2, row.y);
    assert_eq!(a.input.full_text(), "half typed", "a draft in progress is never overwritten");
    assert_eq!(a.queued_msgs.len(), 2);
    a.input.clear();
    pointer(&mut a, K::Down(MouseButton::Left), row.x + 2, row.y);
    assert_eq!(a.input.full_text(), "first request");
    assert_eq!(a.queued_msgs, vec!["second request".to_string()]);
    assert_eq!(drain(&mut rx), vec![AgentCommand::Unqueue { index: 0, text: "first request".into() }]);
}

#[test]
fn the_pieces_of_the_keys_row_light_up_and_open_their_views() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
    use nexo_core::tool::TodoStatus;
    let (mut a, mut rx) = app();
    a.opts.todo_strip = true;
    queue_two(&mut a, &mut rx);
    a.todo = vec![
        TodoItem { text: "read".into(), status: TodoStatus::Done },
        TodoItem { text: "fix".into(), status: TodoStatus::InProgress },
        TodoItem { text: "ship".into(), status: TodoStatus::Pending },
    ];
    a.handle_agent(AgentEvent::Jobs {
        jobs: vec![JobInfo {
            id: 7,
            kind: JobKind::Shell,
            label: "cargo build".into(),
            status: JobStatus::Running,
            elapsed_ms: 0,
            lines: 0,
        }],
    });
    a.shared = vec![("s1".into(), "the parser".into(), 2100)];
    a.session_id = Some("20261104-1".into());
    a.opts.sessions = vec![("20261103-1".into(), "docs".into())];
    a.status = None;
    a.dirty = true;
    frame(&mut a, 200, 20);
    let hits: Vec<_> = a.hint_hits.clone();
    let find = |h: HintHit| hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r).unwrap_or_else(|| panic!("{h:?}"));
    assert!(!hits.iter().any(|(_, x)| *x == HintHit::Todo), "the strip shows the count, the bar does not repeat it");
    assert!(a.todo_area.height > 0);
    for h in [HintHit::Jobs, HintHit::Mem, HintHit::Queued, HintHit::Session] {
        let r = find(h);
        assert!(!lit_cells(&mut a, r), "{h:?} is plain until the pointer comes");
        pointer(&mut a, K::Moved, r.x + 1, r.y);
        assert_eq!(a.hover, Hover::Hint(h));
        assert!(lit_cells(&mut a, r), "{h:?} is underlined under the pointer");
        pointer(&mut a, K::Moved, 0, 0);
        assert_eq!(a.hover, Hover::Nothing);
    }
    let click = |a: &mut App, h: HintHit| {
        frame(a, 200, 20);
        let r = a.hint_hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r).unwrap();
        pointer(a, K::Down(MouseButton::Left), r.x + 1, r.y);
    };
    frame(&mut a, 200, 20);
    let strip = a.todo_area;
    pointer(&mut a, K::Down(MouseButton::Left), strip.x + 1, strip.y);
    match &a.modal {
        Some(Modal::Todo(v)) => {
            assert_eq!(v.len(), 3);
            assert_eq!(
                v.current().map(|t| t.text.as_str()),
                Some("fix"),
                "the cursor starts on the task being worked on"
            );
        }
        other => panic!("{other:?}"),
    }
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none(), "Esc closes it");
    click(&mut a, HintHit::Jobs);
    assert!(matches!(&a.modal, Some(Modal::Jobs(v)) if v.len() == 1));
    assert!(drain(&mut rx).contains(&AgentCommand::JobTails));
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none());
    click(&mut a, HintHit::Mem);
    assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Shared && p.items.len() == 1));
    press(&mut a, KeyCode::Esc);
    click(&mut a, HintHit::Session);
    assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Session));
    press(&mut a, KeyCode::Esc);
    // the queue: picking a message edits it
    click(&mut a, HintHit::Queued);
    assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Queue && p.items.len() == 2));
    press(&mut a, KeyCode::Down);
    press(&mut a, KeyCode::Enter);
    assert!(a.modal.is_none());
    assert_eq!(a.input.full_text(), "second request");
    assert_eq!(drain(&mut rx), vec![AgentCommand::Unqueue { index: 1, text: "second request".into() }]);
}

#[test]
fn todo_queue_and_memory_lists_answer_to_slash_commands() {
    let (mut a, mut rx) = app();
    assert_eq!(commands::parse("/todo"), Some(Command::Todo));
    assert_eq!(commands::parse("/queue"), Some(Command::Queue));
    assert_eq!(commands::parse("/shared-memory list"), Some(Command::Mem(Some("list".into()))));
    // no tasks: the window says so, and closes
    a.input.set_text("/todo");
    a.submit();
    assert!(matches!(&a.modal, Some(Modal::Todo(v)) if v.is_empty()));
    a.modal = None;
    // nothing to list: a notice, no window
    for c in ["/queue", "/shared-memory list"] {
        a.input.set_text(c);
        a.submit();
        assert!(a.modal.is_none(), "{c}");
    }
    assert!(drain(&mut rx).is_empty(), "listing asks nothing of the agent");
    a.shared = vec![("s1".into(), String::new(), 900)];
    a.input.set_text("/shared-memory list");
    a.submit();
    assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Shared && p.items[0].label == "s1"));
}

#[test]
fn a_dialogs_scrollbar_drags_its_text() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    a.run_command(Command::Help);
    frame(&mut a, 70, 16);
    let bar = a.modal_bar.expect("the help is longer than the dialog");
    assert_eq!(bar.target, scrollbar::Target::Modal);
    pointer(&mut a, K::Moved, bar.x, bar.y);
    assert_eq!(a.hover, Hover::Bar(scrollbar::Target::Modal));
    pointer(&mut a, K::Down(MouseButton::Left), bar.x, bar.y + bar.len - 1);
    pointer(&mut a, K::Drag(MouseButton::Left), bar.x, bar.y + bar.len - 1);
    pointer(&mut a, K::Up(MouseButton::Left), bar.x, bar.y + bar.len - 1);
    assert!(matches!(a.modal, Some(Modal::Help { scroll }) if scroll == bar.max_offset()), "{:?}", a.modal.is_some());
    frame(&mut a, 70, 16);
    assert!(a.modal_bar.unwrap().offset > 0);
    a.modal = None;
    frame(&mut a, 70, 16);
    assert!(a.modal_bar.is_none(), "no stale bar once the dialog is gone");
}

#[test]
fn dialogs_and_the_palette_pop_in_unless_the_animations_are_off() {
    let (mut a, _rx) = app();
    a.run_command(Command::Help);
    frame(&mut a, 70, 16);
    assert!(a.modal_at.is_some() && a.modal_pop() < 1.0, "just opened");
    let r = crate::ui::pop_rect(Rect::new(10, 10, 40, 20), 0.0);
    assert!(r.width < 40 && r.height < 20 && r.x > 10 && r.y > 10);
    assert_eq!(crate::ui::pop_rect(Rect::new(10, 10, 40, 20), 1.0), Rect::new(10, 10, 40, 20));

    let (mut a, _rx) = app();
    a.opts.animations = false;
    a.run_command(Command::Help);
    frame(&mut a, 70, 16);
    assert!(a.modal_at.is_none());
    assert_eq!(a.modal_pop(), 1.0);
    a.run_command(Command::Cost);
    assert!(matches!(&a.modal, Some(Modal::Info(i)) if !i.animating() && i.grow() == 1.0));
}

#[test]
fn a_closing_dialog_melts_away_instead_of_vanishing() {
    let (mut a, _rx) = app();
    let t0 = Instant::now();
    fx_at(&mut a, t0, 0);
    a.run_command(Command::Help);
    frame(&mut a, 70, 20);
    fx_at(&mut a, t0, 1000);
    let boxes = |rows: &[String]| rows.iter().filter(|r| r.contains("╭ help")).count();
    let open = frame(&mut a, 70, 20);
    let base = 0;
    assert!(boxes(&open) > base, "the dialog is up");
    a.modal = None;
    fx_at(&mut a, t0, 1020);
    let closing = frame(&mut a, 70, 20);
    assert!(boxes(&closing) > base, "its first frames of leaving still show it");
    assert!(a.tick_fx(), "frames keep coming while it leaves");
    fx_at(&mut a, t0, 1400);
    let gone = frame(&mut a, 70, 20);
    assert!(boxes(&gone) <= base);
    assert!(!a.tick_fx());
    // with the motion off it closes at once
    let (mut b, _rx) = app();
    b.opts.animations = false;
    b.run_command(Command::Help);
    let up = frame(&mut b, 70, 20);
    assert!(up.iter().any(|r| r.contains("╭ help")));
    b.modal = None;
    assert!(!frame(&mut b, 70, 20).iter().any(|r| r.contains("╭ help")));
}

#[test]
fn the_palette_highlight_glides_to_the_next_row() {
    let (mut a, _rx) = app();
    let t0 = Instant::now();
    fx_at(&mut a, t0, 0);
    a.input.set_text("/");
    a.refresh_palette();
    frame(&mut a, 90, 24);
    fx_at(&mut a, t0, 1000);
    frame(&mut a, 90, 24);
    let rows = a.palette_rows.clone();
    assert!(rows.len() > 2, "palette rows: {}", rows.len());
    let bg = |buf: &ratatui::buffer::Buffer, i: usize| buf[(rows[i].0.x + 3, rows[i].0.y)].bg;
    let settled = buf_of(&mut a, 90, 24);
    let sel_bg = bg(&settled, 0);
    a.palette.as_mut().unwrap().selected = 1;
    let first = buf_of(&mut a, 90, 24);
    assert_ne!(bg(&first, 1), sel_bg, "the new row is not lit all at once");
    assert_eq!(bg(&first, 0), sel_bg, "the old row has not gone dark yet");
    assert!(a.tick_fx());
    fx_at(&mut a, t0, 1300);
    let done = buf_of(&mut a, 90, 24);
    assert_eq!(bg(&done, 1), sel_bg);
    assert_ne!(bg(&done, 0), sel_bg);
}

#[test]
fn a_queued_request_fades_in_and_the_keys_row_crossfades() {
    let (mut a, _rx) = app();
    let t0 = Instant::now();
    fx_at(&mut a, t0, 0);
    frame(&mut a, 100, 20);
    fx_at(&mut a, t0, 1000);
    frame(&mut a, 100, 20);
    a.queued_msgs.push("then do the other thing".into());
    a.queued = 1;
    let find = |buf: &ratatui::buffer::Buffer| {
        let y = (0..buf.area.height).find(|y| (0..buf.area.width).any(|x| buf[(x, *y)].symbol() == "◷")).unwrap();
        let x = (0..buf.area.width).find(|x| buf[(*x, y)].symbol() == "t").unwrap();
        buf[(x, y)].fg
    };
    let first = find(&buf_of(&mut a, 100, 20));
    fx_at(&mut a, t0, 1600);
    let later = find(&buf_of(&mut a, 100, 20));
    assert_ne!(first, later, "the row's text comes up");
    // the keys change with the state: a busy turn fades its row in
    a.busy = true;
    let k0 = buf_of(&mut a, 100, 20);
    fx_at(&mut a, t0, 2200);
    let k1 = buf_of(&mut a, 100, 20);
    let y = k0.area.height - 2;
    assert_ne!(k0[(2, y)].fg, k1[(2, y)].fg);
}

#[test]
fn far_blocks_lose_their_lines_but_keep_their_height() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    for i in 0..3000 {
        a.push(Block::User { text: format!("message number {i}"), at: None });
    }
    a.show_reasoning = true;
    screen(&mut a, 80, 24);
    let total = a.total_lines;
    assert!(a.follow, "the view is at the end");
    let at_end = screen(&mut a, 80, 24);
    let kept = cached(&a);
    assert!(kept < 24 * 8, "{kept} blocks keep their lines");
    assert_eq!(a.total_lines, total, "the heights are kept");
    assert_eq!(a.block_at_line(3), Some(1), "a hit-test needs no lines");
    a.scroll_to_top();
    a.follow = false;
    let top = screen(&mut a, 80, 24);
    assert!(top.iter().any(|r| r.contains("message number 0")), "{top:?}");
    assert!(cached(&a) < 24 * 8);
    a.scroll_to_end();
    assert_eq!(screen(&mut a, 80, 24), at_end, "the end draws as before");
}

#[test]
fn without_animations_nothing_is_born_or_opening() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    for i in 0..3 {
        a.push(Block::User { text: format!("m{i}"), at: None });
    }
    screen(&mut a, 80, 24);
    assert!(a.looks.iter().all(|l| l.born.is_none() && l.opened.is_none()));
    assert_eq!(a.fade(0), 1.0);
}

#[test]
fn an_unknown_tool_shows_key_value_pairs_not_json() {
    let args = serde_json::json!({ "action": "output", "id": 3, "wait": 5 });
    assert_eq!(summarise("jobs", &args), "action output, id 3, wait 5");
}

#[test]
fn a_replayed_message_with_a_note_memory_and_text_becomes_cards_and_a_bubble() {
    let (mut a, _rx) = app();
    let base = a.blocks.len();
    let note = "[background jobs ended]\n- job 3 `make` done (exit 0) after 4s, 2 lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id).";
    let mem = "<memory from=\"s-1\" title=\"parser\">\nShared.\n</memory>";
    a.history(0, Message::user(format!("{mem}\n\n{note}\n\nfix it")));
    a.history(1, Message::user("[summary] The conversation before this point was replaced\n[files]"));
    assert!(matches!(&a.blocks[base], Block::User { text, .. } if crate::render::blocks::is_job_note(text)));
    assert!(matches!(&a.blocks[base + 1], Block::User { text, .. } if text == "fix it"));
    // the memory is a folded line under the words, not a card of its own
    assert!(
        matches!(&a.blocks[base + 2], Block::Added { parts, folded: true } if parts.len() == 1 && parts[0].label == "memory from parser")
    );
    assert!(
        matches!(&a.blocks[base + 3], Block::Notice { text, .. } if text.starts_with("earlier conversation summarised"))
    );
}

#[test]
fn a_long_call_summary_cuts_non_ascii_json_on_a_character_boundary() {
    // byte 79 falls inside a two-byte "ñ"
    let args = serde_json::json!({ "x": format!("{}{}", "a".repeat(72), "ñ".repeat(10)) });
    let s = summarise("other", &args);
    assert!(s.ends_with('…'), "{s}");
    assert!(crate::render::wrap::width(&s) <= 80, "{s}");
}

#[test]
fn ctrl_c_with_the_palette_open_clears_the_draft_and_the_palette() {
    let (mut a, _rx) = app();
    for c in "/m".chars() {
        crate::event::key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(a.palette.is_some());
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(a.input.is_empty());
    assert!(a.palette.is_none(), "no list left over an empty prompt");
}

#[test]
fn ctrl_c_in_a_permission_dialog_denies_it_and_stops_the_turn() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::PermissionAsk {
        request: PermissionRequest {
            agent: None,
            id: "p1".into(),
            tool: "bash".into(),
            kind: nexo_core::tool::ToolKind::Exec,
            summary: "x".into(),
            detail: ToolDisplay::Plain,
            suggested_rules: vec![],
        },
    });
    while rx.try_recv().is_ok() {}
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(a.modal.is_none());
    let sent: Vec<AgentCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        matches!(&sent[..], [AgentCommand::Permission { id, decision: PermissionDecision::Deny { .. } }, AgentCommand::Interrupt] if id == "p1"),
        "{sent:?}"
    );
    // a second press quits, as at the prompt
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(a.should_quit);
}

#[test]
fn a_list_at_rest_shows_the_row_the_arrow_reached_without_another_key() {
    let (mut a, _rx) = app();
    a.opts.animations = true;
    // the stars of the empty session behind the list rest later (see `bursts.rs`)
    a.opts.home_stars = false;
    a.opts.sessions = (0..5).map(|i| (format!("s{i}"), format!("chat {i}"))).collect();
    a.run_command(Command::Sessions);
    let t0 = Instant::now();
    fx_at(&mut a, t0, 0);
    frame(&mut a, 70, 16);
    fx_at(&mut a, t0, 20_000);
    a.tick_frame(false);
    assert_eq!(a.load(), crate::frame::Load::Idle, "the open list rests");
    press(&mut a, KeyCode::Down);
    frame(&mut a, 70, 16);
    assert_eq!(a.load(), crate::frame::Load::Motion, "the glide begun in the frame asks for the next ones");
    fx_at(&mut a, t0, 20_000 + crate::motion::GLIDE_MS + 50);
    a.tick_frame(false);
    assert!(a.dirty, "a frame comes after the glide ends");
    frame(&mut a, 70, 16);
    let Some(Modal::Picker(p)) = &a.modal else { panic!() };
    assert_eq!(a.fx.pick_sel.level(p.selected, a.now()), 1.0, "the row reached is fully lit");
}

#[test]
fn mcp_prompts_become_commands_and_send_the_server_prompt_and_arguments() {
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::Mcp { servers: vec![docs_server()] });
    a.input.set_text("/mcp__docs__rev");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("mcp__docs__review"));
    a.input.set_text("/mcp__docs__review src/lib.rs focus=speed");
    a.submit();
    match rx.try_recv() {
        Ok(AgentCommand::McpPrompt { server, prompt, args }) => {
            assert_eq!((server.as_str(), prompt.as_str(), args.as_str()), ("docs", "review", "src/lib.rs focus=speed"));
        }
        other => panic!("{other:?}"),
    }
    // the server goes away: the command goes with it
    a.handle_agent(AgentEvent::Mcp { servers: vec![] });
    assert!(a.custom_commands.iter().all(|c| c.origin != "mcp"));
}

#[test]
fn at_server_colon_completes_resource_uris() {
    let (mut a, _rx) = app();
    a.handle_agent(AgentEvent::Mcp { servers: vec![docs_server()] });
    a.input.set_text("read @docs:db");
    a.refresh_palette();
    assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("docs:db://users"));
    a.palette_accept(false);
    assert!(a.input.text().starts_with("read @docs:db://users"), "{}", a.input.text());
}

#[test]
fn the_mcp_window_lists_servers_and_its_keys_send_commands() {
    use crate::modal::Modal;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nexo_core::agent::{McpServerInfo, McpStatus};
    let down = McpServerInfo {
        name: "broken".into(),
        transport: "stdio".into(),
        status: McpStatus::Failed("spawn failed: no such file".into()),
        tools: 0,
        resources: vec![],
        templates: vec![],
        prompts: vec![],
    };
    let login = McpServerInfo { name: "cloud".into(), status: McpStatus::NeedsLogin, ..down.clone() };
    let (mut a, mut rx) = app();
    a.handle_agent(AgentEvent::Mcp { servers: vec![docs_server(), login, down] });
    a.run_command(crate::commands::Command::Mcp);
    assert!(matches!(a.modal, Some(Modal::Mcp(_))));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowMcp)));
    a.opts.animations = false;
    frame(&mut a, 100, 44);
    let text = frame(&mut a, 100, 44);
    let all = text.join("\n");
    for want in [
        "1/3 connected",
        "docs",
        "http",
        "4 tools · 2 resources · 1 prompt",
        "/mcp__docs__review",
        "needs login",
        "press l to sign in",
        "failed",
        "spawn failed: no such file",
    ] {
        assert!(all.contains(want), "{want} missing:\n{all}");
    }
    let press = |a: &mut App, c: char| crate::event::key(a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    press(&mut a, ' ');
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::McpToggle { server, on: false, persist: false }) if server == "docs")
    );
    press(&mut a, 'j');
    press(&mut a, 'l');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpLogin { server }) if server == "cloud"));
    press(&mut a, 'j');
    press(&mut a, 'r');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpReconnect { server }) if server == "broken"));
    press(&mut a, 'p');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpToggle { on: false, persist: true, .. })));
    // a server that is off is turned on by the same key
    let mut off = docs_server();
    off.status = McpStatus::Disabled;
    a.handle_agent(AgentEvent::Mcp { servers: vec![off] });
    let all = frame(&mut a, 100, 44).join("\n");
    assert!(all.contains("off") && all.contains("Space turns it on"), "{all}");
    press(&mut a, ' ');
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::McpToggle { on: true, .. })));
    press(&mut a, 'q');
    assert!(a.modal.is_none());
}

#[test]
fn a_message_starting_with_a_path_is_sent_not_taken_for_a_command() {
    for line in ["/home/u/foo.rs has a bug", "/tmp/x.log", "/etc/hosts.d", "/foo.rs is broken", "/c\\temp\\x"] {
        let (mut a, mut rx) = app();
        a.input.set_text(line);
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == line), "{line}");
        assert!(a.input.is_empty(), "{line}");
    }
}

#[test]
fn an_unknown_command_warns_and_keeps_the_draft() {
    let (mut a, mut rx) = app();
    a.input.set_text("/foo bar");
    a.submit();
    assert!(rx.try_recv().is_err());
    assert_eq!(a.input.full_text(), "/foo bar");
    assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
}

#[test]
fn enter_on_an_empty_draft_with_a_queued_request_sends_it_now() {
    let (mut a, mut rx) = app();
    a.busy = true;
    a.queued = 1;
    a.submit();
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::InterruptRun)));
    assert!(rx.try_recv().is_err(), "one command");
    let said = a.status.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
    assert!(said.contains("queued request now"), "{said}");
}

#[test]
fn the_branch_keeps_its_slashes_and_a_windows_cwd_shows_its_last_folder() {
    let dir = std::env::temp_dir().join(format!("nexo-branch-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/feature/login\n").unwrap();
    assert_eq!(git_branch(&dir).as_deref(), Some("feature/login"));
    std::fs::remove_dir_all(&dir).ok();

    let (mut a, _rx) = app();
    a.peers = vec![nexo_core::mail::PeerInfo {
        id: "bbbb2222".into(),
        name: "api".into(),
        cwd: "C:\\Users\\dev\\api".into(),
        pid: 1,
        external: false,
        bypass: false,
        sendable: true,
        me: false,
    }];
    assert_eq!(a.peer_values(), vec![("api".to_string(), "api".to_string())]);
}

fn goal_with_log(a: &mut App, state: GoalState) {
    use nexo_core::agent::{GoalLog, GoalStop};
    let now = nexo_core::mailbox::now_ms();
    a.handle_agent(AgentEvent::GoalLog(GoalLog {
        original: Some("tests green pls".into()),
        brief: Some(
            "Want: every test passes\nWhy: the release is blocked\nHow: fix the code\nDone when: 0 failed".into(),
        ),
        started_ms: now - 125_000,
        steps: 7,
        cost_usd: 0.0123,
        stops: vec![GoalStop { at_ms: now - 60_000, why: "interrupted".into() }],
        last: Some("Fixed the parser".into()),
    }));
    a.handle_agent(AgentEvent::Goal {
        text: Some(
            "Want: every test passes\nWhy: the release is blocked\nHow: fix the code\nDone when: 0 failed".into(),
        ),
        round: 2,
        max: 5,
        state,
    });
}

#[test]
fn a_click_on_the_goal_opens_its_window_and_esc_closes_it() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, _rx) = app();
    a.opts.animations = false;
    goal_with_log(&mut a, GoalState::Active);
    frame(&mut a, 120, 50);
    let goal =
        a.status_hits.iter().find(|(_, h)| *h == StatusHit::Goal).map(|(r, _)| *r).expect("the goal is on the bar");
    pointer(&mut a, K::Moved, goal.x, goal.y);
    assert_eq!(a.hover, Hover::Status(StatusHit::Goal));
    pointer(&mut a, K::Down(MouseButton::Left), goal.x + 1, goal.y);
    assert!(matches!(a.modal, Some(Modal::Goal(_))), "the goal opens its window, not the context");
    let all = frame(&mut a, 120, 50).join("\n");
    for want in [
        "◎ goal",
        "round 2/5",
        "running 2 min",
        "7 steps",
        "you asked",
        "tests green pls",
        "restated",
        "Want: every test passes",
        "Fixed the parser",
        "interruptions (1)",
        "1 min ago · interrupted",
    ] {
        assert!(all.contains(want), "missing {want:?} in:\n{all}");
    }
    assert!(!all.contains("Resume"), "a goal at work has nothing to resume:\n{all}");
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none(), "Esc closes it");
    // with no goal there is nothing to click
    a.handle_agent(AgentEvent::Goal { text: None, round: 0, max: 5, state: Default::default() });
    frame(&mut a, 120, 50);
    assert!(a.status_hits.iter().all(|(_, h)| *h != StatusHit::Goal));
}

#[test]
fn the_goal_window_of_a_paused_goal_resumes_it_with_a_click_or_a_key() {
    use crossterm::event::{MouseButton, MouseEventKind as K};
    let (mut a, mut rx) = app();
    a.opts.animations = false;
    goal_with_log(&mut a, GoalState::Paused("interrupted".into()));
    frame(&mut a, 120, 50);
    a.open_goal();
    let all = frame(&mut a, 120, 50).join("\n");
    assert!(all.contains("paused: interrupted") && all.contains("Resume"), "{all}");
    // the button is the row the pointer was given
    let button = a.picker_rows.first().map(|(r, _)| *r).expect("the Resume button is a row");
    pointer(&mut a, K::Down(MouseButton::Left), button.x + 2, button.y);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ResumeGoal)));
    assert!(a.modal.is_none(), "the window closes");
    // the key does the same
    a.open_goal();
    frame(&mut a, 120, 50);
    press(&mut a, KeyCode::Char('r'));
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::ResumeGoal)));
    assert!(a.modal.is_none());
}

#[test]
fn the_goal_window_follows_the_agent_and_closes_when_the_goal_is_cleared() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    goal_with_log(&mut a, GoalState::Active);
    a.open_goal();
    frame(&mut a, 120, 30);
    goal_with_log(&mut a, GoalState::Paused("the turn failed".into()));
    let all = frame(&mut a, 120, 30).join("\n");
    assert!(all.contains("paused: the turn failed") && all.contains("Resume"), "{all}");
    a.handle_agent(AgentEvent::Goal { text: None, round: 0, max: 5, state: Default::default() });
    assert!(a.modal.is_none() && a.goal_log == Default::default());
}

/// A user message and what nexo added to it, as the agent tells them.
fn said_with_additions(a: &mut App) {
    use nexo_core::agent::{AddedKind::*, AddedPart};
    let note = "[project at the start of this request]\nattached: 2 files";
    a.handle_agent(AgentEvent::History {
        index: 0,
        message: nexo_core::message::Message::user(format!("fix the bug\n\n{note}")),
    });
    let file =
        |p: &str| AddedPart::new(File, p, format!("@{p} (attached in full, as it is now):\n```rs\nfn a() {{}}\n```"));
    let long: String = (1..=20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    a.handle_agent(AgentEvent::Added {
        index: 0,
        parts: vec![
            AddedPart::new(Hook, "hook context", "it is monday"),
            AddedPart::new(StartNote, "project", note),
            file("a.rs"),
            AddedPart::new(File, "b.rs", long),
        ],
    });
}

#[test]
fn a_line_under_the_message_says_what_nexo_added_and_opens_to_the_text() {
    for theme in ["dark", "light"] {
        let (mut a, _rx) = app();
        a.opts.animations = false;
        a.set_theme(theme, false);
        said_with_additions(&mut a);
        let all = frame(&mut a, 100, 40).join("\n");
        assert!(all.contains("+ nexo added · ") && all.contains("(hook, start note, 2 files)"), "{theme}: {all}");
        assert!(!all.contains("it is monday") && !all.contains("line 5"), "folded: only the line, {theme}: {all}");
        // the start note left the person's block: it is said in the line
        assert!(all.contains("fix the bug") && !all.contains("attached: 2 files"), "{theme}: {all}");
        // Enter on the selected line opens it: each text with its label, a long one cut
        let at = a.blocks.iter().position(|b| matches!(b, Block::Added { .. })).unwrap();
        a.toggle_block(at);
        let all = frame(&mut a, 100, 60).join("\n");
        for want in [
            "- nexo added",
            "hook context · hook",
            "it is monday",
            "project · start note",
            "b.rs · file",
            "line 12",
            "… 8 more lines",
        ] {
            assert!(all.contains(want), "{theme}: {want} in {all}");
        }
        assert!(!all.contains("line 13"), "{theme}: {all}");
        a.toggle_block(at);
        assert!(!frame(&mut a, 100, 40).join("\n").contains("it is monday"));
    }
}

#[test]
fn a_message_with_nothing_added_has_no_line_and_a_supervisor_note_says_it_rides_one_request() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    a.handle_agent(AgentEvent::History { index: 0, message: nexo_core::message::Message::user("just words") });
    a.handle_agent(AgentEvent::Added { index: 0, parts: vec![] });
    assert!(!a.blocks.iter().any(|b| matches!(b, Block::Added { .. })));
    assert!(!frame(&mut a, 100, 30).join("\n").contains("nexo added"));
    let mut p =
        nexo_core::agent::AddedPart::new(nexo_core::agent::AddedKind::Supervisor, "supervisor", "check the tests");
    p.request_only = true;
    a.handle_agent(AgentEvent::Added { index: 0, parts: vec![p] });
    let all = frame(&mut a, 100, 30).join("\n");
    assert!(all.contains("(supervisor note) · this request only"), "{all}");
    assert!(all.contains("just words"), "the person's block stays as it was");
}

#[test]
fn sent_asks_the_agent_and_the_answer_opens_a_window_with_the_sections() {
    use nexo_core::agent::SentSection;
    let (mut a, mut rx) = app();
    a.opts.animations = false;
    for cmd in ["/sent", "/request"] {
        a.input.set_text(cmd);
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowSent)), "{cmd}");
    }
    let sec = |h: &str, depth: u8, tag: Option<&str>, text: &str| SentSection {
        heading: h.into(),
        depth,
        tag: tag.map(str::to_string),
        tokens: 12,
        text: text.into(),
        collapse: false,
    };
    a.handle_agent(AgentEvent::Sent {
        model: "m1".into(),
        sections: vec![
            sec("System prompt", 0, None, ""),
            sec("instruction files", 1, None, "Use tabs."),
            sec("#1 user", 0, None, "fix it"),
            sec("#1 user · NUDGE", 0, Some("curator"), "[curator] Before the next step, note it"),
        ],
    });
    assert!(matches!(a.modal, Some(Modal::Sent(_))));
    let all = frame(&mut a, 120, 40).join("\n");
    for want in [
        "request · m1 · ~",
        "System prompt",
        "instruction files",
        "Use tabs.",
        "#1 user · NUDGE",
        "[curator]",
        "search",
    ] {
        assert!(all.contains(want), "{want} in {all}");
    }
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none());
}

/// A resumed session's message as it was stored: the words, the start note and a file, all in one text.
fn stored_with_additions(a: &mut App) {
    let t = "Hola cómo estás?\n\n[project at the start of this request]\nattached: 1 files, 1883 tokens\n\n\
             @1-builtin-instructions.md (attached in full, as it is now):\n```md\nuno\ndos\ntres\n```";
    a.handle_agent(AgentEvent::History { index: 0, message: nexo_core::message::Message::user(t) });
}

#[test]
fn the_view_shows_only_what_the_person_typed_until_the_additions_are_opened() {
    for theme in ["dark", "light"] {
        // live: the words as typed, what nexo added told as it was built
        let (mut live, _rx) = app();
        live.opts.animations = false;
        live.set_theme(theme, false);
        said_with_additions(&mut live);
        // resumed: the stored message holds all of it in one text, split at its markers
        let (mut old, _rx) = app();
        old.opts.animations = false;
        old.set_theme(theme, false);
        stored_with_additions(&mut old);
        for (what, a) in [("live", &mut live), ("resumed", &mut old)] {
            let all = frame(a, 100, 40).join("\n");
            assert!(all.contains("+ nexo added · ") && all.contains("start note"), "{what} {theme}: {all}");
            for hidden in ["project at the start", "attached: ", "builtin-instructions", "uno"] {
                assert!(!all.contains(hidden), "{what} {theme}: {hidden:?} shows by default: {all}");
            }
            let words = if what == "live" { "fix the bug" } else { "Hola cómo estás?" };
            let Some(Block::User { text, .. }) = a.blocks.iter().find(|b| matches!(b, Block::User { .. })) else {
                panic!("no message")
            };
            assert_eq!(text, words, "{what} {theme}: the message holds only the typed words");
        }
        let all = frame(&mut old, 100, 40).join("\n");
        assert!(all.contains("(start note, file)"), "{theme}: {all}");
    }
}

#[test]
fn right_opens_what_was_added_to_the_selected_message_and_left_folds_it_back() {
    for theme in ["dark", "light"] {
        let (mut a, _rx) = app();
        a.opts.animations = false;
        a.set_theme(theme, false);
        stored_with_additions(&mut a);
        frame(&mut a, 100, 40);
        let user = a.blocks.iter().position(|b| matches!(b, Block::User { .. })).unwrap();
        // a one-line message with something added can be selected: it has a line to open
        a.selected = Some(user);
        press(&mut a, KeyCode::Right);
        let all = frame(&mut a, 100, 60).join("\n");
        for want in [
            "- nexo added",
            "project at the start of the request · start note",
            "attached: 1 files, 1883 tokens",
            "1-builtin-instructions.md · file",
            "uno",
        ] {
            assert!(all.contains(want), "{theme}: {want} in {all}");
        }
        assert_eq!(a.selected, Some(user), "the message stays selected");
        press(&mut a, KeyCode::Right);
        assert!(
            frame(&mut a, 100, 60).join("\n").contains("attached: 1 files, 1883 tokens"),
            "→ on an open one keeps it"
        );
        press(&mut a, KeyCode::Left);
        let all = frame(&mut a, 100, 60).join("\n");
        assert!(all.contains("+ nexo added") && !all.contains("attached: 1 files") && !all.contains("uno"), "{all}");
        assert!(all.contains("Hola cómo estás?"));
    }
}

#[test]
fn a_double_click_on_the_message_opens_what_was_added_and_another_folds_it() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    stored_with_additions(&mut a);
    a.transcript_area = Rect::new(0, 1, 100, 30);
    a.layout(100);
    a.settle(30);
    let user = a.blocks.iter().position(|b| matches!(b, Block::User { .. })).unwrap();
    let added = a.companion(user).expect("a line under the message");
    let row = |a: &App, i: usize| (a.offsets[i] - a.scroll) as u16 + 1;
    let open = |a: &App| matches!(a.blocks[added], Block::Added { folded: false, .. });
    // one click only selects
    let r = row(&a, user);
    a.press_at(4, r);
    a.release_at(4, r);
    assert_eq!(a.selected, Some(user));
    assert!(!open(&a), "one click only selects");
    // the second, at once, opens it, and the word under it is not what it selects
    a.press_at(4, r);
    a.release_at(4, r);
    assert!(open(&a));
    assert!(a.selection.is_none());
    a.layout(100);
    a.settle(30);
    // a double click on the line of the additions folds it back
    std::thread::sleep(std::time::Duration::from_millis(450));
    let r = row(&a, added);
    a.press_at(4, r);
    a.release_at(4, r);
    assert!(open(&a), "a single click leaves it as it is");
    a.press_at(4, r);
    a.release_at(4, r);
    assert!(!open(&a));
}

#[test]
fn a_new_model_or_mode_in_the_status_bar_lights_up_once_and_the_first_look_does_not() {
    let (mut a, _rx) = app();
    let t0 = std::time::Instant::now();
    a.fx.clock = Some(t0);
    assert!(!a.tick_fx(), "the first look at the bar is not a change");
    let model_fg = |a: &App| {
        let segs = crate::ui::title_segments(a, 200);
        segs.iter().flatten().find(|s| s.text.contains("mock")).and_then(|s| s.style.fg).unwrap()
    };
    let own = model_fg(&a);
    a.model = "n".into();
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(10));
    assert!(a.tick_fx(), "a change asks for frames while it fades");
    assert_ne!(model_fg(&a), own, "the new model is lit");
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(crate::motion::CUT_MS + 300));
    assert!(!a.tick_fx(), "and the frames stop when it has settled");
    assert_eq!(model_fg(&a), own);
    // with the motion off nothing lights
    a.opts.animations = false;
    a.permission_mode = nexo_core::permission::PermissionMode::Plan;
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(crate::motion::CUT_MS + 400));
    assert!(!a.tick_fx());
}

#[test]
fn a_model_saved_from_the_setup_leaves_it_open_on_its_last_screen_until_the_agent_confirms() {
    use crate::modal::setup::{Setup, Step};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nexo_core::{ModelCheck, ModelInfo, ProviderConfig, ServerProbe};
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (mut a, mut rx) = app();
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_APP_SETUP_DS"));
    ds.default_model = Some("m1".into());
    let mut s = Setup::new(vec![("deepseek".into(), ds)], Vec::new(), "");
    let press = |s: &mut Setup, code: KeyCode| s.key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut s, KeyCode::Enter);
    for c in "sk-made-up-key-0000".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(ServerProbe {
        protocol: "openai-chat".into(),
        base_url: "https://api.deepseek.com".into(),
        models: vec![ModelInfo { id: "m1".into(), ..Default::default() }],
        ..Default::default()
    }));
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck { info: ModelInfo { id: "m1".into(), ..Default::default() }, ..Default::default() });
    a.modal = Some(Modal::Setup(Box::new(s)));
    let out = a.modal.as_mut().unwrap().key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    a.modal_out(out);
    // sent, not yet offered by the switcher (only a save that went through is), and the setup is still there
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::SaveModel { ref name, ref model, .. }) if name == "deepseek" && model == "m1")
    );
    assert!(!a.opts.models.contains(&("deepseek".to_string(), "m1".to_string())));
    let Some(Modal::Setup(st)) = &a.modal else { panic!("the setup stays open") };
    assert_eq!(st.step, Step::Review);
    // another model changing is not our save
    a.handle_agent(AgentEvent::ModelChanged { provider: "other".into(), model: "x".into() });
    let Some(Modal::Setup(st)) = &a.modal else { panic!("still open") };
    assert_eq!(st.step, Step::Review);
    // the agent's word: the last screen
    a.handle_agent(AgentEvent::ModelChanged { provider: "deepseek".into(), model: "m1".into() });
    let Some(Modal::Setup(st)) = &a.modal else { panic!("the last screen") };
    assert_eq!(st.step, Step::Done);
    assert_eq!((a.provider.as_str(), a.model.as_str()), ("deepseek", "m1"));
    assert!(a.opts.models.contains(&("deepseek".to_string(), "m1".to_string())), "offered once saved");
}

/// The setup walked to the point where Enter on the review has sent `SaveModel` (the setup still open).
fn saving_setup() -> (App, tokio::sync::mpsc::UnboundedReceiver<AgentCommand>) {
    use crate::modal::setup::Setup;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nexo_core::{ModelCheck, ModelInfo, ProviderConfig, ServerProbe};
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (mut a, mut rx) = app();
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_APP_SETUP_DS"));
    ds.default_model = Some("m1".into());
    let mut s = Setup::new(vec![("deepseek".into(), ds)], Vec::new(), "");
    let press = |s: &mut Setup, code: KeyCode| s.key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut s, KeyCode::Enter);
    for c in "sk-made-up-key-0000".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(ServerProbe {
        protocol: "openai-chat".into(),
        base_url: "https://api.deepseek.com".into(),
        models: vec![ModelInfo { id: "m1".into(), ..Default::default() }],
        ..Default::default()
    }));
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck { info: ModelInfo { id: "m1".into(), ..Default::default() }, ..Default::default() });
    a.modal = Some(Modal::Setup(Box::new(s)));
    let out = a.modal.as_mut().unwrap().key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    a.modal_out(out);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SaveModel { .. })));
    (a, rx)
}

fn setup_text(a: &App) -> String {
    let Some(Modal::Setup(s)) = &a.modal else { panic!("the setup is open") };
    s.lines(100, &crate::theme::Theme::nexo(), 0, false)
        .0
        .iter()
        .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>() + "\n")
        .collect()
}

#[test]
fn a_failed_save_told_by_the_agent_keeps_the_open_setup_on_its_step() {
    use crate::modal::setup::Step;
    let (mut a, _rx) = saving_setup();
    a.handle_agent(AgentEvent::SaveFailed {
        name: "deepseek".into(),
        model: "m1".into(),
        reason: "Not saved: config.toml could not be written. Make it writable, then press Enter.".into(),
    });
    let Some(Modal::Setup(s)) = &a.modal else { panic!("still open") };
    assert_eq!(s.step, Step::Review);
    let t = setup_text(&a);
    assert!(t.contains("Not saved") && !t.contains("is ready"), "{t}");
    // nothing else is told: the wizard said it
    assert!(!matches!(a.blocks.last(), Some(Block::Notice { level: Level::Warn, .. })));
}

#[test]
fn a_failed_save_whose_wizard_was_closed_still_shows_one_warning() {
    let (mut a, _rx) = saving_setup();
    a.modal = None;
    let before = a.blocks.len();
    a.handle_agent(AgentEvent::SaveFailed {
        name: "deepseek".into(),
        model: "m1".into(),
        reason: "Not saved: config.toml could not be written. Make it writable, then press Enter.".into(),
    });
    assert_eq!(a.blocks.len(), before + 1);
    assert!(
        matches!(a.blocks.last(), Some(Block::Notice { level: Level::Warn, text }) if text.starts_with("Not saved")),
        "{:?}",
        a.blocks.last()
    );
}

#[test]
fn the_last_screen_says_only_you_can_read_it_where_the_file_mode_is_set() {
    use crate::modal::setup::Step;
    let (mut a, _rx) = saving_setup();
    a.handle_agent(AgentEvent::ModelChanged { provider: "deepseek".into(), model: "m1".into() });
    let Some(Modal::Setup(s)) = &a.modal else { panic!("the last screen") };
    assert_eq!(s.step, Step::Done);
    let t = setup_text(&a);
    assert_eq!(t.contains("only you can read it"), cfg!(unix), "{t}");
    assert!(t.contains("key is in") || t.contains("Key is in") || t.contains("The key is in"), "{t}");
}

#[test]
fn opening_a_block_while_scrolled_up_is_not_news() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    long_answer(&mut a, 0);
    a.push(card("c1", 30));
    long_answer(&mut a, 100);
    frame(&mut a, 60, 20);
    a.toggle_block(1);
    frame(&mut a, 60, 20);
    a.scroll_to_line(0, false);
    frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 0);
    {
        let before = a.total_lines;
        a.toggle_block(1);
        frame(&mut a, 60, 20);
        assert_ne!(a.total_lines, before, "the fold changed the height");
        assert!(!a.follow);
        assert_eq!(a.unseen(), 0, "the rows a fold shows were not sent by anyone");
    }
    long_answer(&mut a, 200);
    frame(&mut a, 60, 20);
    assert!(a.unseen() > 0, "a real arrival still counts");
}
