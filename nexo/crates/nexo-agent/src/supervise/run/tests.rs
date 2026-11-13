use super::*;
use crate::supervise::EvKind;
use crate::supervise::{ObsKind, Observation};
use nexo_core::agent::AgentEvent;

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

fn ob(key: &str, at: Secs, kind: ObsKind) -> Observation {
    Observation { agent_key: key.into(), at, kind }
}

#[test]
fn a_finished_session_goes_from_working_to_idle_and_each_change_is_marked() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(
        SourceKind::External,
        vec![
            ob(
                "a",
                1000,
                ObsKind::Meta { name: Some("Builder".into()), cwd: Some("/w/app".into()), permission_mode: None },
            ),
            ob("a", 1000, ObsKind::Step { text: "Bash  cargo test".into() }),
            ob("a", 1000, ObsKind::TurnEnd { last_text_tail: "done".into() }),
        ],
    )));
    assert!(s.tick(1010));
    let rows = s.rows(1010);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Builder");
    assert_eq!(rows[0].last_step.as_deref(), Some("Bash  cargo test"));
    assert_eq!(rows[0].idle_secs, Some(10));
    let v = s.version;
    assert!(!s.tick(1011), "nothing changed");
    assert_eq!(s.version, v);
    assert!(s.tick(1000 + 6 * 60), "idle after the threshold");
    let r = &s.rows(1000 + 6 * 60)[0];
    assert!(matches!(r.state, State::Idle { .. }));
    assert_eq!(r.last_action.as_ref().map(|a| a.0.as_str()), Some("marked idle"));
    s.tick(1000 + 40 * 60);
    assert_eq!(s.rows(1000 + 40 * 60)[0].state, State::Idle { min: 40 }, "a finished turn never reads as stalled");
}

#[test]
fn nexo_events_become_a_session_with_sub_agents_under_it() {
    use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
    let nexo = NexoSource::new();
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(nexo.clone()));
    nexo.observe(&AgentEvent::SessionChanged { session_id: "s1".into(), title: "Main work".into() }, 100);
    nexo.observe(&AgentEvent::TurnStart { turn: 1 }, 100);
    nexo.observe(
        &AgentEvent::Jobs {
            jobs: vec![JobInfo {
                id: 3,
                kind: JobKind::Agent,
                label: "scan the docs".into(),
                status: JobStatus::Running,
                elapsed_ms: 0,
                lines: 0,
            }],
        },
        101,
    );
    s.tick(105);
    let rows = s.rows(105);
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].key.as_str(), rows[0].source), ("s1", SourceKind::Nexo));
    assert_eq!(rows[0].program, "nexo");
    assert_eq!((rows[1].key.as_str(), rows[1].parent.as_deref()), ("s1.3", Some("s1")));
    assert_eq!(rows[1].name, "scan the docs");
}

