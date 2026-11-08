//! The state classifier: pure, no I/O. The first row that holds wins:
//! done, waiting for permission, waiting for an answer, looping, stalled, idle, working.

use super::policy::Policy;
use super::{AgentTrack, Reported, Secs, State, RESULT_WINDOW};

const MIN: u64 = 60;
/// Errors in the window that make a loop, and the same call failing this many times running.
const WINDOW_ERRORS: usize = 4;
const SAME_CALL_ERRORS: usize = 3;
/// Error level records that make a loop, within this many seconds.
const LOG_ERRORS: usize = 3;
const LOG_ERROR_SPAN: i64 = 600;
/// How much of the end of a reply is read for a question.
const TAIL_CHARS: usize = 300;

/// Why a session counts as looping, for the nudge text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopWhy {
    pub errors: usize,
    pub tool: String,
}

/// The state of `track` at `now` under `policy`.
pub fn classify(track: &AgentTrack, now: Secs, policy: &Policy) -> State {
    let t = &policy.thresholds;
    let idle = track.idle_secs(now);
    let tail_question = track.turn_end.as_ref().is_some_and(|(_, tail)| ends_in_question(tail, &t.ask_patterns));

    // Done
    if track.reported == Some(Reported::Done)
        || track.done
        || (track.dead && track.turn_end.is_some() && !tail_question)
    {
        return State::Done;
    }

    // Gone mid-work: it will not go on by itself, whatever it was waiting for.
    if track.dead && track.turn_end.is_none() {
        return State::Stalled;
    }

    // WaitingPermission
    if track.permission_pending.is_some() {
        return State::WaitingPermission;
    }
    if track.reported == Some(Reported::Blocked)
        && !track.bypasses_permission()
        && track.oldest_pending_age(now).is_some_and(|a| a > t.permission_secs)
    {
        return State::WaitingPermission;
    }

    // WaitingAnswer
    if track.ask_pending.is_some() || track.pending.iter().any(|(n, _)| n == "AskUserQuestion") {
        return State::WaitingAnswer;
    }
    if let Some((at, _)) = &track.turn_end {
        if tail_question && (now - at).max(0) as u64 >= t.answer_secs {
            return State::WaitingAnswer;
        }
    }

    // Looping
    if loop_signal(track, now).is_some() {
        return State::Looping;
    }

    // A tool that has run for less than the grace is a long build, not a stop.
    let busy_tool = track.oldest_pending_age(now).is_some_and(|a| a < t.tool_grace_min * MIN);

    // Stalled
    if let Some(idle) = idle {
        let limit = match track.loop_secs {
            Some(period) => 2 * period + 10 * MIN,
            None => t.stall_min * MIN,
        };
        if idle > limit && !busy_tool {
            return State::Stalled;
        }
    }

    // Idle
    let quiet = idle.unwrap_or(0);
    if track.turn_end.is_some() || idle.is_none() || (quiet > t.idle_min * MIN && !busy_tool) {
        return State::Idle { min: quiet / MIN };
    }
    State::Working
}

/// Whether the end of a reply asks the person something: its last 300 characters, markdown stripped,
/// end in a question mark or hold one of `patterns` (matched without regard to case).
pub fn ends_in_question(text: &str, patterns: &[String]) -> bool {
    let plain: String = text.chars().filter(|c| !matches!(c, '*' | '_' | '`' | '#' | '>' | '~')).collect();
    let chars: Vec<char> = plain.trim().chars().collect();
    let tail: String = chars[chars.len().saturating_sub(TAIL_CHARS)..].iter().collect();
    let tail = tail.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | '”' | '»'));
    if tail.ends_with('?') || tail.ends_with('？') {
        return true;
    }
    let lower = tail.to_lowercase();
    patterns.iter().any(|p| !p.is_empty() && lower.contains(&p.to_lowercase()))
}

