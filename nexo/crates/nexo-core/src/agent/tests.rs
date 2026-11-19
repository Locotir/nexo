use super::*;
use crate::subs::SubItem;

#[test]
fn event_send_returns_unsent_event_when_receiver_is_closed() {
    let (handle, ends) = channels();
    drop(handle);
    let error = ends.events.send(AgentEvent::TextDelta { text: "unsent".into() }).unwrap_err();
    assert!(matches!(error.0, AgentEvent::TextDelta { text } if text == "unsent"));
}

#[test]
fn a_line_that_heads_a_file_gives_its_path_and_the_two_headers_never_match_each_other() {
    assert_eq!(headed_file("@src/a b.rs (attached in full, as it is now):"), Some("src/a b.rs"));
    let own = "src/lib.rs (project file, included in full by nexo, as it is now):";
    assert_eq!(headed_file(own), Some("src/lib.rs"));
    assert!(!own.starts_with('@') && !own.ends_with(ATTACHED) && !ATTACHED.ends_with(INCLUDED));
    // the person's header needs its `@`
    assert_eq!(headed_file("src/lib.rs (attached in full, as it is now):"), None);
    assert_eq!(headed_file("@shot.png: attached as an image"), None);
    assert_eq!(headed_file(" (project file, included in full by nexo, as it is now):"), None);
    assert_eq!(headed_file("fn main() {"), None);
}

#[test]
fn sub_events_are_marked_and_the_others_are_not() {
    let item = SubItem::Task { text: "t".into() };
    let subs = [
        AgentEvent::Sub { id: "a#1".into(), item },
        AgentEvent::SubDelta { id: "a#1".into(), text: "x".into() },
        AgentEvent::SubNotDelivered { id: "a#1".into(), why: "it ended".into(), text: "message".into() },
        AgentEvent::FlowRun {
            info: crate::subs::FlowInfo {
                id: "wf#1".into(),
                name: "w".into(),
                started_ms: 0,
                ended_ms: None,
                state: crate::subs::FlowState::Running,
                phases: vec![],
            },
        },
    ];
    assert!(subs.iter().all(AgentEvent::is_sub));
    assert!(!AgentEvent::Shutdown.is_sub());
    assert!(!AgentEvent::Jobs { jobs: vec![] }.is_sub());
}

#[test]
fn a_goal_mark_is_found_at_the_start_or_after_the_notes_before_it() {
    let at = goal_mark_at;
    assert_eq!(at("[goal set] ship it"), Some(0));
    assert_eq!(at("[goal: continue]"), Some(0));
    let notes = "[background jobs ended]\njob 1 done\n\n";
    assert_eq!(at(&format!("{notes}[goal: continue]\n\nnote")), Some(notes.len()));
    assert_eq!(at(&format!("[stop line]\n\n{notes}[goal set] ship it")), Some("[stop line]\n\n".len() + notes.len()));
    // a mark inside a line, or one the person quotes after a single newline, is not the loop's
    assert_eq!(at("please add [goal set] to the docs"), None);
    assert_eq!(at("see\n[goal: continue]"), None);
    assert_eq!(at("[goal: met, not running; /goal resume reopens it]\n\nthanks"), None);
    assert_eq!(at(""), None);
}

#[test]
fn a_goal_line_is_cut_only_where_the_goal_window_holds_the_rest() {
    let long = format!("make it fast {}", "and keep every test green ".repeat(12));
    let one_line = long.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(one_line.chars().count() > GOAL_SHOWN);
    let cut = goal_shown(&long, true);
    assert!(cut.ends_with("· the rest is in the goal window") && cut.chars().count() < GOAL_SHOWN + 60, "{cut}");
    // words the window does not hold stay whole, on one line
    assert_eq!(goal_shown(&long, false), one_line);
    assert_eq!(goal_shown("make it\nfast", false), "make it fast");
    assert_eq!(goal_shown("make it\nfast", true), "make it fast");
}

#[test]
fn a_goal_logs_progress_is_what_is_saved_and_comes_back_with_the_words_untouched() {
    let stop = GoalStop { at_ms: 7, why: "Esc".into() };
    let log = GoalLog {
        original: Some("ship it".into()),
        brief: Some("Want: ship".into()),
        started_ms: 5,
        steps: 3,
        cost_usd: 1.5,
        stops: vec![stop.clone()],
        last: Some("on it".into()),
    };
    let p = log.progress();
    assert_eq!(p, GoalProgress { started_ms: 5, steps: 3, stops: vec![stop] });
    let back = GoalLog { original: Some("ship it".into()), ..GoalLog::default() }.with_progress(p.clone());
    assert_eq!((back.original.as_deref(), back.started_ms, back.steps, back.stops.len()), (Some("ship it"), 5, 3, 1));
    assert_eq!(back.progress(), p);
    // a saved progress with fields missing reads as the default
    assert_eq!(serde_json::from_str::<GoalProgress>("{}").unwrap(), GoalProgress::default());
}
