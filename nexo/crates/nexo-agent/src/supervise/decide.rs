//! The decision ladder: given the state of one session, what the rules do about it. Pure: no clock, no files,
//! no sockets. The rungs, cheapest first: mark (always, done by the loop), nudge, alert the person. A rate
//! limit, a cool-down and the person's own presence hold every automatic nudge back; a session that does
//! not answer to nudges ends up as an alert instead of a fourth nudge.

use super::policy::Policy;
use super::{AgentTrack, Secs, State};

/// The person wrote in the session this recently: the supervisor stays out of the way.
pub const PERSON_PRESENT_SECS: Secs = 300;
/// How long in a looping state before the person is told.
pub const LOOP_ALERT_SECS: u64 = 15 * 60;
/// A nudge counts as having had an effect when the session did anything this long after it.
pub const EFFECT_SECS: Secs = 30;
const HOUR: Secs = 3600;

/// A nudge waiting for the session's next stop.
#[derive(Clone, Debug, PartialEq)]
pub struct Queued {
    pub text: String,
    pub rule: String,
    /// Written by the person (from the panel): no rate limit applies, only the loop guard.
    pub manual: bool,
    pub at: Secs,
}

/// What the supervisor remembers of its own dealings with one session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    /// When nudges were delivered, oldest first.
    pub nudges: Vec<Secs>,
    /// Nudges in a row that were followed by nothing.
    pub fruitless: u32,
    pub paused: bool,
    pub queued: Option<Queued>,
    /// The last alert: what it was about and when.
    pub alerted: Option<(String, Secs)>,
}

impl Ledger {
    pub fn last_nudge(&self) -> Option<Secs> {
        self.nudges.last().copied()
    }

    pub fn in_last_hour(&self, now: Secs) -> usize {
        self.nudges.iter().filter(|t| now - **t < HOUR).count()
    }

    /// The session did something after the last nudge: it worked.
    pub fn note_activity(&mut self, last_activity: Option<Secs>) {
        if let (Some(n), Some(a)) = (self.last_nudge(), last_activity) {
            if a > n + EFFECT_SECS {
                self.fruitless = 0;
            }
        }
    }

    /// A nudge goes out now.
    pub fn note_delivery(&mut self, last_activity: Option<Secs>, now: Secs) {
        if let Some(n) = self.last_nudge() {
            if last_activity.is_none_or(|a| a <= n + EFFECT_SECS) {
                self.fruitless += 1;
            } else {
                self.fruitless = 0;
            }
        }
        self.nudges.push(now);
        self.nudges.retain(|t| now - *t < 2 * HOUR);
        self.queued = None;
    }
}

/// What the rules decide.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Nothing,
    Nudge { text: String, rule: &'static str },
    Alert { why: String, rule: &'static str },
}

/// Everything `decide` looks at.
pub struct Ctx<'a> {
    pub track: &'a AgentTrack,
    pub state: State,
    /// Seconds spent in this state.
    pub in_state: u64,
    pub policy: &'a Policy,
    pub ledger: &'a Ledger,
    pub paused_all: bool,
    pub now: Secs,
}

/// The text of a nudge with `{idle}`, `{errors}` and `{tool}` filled in.
pub fn fill(template: &str, track: &AgentTrack, now: Secs) -> String {
    let idle = track.idle_secs(now).map_or("a while".to_string(), human);
    let errors = track.results.iter().filter(|r| r.is_error).count();
    let tool = track.results.iter().rev().find(|r| r.is_error).map_or("a tool", |r| r.name.as_str());
    template.replace("{idle}", &idle).replace("{errors}", &errors.to_string()).replace("{tool}", tool)
}

pub fn human(secs: u64) -> String {
    if secs < 90 {
        format!("{secs} s")
    } else if secs < 90 * 60 {
        format!("{} min", secs / 60)
    } else {
        format!("{} h", secs / 3600)
    }
}