/// Whether recent results show a loop: at least 4 errors among the last 8, the same call failing 3 times in a
/// row, or 3 error level records within 10 minutes.
pub fn loop_signal(track: &AgentTrack, now: Secs) -> Option<LoopWhy> {
    let recent: Vec<_> = track.results.iter().rev().take(RESULT_WINDOW).collect();
    let errors = recent.iter().filter(|r| r.is_error).count();
    let last_error_tool = || recent.iter().find(|r| r.is_error).map(|r| r.name.clone()).unwrap_or_default();

    // The same call, failing, newest first.
    if let Some(first) = recent.first().filter(|r| r.is_error) {
        let run = recent
            .iter()
            .take_while(|r| r.is_error && r.name == first.name && r.input_hash == first.input_hash)
            .count();
        if run >= SAME_CALL_ERRORS {
            return Some(LoopWhy { errors: run, tool: first.name.clone() });
        }
    }
    if errors >= WINDOW_ERRORS {
        return Some(LoopWhy { errors, tool: last_error_tool() });
    }
    let logged = track.errors.iter().filter(|t| now - **t <= LOG_ERROR_SPAN).count();
    if logged >= LOG_ERRORS {
        return Some(LoopWhy { errors: logged, tool: last_error_tool() });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::{input_hash, ObsKind, Observation};
    use super::*;

    const T0: Secs = 1_000_000;

    fn obs(track: &mut AgentTrack, at: Secs, kind: ObsKind) {
        track.apply(&Observation { agent_key: track.key.clone(), at, kind });
    }

    fn pending(name: &str) -> ObsKind {
        ObsKind::ToolPending { name: name.into() }
    }

    fn result(name: &str, input: &str, is_error: bool) -> ObsKind {
        ObsKind::ToolResult { name: name.into(), input_hash: input_hash(input), is_error }
    }

    fn end(text: &str) -> ObsKind {
        ObsKind::TurnEnd { last_text_tail: text.into() }
    }

    fn state(track: &AgentTrack, now: Secs) -> State {
        classify(track, now, &Policy::default())
    }

    fn fresh() -> AgentTrack {
        AgentTrack::new("a")
    }

    #[test]
    fn a_dead_process_mid_work_is_stalled_and_new_activity_after_it_revives_the_session() {
        let mut t = fresh();
        obs(&mut t, T0, pending("Bash"));
        obs(&mut t, T0 + 5, ObsKind::Dead);
        assert_eq!(state(&t, T0 + 6), State::Stalled);
        // a late report or name is not a sign of life
        obs(&mut t, T0 + 20, ObsKind::Reported(Reported::Working));
        assert_eq!(state(&t, T0 + 21), State::Stalled);
        assert_eq!(t.dead_since(), Some(T0 + 5));
        // the log moves on after it: it was only the list that was late
        obs(&mut t, T0 + 30, ObsKind::Activity);
        assert_eq!(state(&t, T0 + 31), State::Working);
        assert_eq!(t.dead_since(), None);
    }

    #[test]
    fn done_by_report_flag_or_dead_after_plain_end() {
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::Activity);
        obs(&mut t, T0, ObsKind::Reported(Reported::Done));
        assert_eq!(state(&t, T0 + 5), State::Done);

        let mut t = fresh();
        obs(&mut t, T0, ObsKind::Done);
        assert_eq!(state(&t, T0 + 5), State::Done);

        let mut t = fresh();
        obs(&mut t, T0, end("All finished."));
        obs(&mut t, T0 + 1, ObsKind::Dead);
        assert_eq!(state(&t, T0 + 2), State::Done);

        // Dead, but the last words were a question: the person still has to answer.
        let mut t = fresh();
        obs(&mut t, T0, end("Shall I go on?"));
        obs(&mut t, T0 + 1, ObsKind::Dead);
        assert_ne!(state(&t, T0 + 2), State::Done);

        // Work after a Done mark brings it back.
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::Done);
        obs(&mut t, T0 + 10, ObsKind::UserInput);
        assert_eq!(state(&t, T0 + 11), State::Working);
    }

    #[test]
    fn permission_by_ask_resolved_or_blocked_tool() {
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::PermissionPending { tool: "Bash".into() });
        assert_eq!(state(&t, T0 + 1), State::WaitingPermission);
        obs(&mut t, T0 + 2, ObsKind::PermissionResolved);
        assert_eq!(state(&t, T0 + 3), State::Working);

        // A tool without a result for over 20 s, session reported blocked, not in bypass mode.
        let mut t = fresh();
        obs(&mut t, T0, pending("Bash"));
        obs(&mut t, T0 + 25, ObsKind::Reported(Reported::Blocked));
        assert_eq!(state(&t, T0 + 25), State::WaitingPermission);
        // Only 10 s: not yet.
        assert_eq!(state(&t, T0 + 10), State::Working);

        // In bypass mode a silent tool is just a long tool.
        obs(
            &mut t,
            T0 + 26,
            ObsKind::Meta { name: None, cwd: None, permission_mode: Some("bypassPermissions".into()) },
        );
        assert_eq!(state(&t, T0 + 30), State::Working);

        // Not reported blocked: a long tool again.
        let mut t = fresh();
        obs(&mut t, T0, pending("Bash"));
        obs(&mut t, T0 + 40, ObsKind::Reported(Reported::Working));
        assert_eq!(state(&t, T0 + 40), State::Working);
    }

    #[test]
    fn answer_by_question_tool_or_text() {
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::AskPending);
        assert_eq!(state(&t, T0 + 1), State::WaitingAnswer);
        obs(&mut t, T0 + 2, ObsKind::AskResolved);
        assert_eq!(state(&t, T0 + 3), State::Working);

        let mut t = fresh();
        obs(&mut t, T0, pending("AskUserQuestion"));
        assert_eq!(state(&t, T0 + 1), State::WaitingAnswer);

        // A final question counts after 60 s; before, the session is just idle after its turn.
        let mut t = fresh();
        obs(&mut t, T0, end("I can do A or B. Which one do you prefer?"));
        assert_eq!(state(&t, T0 + 30), State::Idle { min: 0 });
        assert_eq!(state(&t, T0 + 60), State::WaitingAnswer);

        // A pattern without a question mark.
        let mut t = fresh();
        obs(&mut t, T0, end("Done with the first part. Let me know how to proceed."));
        assert_eq!(state(&t, T0 + 120), State::WaitingAnswer);

        // The person answered: new work, the old question is gone.
        obs(&mut t, T0 + 130, ObsKind::UserInput);
        assert_eq!(state(&t, T0 + 131), State::Working);
    }

    #[test]
    fn plain_end_of_turn_is_idle() {
        let mut t = fresh();
        obs(&mut t, T0, end("Tests pass."));
        assert_eq!(state(&t, T0 + 3), State::Idle { min: 0 });
        assert_eq!(state(&t, T0 + 4 * 60), State::Idle { min: 4 });
    }

    #[test]
    fn question_detection() {
        let p = Policy::default().thresholds.ask_patterns;
        assert!(ends_in_question("Is this fine?", &p));
        assert!(ends_in_question("**Which one?**", &p));
        assert!(ends_in_question("Ready (should I start?)", &p));
        assert!(ends_in_question("¿Sigo con el siguiente paso", &p));
        assert!(ends_in_question("Do you want me to commit", &p));
        assert!(!ends_in_question("Done. Committed abc123.", &p));
        // A question far back in a long reply is not the end of it.
        let long = format!("Is this fine? {}", "x".repeat(400));
        assert!(!ends_in_question(&long, &p));
        assert!(!ends_in_question("", &p));
    }

    #[test]
    fn looping_by_window_same_call_or_log() {
        // 4 errors among the last 8, different calls.
        let mut t = fresh();
        for i in 0..8 {
            obs(&mut t, T0 + i, result("Bash", &format!("cmd{i}"), i % 2 == 0));
        }
        assert_eq!(state(&t, T0 + 9), State::Looping);

        // 3 of 8: not a loop.
        let mut t = fresh();
        for i in 0..8 {
            obs(&mut t, T0 + i, result("Bash", &format!("cmd{i}"), i < 3));
        }
        assert_eq!(state(&t, T0 + 9), State::Working);

        // The same call failing 3 times running.
        let mut t = fresh();
        obs(&mut t, T0, result("Edit", "ok", false));
        for i in 1..=3 {
            obs(&mut t, T0 + i, result("Edit", "same", true));
        }
        let why = loop_signal(&t, T0 + 4).expect("a loop");
        assert_eq!((why.errors, why.tool.as_str()), (3, "Edit"));

        // Three different calls failing is not the same call.
        let mut t = fresh();
        for i in 1..=3 {
            obs(&mut t, T0 + i, result("Edit", &format!("n{i}"), true));
        }
        assert_eq!(loop_signal(&t, T0 + 4), None);

        // 3 error records within 10 minutes.
        let mut t = fresh();
        for i in 0..3 {
            obs(&mut t, T0 + i * 100, ObsKind::SystemError);
        }
        assert_eq!(state(&t, T0 + 250), State::Looping);
        // The same three, long ago: not a loop now.
        assert_eq!(loop_signal(&t, T0 + 5000), None);
    }

    #[test]
    fn long_build_is_not_a_stop() {
        let mut t = fresh();
        obs(&mut t, T0, pending("Bash"));
        // 15 minutes of silence with a tool running: still working.
        assert_eq!(state(&t, T0 + 15 * 60), State::Working);
        // Past the 20 minute grace the tool no longer excuses the silence.
        assert_eq!(state(&t, T0 + 25 * 60), State::Idle { min: 25 });
        // Past 30 minutes it is a stall.
        assert_eq!(state(&t, T0 + 31 * 60), State::Stalled);
        // The result arrives: back to normal.
        obs(&mut t, T0 + 31 * 60, result("Bash", "build", false));
        assert_eq!(state(&t, T0 + 31 * 60 + 5), State::Working);
    }

    #[test]
    fn stalled_and_idle_by_silence() {
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::Activity);
        assert_eq!(state(&t, T0 + 4 * 60), State::Working);
        assert_eq!(state(&t, T0 + 6 * 60), State::Idle { min: 6 });
        assert_eq!(state(&t, T0 + 29 * 60), State::Idle { min: 29 });
        assert_eq!(state(&t, T0 + 31 * 60), State::Stalled);
    }

    #[test]
    fn scheduled_session_waits_for_its_next_run() {
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::Loop { cron_secs: 15 * 60 });
        obs(&mut t, T0, end("Round done."));
        // Period 15 min: stalled only after 2 x 15 + 10 = 40 min, not 30.
        assert_eq!(state(&t, T0 + 35 * 60), State::Idle { min: 35 });
        assert_eq!(state(&t, T0 + 41 * 60), State::Stalled);
    }

    #[test]
    fn sidechain_activity_keeps_the_parent_alive() {
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::Activity);
        // A sub-agent keeps writing under the parent's key.
        obs(&mut t, T0 + 20 * 60, ObsKind::Activity);
        assert_eq!(state(&t, T0 + 21 * 60), State::Working);
        // An older record arriving late does not move the clock back.
        obs(&mut t, T0 + 10, ObsKind::Activity);
        assert_eq!(t.last_activity, Some(T0 + 20 * 60));
    }

    #[test]
    fn meta_is_not_activity() {
        let mut t = fresh();
        obs(&mut t, T0, ObsKind::Activity);
        obs(&mut t, T0 + 3000, ObsKind::Meta { name: Some("n".into()), cwd: None, permission_mode: None });
        assert_eq!(t.last_activity, Some(T0));
        assert_eq!(t.name.as_deref(), Some("n"));
        assert_eq!(state(&t, T0 + 3000), State::Stalled);
    }

    #[test]
    fn nothing_seen_is_idle() {
        assert_eq!(state(&fresh(), T0), State::Idle { min: 0 });
    }

    #[test]
    fn priority_order_holds() {
        // Permission beats a loop and a question.
        let mut t = fresh();
        for i in 0..4 {
            obs(&mut t, T0 + i, result("Bash", "x", true));
        }
        obs(&mut t, T0 + 5, ObsKind::AskPending);
        assert_eq!(state(&t, T0 + 6), State::WaitingAnswer);
        obs(&mut t, T0 + 7, ObsKind::PermissionPending { tool: "Bash".into() });
        assert_eq!(state(&t, T0 + 8), State::WaitingPermission);
        // Done beats everything.
        obs(&mut t, T0 + 9, ObsKind::Done);
        assert_eq!(state(&t, T0 + 10), State::Done);
    }
}
