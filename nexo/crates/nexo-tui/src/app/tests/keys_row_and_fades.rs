//! Queued messages, the keys row, dialog scrollbars and fade animations.

use super::*;

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
