//! A left click outside a window closes it as Esc would; a decision dialog stays.

use super::*;
use crate::modal::{Modal, PickItem, PickKind, Picker};
use ratatui::layout::Rect;

fn item(v: &str) -> PickItem {
    PickItem { label: v.into(), value: v.into(), hint: String::new() }
}

fn picker(kind: PickKind) -> Modal {
    Modal::Picker(Picker::new(kind, "T", vec![item("a"), item("b")], None))
}

fn click(app: &mut App, x: u16, y: u16) {
    crate::event::mouse(
        app,
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn a_click_outside_every_window_closes_it_and_a_decision_dialog_stays() {
    let goal = crate::app::Goal { text: "g".into(), round: 1, max: 3, state: nexo_core::agent::GoalState::Active };
    let req = nexo_core::permission::PermissionRequest {
        id: "1".into(),
        tool: "bash".into(),
        kind: nexo_core::tool::ToolKind::Exec,
        summary: "ls".into(),
        detail: nexo_core::tool::ToolDisplay::Plain,
        suggested_rules: vec![],
    };
    let cases: Vec<(&str, Modal, bool)> = vec![
        ("model", picker(PickKind::Model), true),
        ("effort", picker(PickKind::Effort), true),
        ("resume", picker(PickKind::Session), true),
        ("mode", picker(PickKind::PermissionMode), true),
        ("theme", picker(PickKind::Theme), true),
        ("help", Modal::Help { scroll: 0 }, true),
        ("usage", Modal::Info(Box::new(crate::overlay::Info::new(1))), true),
        ("jobs", Modal::Jobs(Box::new(crate::jobs_view::JobsView::new(vec![]))), true),
        ("mcp", Modal::Mcp(Box::new(crate::mcp_view::McpView::new(vec![]))), true),
        ("todo", Modal::Todo(Box::new(crate::todo_view::TodoView::new(vec![]))), true),
        ("sent", Modal::Sent(Box::new(crate::sent_view::SentView::new("m".into(), vec![]))), true),
        ("goal", Modal::Goal(Box::new(crate::goal_view::GoalView::new(goal, Default::default()))), true),
        ("agents", Modal::Agents(Box::new(crate::agents_view::AgentsView::with_rows(vec![], 0, true))), true),
        ("trust", Modal::Trust(crate::modal::Trust::new("/p".into(), "x".into(), None)), false),
        ("permission", Modal::permission(req), false),
    ];
    for (name, modal, closes) in cases {
        let (mut a, _rx) = app();
        a.modal = Some(modal);
        a.dialog_size.modal.drawn = Rect::new(20, 5, 30, 10);
        // inside: the window stays (a row click may act, but nothing closes on empty ground)
        assert!(!a.modal_outside_click(25, 8), "{name}: inside is not outside");
        click(&mut a, 0, 0);
        assert_eq!(a.modal.is_none(), closes, "{name}: click outside");
    }
}

#[test]
fn a_click_off_both_panes_of_the_diff_view_closes_it() {
    let (mut a, _rx) = app();
    let mut v = crate::diffview::DiffView::from_collected(Default::default());
    v.list_area = Rect::new(0, 1, 20, 10);
    v.body_area = Rect::new(20, 1, 40, 10);
    a.diffview = Some(v);
    click(&mut a, 5, 3);
    assert!(a.diffview.is_some(), "a click on the list stays");
    click(&mut a, 30, 3);
    assert!(a.diffview.is_some(), "a click on the body stays");
    click(&mut a, 70, 20);
    assert!(a.diffview.is_none(), "a click outside both panes closes it");
}
