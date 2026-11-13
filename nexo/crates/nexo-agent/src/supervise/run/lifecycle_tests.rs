use super::*;
use crate::supervise::{Observation, Reported};

struct Fake(SourceKind, Vec<Observation>);
impl Source for Fake {
    fn id(&self) -> &str {
        "fake"
    }
    fn kind(&self) -> SourceKind {
        self.0
    }
    fn poll(&mut self, _now: Secs) -> Vec<Observation> {
        std::mem::take(&mut self.1)
    }
}

struct Panics;
impl Source for Panics {
    fn id(&self) -> &str {
        "panics"
    }
    fn kind(&self) -> SourceKind {
        SourceKind::External
    }
    fn poll(&mut self, _now: Secs) -> Vec<Observation> {
        panic!("a source that cannot read its own files");
    }
}

fn ob(key: &str, at: Secs, kind: ObsKind) -> Observation {
    Observation { agent_key: key.into(), at, kind }
}

fn keys(s: &Supervisor, now: Secs) -> Vec<String> {
    s.rows(now).into_iter().map(|r| r.key).collect()
}

const T0: Secs = 1_000_000;

#[test]
fn a_source_that_fails_does_not_hide_the_others() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Panics));
    s.add(Box::new(Fake(SourceKind::External, vec![ob("ok", T0, ObsKind::Activity)])));
    s.tick(T0 + 1);
    assert_eq!(keys(&s, T0 + 1), vec!["ok".to_string()]);
    // and the failing one is asked again next round, not dropped
    assert_eq!(s.sources.len(), 2);
}

#[test]
fn a_clock_ahead_of_ours_does_not_keep_a_session_working() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(
        SourceKind::External,
        vec![
            ob("f", T0 + 100_000, ObsKind::Activity),
            ob("f", T0 + 100_000, ObsKind::Step { text: "Bash  sleep".into() }),
        ],
    )));
    s.tick(T0);
    let r = &s.rows(T0)[0];
    assert_eq!(r.idle_secs, Some(0));
    // time passes by our clock: it goes quiet and is then marked like any other
    s.tick(T0 + 40 * 60);
    assert_eq!(s.rows(T0 + 40 * 60)[0].state, State::Stalled);
}

#[test]
fn a_finished_session_leaves_after_a_while_and_a_list_that_still_names_it_does_not_bring_it_back() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(
        SourceKind::External,
        vec![ob("d", T0, ObsKind::Activity), ob("d", T0, ObsKind::Reported(Reported::Done))],
    )));
    s.tick(T0 + 1);
    assert_eq!(s.rows(T0 + 1)[0].state, State::Done);
    s.tick(T0 + 5 * 60);
    assert_eq!(keys(&s, T0 + 5 * 60), vec!["d".to_string()], "still there for a few minutes");
    s.tick(T0 + 11 * 60);
    assert!(keys(&s, T0 + 11 * 60).is_empty());
    // the list names it again, as it does every round
    s.add(Box::new(Fake(
        SourceKind::External,
        vec![
            ob("d", T0 + 12 * 60, ObsKind::Meta { name: Some("d".into()), cwd: None, permission_mode: None }),
            ob("d", T0 + 12 * 60, ObsKind::Reported(Reported::Done)),
        ],
    )));
    s.tick(T0 + 12 * 60);
    assert!(keys(&s, T0 + 12 * 60).is_empty(), "a report is not a sign of life");
    // but a line it writes after it left brings it back
    s.add(Box::new(Fake(SourceKind::External, vec![ob("d", T0 + 13 * 60, ObsKind::UserInput)])));
    s.tick(T0 + 13 * 60);
    assert_eq!(s.rows(T0 + 13 * 60)[0].state, State::Working);
}

#[test]
fn a_session_whose_process_died_is_stalled_at_once_then_leaves() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(
        SourceKind::External,
        vec![ob("x", T0, ObsKind::ToolPending { name: "Bash".into() }), ob("x", T0 + 1, ObsKind::Dead)],
    )));
    s.tick(T0 + 2);
    assert_eq!(s.rows(T0 + 2)[0].state, State::Stalled, "not working while nothing runs");
    s.tick(T0 + 20 * 60);
    assert_eq!(keys(&s, T0 + 20 * 60).len(), 1);
    s.tick(T0 + 40 * 60);
    assert!(keys(&s, T0 + 40 * 60).is_empty());
}

#[test]
fn a_session_quiet_for_longer_than_anyone_looks_back_leaves_and_a_finished_one_sooner() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(
        SourceKind::External,
        vec![
            ob("cut-off", T0, ObsKind::Activity),
            ob("finished", T0, ObsKind::TurnEnd { last_text_tail: "All done.".into() }),
            ob("asking", T0, ObsKind::TurnEnd { last_text_tail: "Shall I go on?".into() }),
        ],
    )));
    s.tick(T0 + 60);
    assert_eq!(keys(&s, T0 + 60).len(), 3);
    // 3 hours: the finished one is gone, one that asks the person still stands
    s.tick(T0 + 3 * 3600);
    let left = keys(&s, T0 + 3 * 3600);
    assert_eq!(left, vec!["asking".to_string(), "cut-off".to_string()]);
    // past the 12 hour window nothing stays
    s.tick(T0 + 13 * 3600);
    assert!(keys(&s, T0 + 13 * 3600).is_empty());
}

#[test]
fn this_programs_own_session_stays_and_a_card_opened_before_its_id_goes() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(
        SourceKind::Nexo,
        vec![ob(LOCAL_KEY, T0, ObsKind::Activity), ob("real-id", T0 + 1, ObsKind::Activity)],
    )));
    s.tick(T0 + 2);
    assert_eq!(keys(&s, T0 + 2).len(), 2);
    s.tick(T0 + 5 * 60);
    assert_eq!(keys(&s, T0 + 5 * 60), vec!["real-id".to_string()]);
    // a quiet session of this program is the person's own: it stays all day
    s.tick(T0 + 20 * 3600);
    assert_eq!(keys(&s, T0 + 20 * 3600), vec!["real-id".to_string()]);
}

#[test]
fn a_pause_set_by_the_person_survives_the_card() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(SourceKind::External, vec![ob("p", T0, ObsKind::Done)])));
    s.tick(T0 + 1);
    s.set_pause(Some("p"), true, T0 + 2);
    s.tick(T0 + 20 * 60);
    assert!(keys(&s, T0 + 20 * 60).is_empty());
    assert!(s.ledgers.get("p").is_some_and(|l| l.paused));
}