#[test]
fn the_state_file_is_written_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("supervisor").join("state.json");
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(SourceKind::External, vec![ob("k", 5, ObsKind::Activity)])));
    s.tick(6);
    write_state(&path, &s.rows(6), 6).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["agents"][0]["state"], "working");
    assert_eq!(v["agents"][0]["source"], "external");
    let left: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().collect();
    assert_eq!(left.len(), 1, "no temporary file stays");
}
#[test]
fn nexo_events_fill_the_ribbon_the_counters_and_the_mail_feed() {
    use nexo_core::mail::Stage;
    let nexo = NexoSource::new();
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(nexo.clone()));
    nexo.observe(&AgentEvent::SessionChanged { session_id: "s1".into(), title: "api".into() }, 100);
    nexo.observe(&AgentEvent::TurnStart { turn: 1 }, 100);
    nexo.observe(&AgentEvent::TodoChanged { done: 1, total: 4 }, 101);
    nexo.observe(&AgentEvent::Retrying { attempt: 1, wait_secs: 30, rate_limit: true }, 102);
    nexo.observe(&AgentEvent::Compacted { before_tokens: 9000, after_tokens: 900 }, 103);
    nexo.observe(
        &AgentEvent::MailSent {
            id: "m-1".into(),
            to: "s2".into(),
            to_name: "web".into(),
            summary: "hi".into(),
            bytes: 2,
        },
        104,
    );
    nexo.observe(
        &AgentEvent::MailArrived {
            id: "m-2".into(),
            from: "s2".into(),
            from_name: "web".into(),
            summary: "yo".into(),
            held: false,
        },
        105,
    );
    nexo.observe(
        &AgentEvent::MailArrived {
            id: "m-3".into(),
            from: "s2".into(),
            from_name: "web".into(),
            summary: "ok?".into(),
            held: true,
        },
        105,
    );
    s.tick(106);
    let r = &s.rows(106)[0];
    assert_eq!((r.todo, r.unread, r.held), (Some((1, 4)), 1, 1));
    assert_eq!(r.retry_secs, Some(26));
    let kinds: Vec<(EvKind, u32)> = r.ribbon.iter().map(|e| (e.kind, e.n)).collect();
    // two mails in one second fold into one cell
    let want = vec![
        (EvKind::TurnStart, 1),
        (EvKind::Todo, 1),
        (EvKind::RateLimit, 1),
        (EvKind::Compact, 1),
        (EvKind::MsgOut, 1),
        (EvKind::MsgIn, 2),
    ];
    assert_eq!(kinds, want);
    let feed = s.mail_feed(0);
    assert_eq!(
        feed.iter().map(|m| (m.id.as_str(), m.stage)).collect::<Vec<_>>(),
        vec![("m-1", Stage::Sent), ("m-2", Stage::Sent), ("m-3", Stage::Held)]
    );
    // the receiver's host delivers m-2 and the model reads it; the mail leaves the unread count
    nexo.observe(&AgentEvent::MailDelivered { ids: vec!["m-2".into()] }, 110);
    nexo.observe(&AgentEvent::TurnStart { turn: 2 }, 110);
    nexo.observe(&AgentEvent::MailRead { ids: vec!["m-2".into()] }, 111);
    s.tick(112);
    let r = &s.rows(112)[0];
    assert_eq!((r.unread, r.held), (0, 1));
    let m2 = s.mail_feed(0).into_iter().find(|m| m.id == "m-2").unwrap();
    assert_eq!(m2.stage, Stage::Read);
    assert!(!r.ribbon.iter().any(|e| e.kind == EvKind::TurnStart && e.at == 110), "the mail turn is not the person's");
}

#[test]
fn two_sources_telling_one_mail_make_one_row_and_one_ribbon_event() {
    let nexo = NexoSource::new();
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(nexo.clone()));
    nexo.observe(&AgentEvent::SessionChanged { session_id: "s1".into(), title: "api".into() }, 100);
    let out = |at| {
        ob("s1", at, ObsKind::MsgOut { to: "s2".into(), mail: Some("m-9".into()), summary: "hello".into(), bytes: 5 })
    };
    nexo.observe(
        &AgentEvent::MailSent {
            id: "m-9".into(),
            to: "s2".into(),
            to_name: "web".into(),
            summary: "hello".into(),
            bytes: 5,
        },
        101,
    );
    s.add(Box::new(Fake(SourceKind::Nexo, vec![out(101)])));
    s.tick(102);
    assert_eq!(s.mail_feed(0).len(), 1);
    let r = &s.rows(102)[0];
    assert_eq!(r.ribbon.iter().filter(|e| e.kind == EvKind::MsgOut).map(|e| e.n).sum::<u32>(), 1);
}

#[test]
fn a_mail_of_a_log_pairs_its_out_and_its_in_and_a_stranger_gets_no_card() {
    let mut s = Supervisor::new(Policy::default());
    s.add(Box::new(Fake(
        SourceKind::External,
        vec![
            ob("k1", 1000, ObsKind::Activity),
            ob("k2", 1000, ObsKind::Activity),
            ob(
                "k1",
                1010,
                ObsKind::MsgOut { to: "second".into(), mail: None, summary: "build is red".into(), bytes: 12 },
            ),
            ob(
                "k2",
                1015,
                ObsKind::MsgIn { from: "first".into(), mail: None, summary: "build is red".into(), held: false },
            ),
            ob(
                "ghost",
                1016,
                ObsKind::MsgIn { from: "x".into(), mail: Some("m-5".into()), summary: "boo".into(), held: false },
            ),
        ],
    )));
    s.tick(1020);
    let feed = s.mail_feed(0);
    let x: Vec<_> = feed.iter().filter(|m| m.id.starts_with("x-")).collect();
    assert_eq!(x.len(), 1, "{feed:?}");
    assert_eq!((x[0].from.as_str(), x[0].to.as_str()), ("k1", "k2"));
    assert_eq!(x[0].stage, nexo_core::mail::Stage::Delivered);
    assert_eq!(s.rows(1020).len(), 2, "the mail of a session nobody tracks adds no card");
    assert!(feed.iter().any(|m| m.id == "m-5"));
}
