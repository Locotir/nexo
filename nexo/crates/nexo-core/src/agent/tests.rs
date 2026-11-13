use super::*;
use crate::subs::SubItem;

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
        AgentEvent::SubNotDelivered { id: "a#1".into(), why: "it ended".into() },
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