/// The ladder for one session.
pub fn decide(c: &Ctx) -> Action {
    let p = c.policy;
    let name = c.track.name.as_deref().unwrap_or("");
    let cwd = c.track.cwd.as_deref().unwrap_or("");
    let fa = p.for_agent(name, cwd);
    if !p.enabled || c.paused_all || fa.paused || c.ledger.paused {
        return Action::Nothing;
    }
    match c.state {
        State::WaitingPermission => {
            let waited = c.track.permission_pending.as_ref().map_or(0, |(_, t)| (c.now - *t).max(0) as u64);
            if waited > p.thresholds.alert_after_min * 60 {
                return Action::Alert {
                    why: format!("waiting for permission for {}", human(waited)),
                    rule: "permission",
                };
            }
            return Action::Nothing;
        }
        State::Looping if c.in_state > LOOP_ALERT_SECS => {
            return Action::Alert { why: format!("looping for {}", human(c.in_state)), rule: "looping" };
        }
        State::Stalled | State::Looping => {}
        State::WaitingAnswer if fa.answer_questions => {}
        _ => return Action::Nothing,
    }
    // from here on the supervisor would write into the session
    if c.track.dead {
        return Action::Nothing;
    }
    if c.ledger.fruitless >= p.nudge.nudges_per_hour.max(1) {
        return Action::Alert { why: format!("{} nudges without effect", c.ledger.fruitless), rule: "no-effect" };
    }
    if c.track.last_user.is_some_and(|t| c.now - t < PERSON_PRESENT_SECS) {
        return Action::Nothing;
    }
    if c.ledger.in_last_hour(c.now) >= p.nudge.nudges_per_hour as usize {
        return Action::Alert { why: "nudge limit for this hour reached".into(), rule: "limit" };
    }
    if c.ledger.last_nudge().is_some_and(|t| c.now - t < (p.nudge.nudge_cooldown_min * 60) as Secs) {
        return Action::Nothing;
    }
    let (template, rule) = match c.state {
        State::Stalled => (&p.nudge.stalled, "stalled"),
        State::Looping => (&p.nudge.looping, "looping"),
        _ => (&p.nudge.question, "question"),
    };
    Action::Nudge { text: fill(template, c.track, c.now), rule }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(now: Secs) -> AgentTrack {
        let mut t = AgentTrack::new("k");
        t.last_activity = Some(now - 4000);
        t
    }

    fn run(t: &AgentTrack, state: State, l: &Ledger, now: Secs) -> Action {
        let p = Policy::default();
        decide(&Ctx { track: t, state, in_state: 100, policy: &p, ledger: l, paused_all: false, now })
    }

    #[test]
    fn a_stalled_session_gets_the_stalled_text_with_its_idle_time() {
        let now = 100_000;
        match run(&track(now), State::Stalled, &Ledger::default(), now) {
            Action::Nudge { text, rule } => {
                assert_eq!(rule, "stalled");
                assert!(text.contains("66 min"), "{text}");
            }
            a => panic!("{a:?}"),
        }
    }

    #[test]
    fn a_session_whose_process_is_gone_is_never_nudged() {
        let now = 100_000;
        let mut t = track(now);
        t.apply(&super::super::Observation { agent_key: "k".into(), at: now - 10, kind: super::super::ObsKind::Dead });
        assert!(t.dead);
        assert_eq!(run(&t, State::Stalled, &Ledger::default(), now), Action::Nothing);
    }

    #[test]
    fn working_idle_and_done_sessions_are_left_alone() {
        let now = 100_000;
        for s in [State::Working, State::Idle { min: 9 }, State::Done] {
            assert_eq!(run(&track(now), s, &Ledger::default(), now), Action::Nothing);
        }
    }

    #[test]
    fn the_hourly_limit_turns_into_an_alert() {
        let now = 100_000;
        let l = Ledger { nudges: vec![now - 3000, now - 2000, now - 1000], ..Default::default() };
        assert!(matches!(run(&track(now), State::Stalled, &l, now), Action::Alert { rule: "limit", .. }));
        let old = Ledger { nudges: vec![now - 9000, now - 8000, now - 7000], ..Default::default() };
        assert!(matches!(run(&track(now), State::Stalled, &old, now), Action::Nudge { .. }));
    }

    #[test]
    fn the_cooldown_holds_a_second_nudge_back() {
        let now = 100_000;
        let l = Ledger { nudges: vec![now - 14 * 60], ..Default::default() };
        assert_eq!(run(&track(now), State::Stalled, &l, now), Action::Nothing);
        let l = Ledger { nudges: vec![now - 16 * 60], ..Default::default() };
        assert!(matches!(run(&track(now), State::Stalled, &l, now), Action::Nudge { .. }));
    }

    #[test]
    fn the_person_being_present_holds_every_nudge_back() {
        let now = 100_000;
        let mut t = track(now);
        t.last_user = Some(now - 120);
        assert_eq!(run(&t, State::Stalled, &Ledger::default(), now), Action::Nothing);
        t.last_user = Some(now - 301);
        assert!(matches!(run(&t, State::Stalled, &Ledger::default(), now), Action::Nudge { .. }));
    }

    #[test]
    fn three_fruitless_nudges_alert_instead_of_a_fourth() {
        let now = 100_000;
        let l = Ledger { fruitless: 3, nudges: vec![now - 5000], ..Default::default() };
        assert!(matches!(run(&track(now), State::Looping, &l, now), Action::Alert { rule: "no-effect", .. }));
    }

    #[test]
    fn fruitless_counting_follows_activity_after_each_nudge() {
        let mut l = Ledger::default();
        l.note_delivery(Some(10), 100);
        assert_eq!(l.fruitless, 0, "the first nudge has nothing before it");
        l.note_delivery(Some(100), 1000);
        assert_eq!(l.fruitless, 1);
        l.note_delivery(Some(1000), 2000);
        assert_eq!(l.fruitless, 2);
        l.note_delivery(Some(2500), 3000);
        assert_eq!(l.fruitless, 0, "it worked after the nudge");
        l.fruitless = 2;
        l.note_activity(Some(3100));
        assert_eq!(l.fruitless, 0);
    }

    #[test]
    fn questions_follow_the_per_agent_switch_and_long_loops_alert() {
        let now = 100_000;
        let mut p = Policy::default();
        p.agents.push(crate::supervise::policy::AgentRule {
            pattern: "QA*".into(),
            answer_questions: Some(false),
            paused: None,
        });
        let mut t = track(now);
        t.name = Some("QA lane".into());
        let c = |t: &AgentTrack, p: &Policy, s: State, in_state: u64| {
            decide(&Ctx { track: t, state: s, in_state, policy: p, ledger: &Ledger::default(), paused_all: false, now })
        };
        assert_eq!(c(&t, &p, State::WaitingAnswer, 10), Action::Nothing);
        t.name = Some("build".into());
        assert!(matches!(c(&t, &p, State::WaitingAnswer, 10), Action::Nudge { rule: "question", .. }));
        assert!(matches!(c(&t, &p, State::Looping, 16 * 60), Action::Alert { rule: "looping", .. }));
        assert!(matches!(c(&t, &p, State::Looping, 60), Action::Nudge { rule: "looping", .. }));
    }

    #[test]
    fn permission_waits_alert_only_after_the_threshold_and_pauses_silence_everything() {
        let now = 100_000;
        let mut t = track(now);
        t.permission_pending = Some(("Bash".into(), now - 21 * 60));
        assert!(matches!(run(&t, State::WaitingPermission, &Ledger::default(), now), Action::Alert { .. }));
        t.permission_pending = Some(("Bash".into(), now - 5 * 60));
        assert_eq!(run(&t, State::WaitingPermission, &Ledger::default(), now), Action::Nothing);
        let l = Ledger { paused: true, ..Default::default() };
        assert_eq!(run(&track(now), State::Stalled, &l, now), Action::Nothing);
        let p = Policy::default();
        let all = Ctx {
            track: &track(now),
            state: State::Stalled,
            in_state: 1,
            policy: &p,
            ledger: &Ledger::default(),
            paused_all: true,
            now,
        };
        assert_eq!(decide(&all), Action::Nothing);
    }
}
