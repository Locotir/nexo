//! What nexo starts by itself (a resume after a check-in, a goal round, jobs ended, mail) is never drawn as
//! a message of the person's, and never touches the line of what was added to one of theirs.

use super::*;
use nexo_core::agent::{AddedKind, AddedPart, GOAL_CONTINUE_MARK, SUPERVISOR_RESUME_MARK};

const START: &str = "[project at the start of this request]\nProject (changes since you last saw it; the rest as before):\n- src/ (3 files)\n  - src/c.rs";
const JOBS: &str = "[background jobs ended]\n- job 3 `make` done (exit 0) after 4s, 2 lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id).";

fn mail_text() -> String {
    let mail = nexo_core::mail::Mail {
        v: nexo_core::mail::MAIL_VERSION,
        id: "m-1-abcd".into(),
        from: nexo_core::mail::PeerInfo { id: "s-2".into(), name: "other".into(), ..Default::default() },
        from_kind: nexo_core::mail::FromKind::Agent,
        to: "s-1".into(),
        summary: "hi".into(),
        text: "please look at the parser".into(),
        at: 1_700_000_000_000,
        reply_to: None,
    };
    nexo_core::mail::render(&[mail])
}

fn started(a: &App) -> Vec<(&str, &Vec<AddedPart>)> {
    a.blocks
        .iter()
        .filter_map(|b| if let Block::Started { why, parts, .. } = b { Some((why.as_str(), parts)) } else { None })
        .collect()
}

#[test]
fn nexo_started_texts_are_never_a_message_of_the_person() {
    let cases: [(&str, String, &str); 6] = [
        ("bare resume", SUPERVISOR_RESUME_MARK.to_string(), "supervisor: continuing after a check-in"),
        ("resume + tree", format!("{SUPERVISOR_RESUME_MARK}\n\n{START}"), "supervisor: continuing after a check-in"),
        ("jobs", JOBS.to_string(), "background jobs ended"),
        ("jobs + note", format!("{JOBS}\n\n{START}"), "background jobs ended"),
        ("goal round + note", format!("{GOAL_CONTINUE_MARK}\n\n{START}"), "◎ goal · continuing"),
        ("mail + note", format!("{}\n\n{START}", mail_text()), "mail from another session"),
    ];
    for (name, text, why) in cases {
        let (mut a, _rx) = app();
        a.history(0, Message::user(text));
        assert!(!a.blocks.iter().any(|b| matches!(b, Block::User { .. })), "{name}: drawn as the person's");
        assert!(a.user_messages.is_empty(), "{name}: counted as the person's");
        let s = started(&a);
        assert_eq!(s.len(), 1, "{name}: one line for the request: {:?}", a.blocks);
        assert_eq!(s[0].0, why, "{name}");
        assert!(
            a.blocks.iter().all(|b| !matches!(b, Block::Added { .. })),
            "{name}: its notes are behind its own line"
        );
        if name.contains("note") || name.contains("tree") {
            assert!(s[0].1.iter().any(|p| p.kind == AddedKind::StartNote), "{name}: {:?}", s[0].1);
        }
        assert!(matches!(a.blocks.last(), Some(Block::Started { folded: true, .. })), "{name}: folded");
    }
}

#[test]
fn a_started_request_leaves_the_added_line_of_the_persons_message_alone() {
    let (mut a, _rx) = app();
    let note = "[project at the start of this request]\nattached: 1 files";
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user(format!("fix the bug\n\n{note}")) });
    let mine = vec![
        AddedPart::new(AddedKind::Hook, "hook context", "it is monday"),
        AddedPart::new(AddedKind::StartNote, "project", note),
    ];
    a.handle_agent(AgentEvent::Added { index: 0, parts: mine.clone() });
    let added = |a: &App| -> Vec<Vec<AddedPart>> {
        a.blocks
            .iter()
            .filter_map(|b| if let Block::Added { parts, .. } = b { Some(parts.clone()) } else { None })
            .collect()
    };
    assert_eq!(added(&a), vec![mine.clone()]);
    a.handle_agent(AgentEvent::History {
        index: 2,
        message: Message::user(format!("{SUPERVISOR_RESUME_MARK}\n\n{START}")),
    });
    let live = AddedPart::new(AddedKind::StartNote, "project", "live start note");
    a.handle_agent(AgentEvent::Added { index: 2, parts: vec![live.clone()] });
    assert_eq!(added(&a), vec![mine], "the person's line is as it was");
    let s = started(&a);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].1, &vec![live], "the live parts went to the request nexo started");
    assert_eq!(a.user_messages.len(), 1, "only the person's message counts");
}

#[test]
fn a_message_with_words_of_the_persons_still_counts_after_a_jobs_note() {
    let (mut a, _rx) = app();
    a.history(0, Message::user(format!("{JOBS}\n\n{START}\n\n{}\nnow fix it", nexo_core::agent::PERSON_MARK)));
    assert_eq!(a.user_messages.len(), 1);
    assert!(a.blocks.iter().any(|b| matches!(b, Block::User { text, .. } if text == "now fix it")));
}
