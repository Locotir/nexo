//! Part of the agent loop; see the parent module.

use super::*;

pub(super) fn goal_rounds() -> u32 {
    parse_rounds(std::env::var("NEXO_GOAL_ROUNDS").ok())
}

pub(super) fn parse_rounds(v: Option<String>) -> u32 {
    v.and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(GOAL_ROUNDS)
}

/// The whole system prompt of a request: the prompt, the policy's directives
/// (stable, so cached) and the goal. A raw prompt file is sent as written.
pub(super) fn full_system(st: &State) -> String {
    let base =
        prompt::system_prompt_with(&st.cwd, &st.config.agent, &st.tools, window_of(&st.limits), &st.prefix.volatile);
    let specs = offered(st);
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    let mut base = prompt::with_directives(base, &st.config.agent, st.policy.directives(&names));
    if st.perm.mode == PermissionMode::Plan {
        let directive = match st.tools.get("ask_user") {
            Some(_) => crate::ask::PLAN_DIRECTIVE,
            None => crate::ask::PLAN_DIRECTIVE_HEADLESS,
        };
        base = format!("{base}\n\n{directive}");
    }
    let base = with_goal(&base, st.goal.as_deref());
    match st.tools.get("todo").and_then(|t| t.todo_items()) {
        Some(items) if !items.is_empty() => format!("{base}\n\n{}", todo_block(&items)),
        _ => base,
    }
}

/// The task list as a section of the system prompt. History compaction never touches the system
/// prompt, so the list stays in front of the model however long the work runs.
pub(super) fn todo_block(items: &[nexo_core::tool::TodoItem]) -> String {
    let body = nexo_tools::todo::render(items);
    format!(
        "<todo-list>\nYour task list, kept here every turn so it never slips out of context as the conversation \
         is compacted. Keep it current with the `todo` tool as steps finish or the plan changes.\n{body}\n</todo-list>"
    )
}

/// The system prompt with the goal pinned at its end. It stays the same whatever the goal's state,
/// so pausing or resuming does not cost the cached prefix. The person's own words come with the
/// request to restate them (`goal_brief`); once restated, the restatement is the goal.
pub(super) fn with_goal(system: &str, goal: Option<&str>) -> String {
    let Some(g) = goal else { return system.to_string() };
    let head = if g.starts_with(GOAL_BRIEF_HEAD) {
        format!("Goal, as the person means it:\n{g}")
    } else {
        format!(
            "Goal, in the person's words: {g}\nBefore working on it, call `goal_brief` once to restate it as they \
             mean it: what they want, why, how they expect it done, and when it counts as done."
        )
    };
    format!(
        "{system}\n\n{head}\nKeep working towards this goal across turns. `{GOAL_CONTINUE_MARK}` comes from the loop, \
         not the person: the goal is not marked done yet, so go on. When it is fully met, end your reply with the \
         single line GOAL DONE; if nothing is left to do, say so and end with that line. When you cannot go on \
         without the person (a question only they can answer, a decision, an approval, access you lack), ask it and \
         end your reply with the single line GOAL WAIT: the work waits for their answer. Do not stop for what you \
         can decide or check yourself. A message the person sends while you work comes first: answer it plainly, \
         then go on; call `goal_brief` again when they change what they want."
    )
}

/// Whether a reply declares the goal met: its last non-empty line is `GOAL DONE`, or the words
/// appear in its last `GOAL_TAIL` characters (models add a closing sentence after the line).
pub(super) fn goal_done(text: &str) -> bool {
    signal(text, "GOAL DONE")
}

/// Whether a reply stops for the person: `GOAL WAIT`, read as `GOAL DONE` is, or a last line that is
/// a question (models end a reply with one when they wait for an answer, whether told to or not).
pub(super) fn goal_wait(text: &str) -> bool {
    if signal(text, "GOAL WAIT") {
        return true;
    }
    let last = text.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("");
    let bare = last.trim_end_matches(['*', '_', '`', ')', '"', '\'', '»', ' ']);
    !last.starts_with("```") && (bare.ends_with('?') || bare.ends_with('？'))
}

/// `word` (upper case) as the last non-empty line of a reply, or in its last `GOAL_TAIL` characters.
pub(super) fn signal(text: &str, word: &str) -> bool {
    let last_line = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_end_matches(['.', '!']).trim().eq_ignore_ascii_case(word))
        .unwrap_or(false);
    let tail: String = text.chars().rev().take(GOAL_TAIL).collect::<Vec<_>>().into_iter().rev().collect();
    last_line || tail.to_ascii_uppercase().contains(word)
}

/// Pins (or clears) the goal; gives back the goal set, for the caller to start the work.
pub(super) async fn set_goal(state: &Shared, text: Option<String>) -> Option<String> {
    let mut st = state.lock().await;
    let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    st.goal = text.clone();
    st.goal_round = 0;
    st.goal_state = GoalState::Active;
    st.goal_log = match &text {
        Some(t) => {
            GoalLog { original: Some(t.clone()), started_ms: nexo_core::mailbox::now_ms(), ..GoalLog::default() }
        }
        None => GoalLog::default(),
    };
    st.goal_cost0 = st.cost;
    st.goal_idle = GoalIdle::default();
    if let Err(e) = st.session.record_goal(text.as_deref()) {
        notice(&st.events, Level::Warn, format!("cannot save the goal: {e}"));
    }
    match &text {
        // a goal set shows where its request joins the conversation
        Some(_) => {}
        None => notice(&st.events, Level::Info, "goal cleared"),
    }
    emit_goal(&st);
    text
}

/// Answers `goal_brief`: the agent's restatement takes the place of the goal from the next request on.
pub(super) async fn goal_brief(state: &Shared, args: &Value) -> ToolOutput {
    let brief = match crate::goal_brief::brief(args) {
        Ok(b) => b,
        Err(key) => return ToolOutput::error(format!("`{key}` is required")),
    };
    let mut st = state.lock().await;
    if st.goal.is_none() {
        return ToolOutput::error("no goal is set: carry on with the request");
    }
    st.goal = Some(brief.clone());
    st.goal_log.brief = Some(brief.clone());
    if let Err(e) = st.session.record_goal(Some(&brief)) {
        notice(&st.events, Level::Warn, format!("cannot save the goal: {e}"));
    }
    st.system = full_system(&st);
    emit_goal(&st);
    ToolOutput::ok("The goal now reads as restated. Go on with the work.")
}

/// The log of a goal found on disk: the person's words are not saved with the session, so a restated goal is
/// shown as the restatement, and one never restated as the words themselves; the time it was set is not known.
pub(super) fn resumed_log(goal: Option<&str>) -> GoalLog {
    let mut log = GoalLog::default();
    match goal {
        Some(g) if g.starts_with(GOAL_BRIEF_HEAD) => log.brief = Some(g.to_string()),
        Some(g) => log.original = Some(g.to_string()),
        None => {}
    }
    log
}

/// What the goal panel shows now: the log, the spend since the goal was set and the latest reply's first line.
pub(super) fn goal_log(st: &State) -> GoalLog {
    let last = st
        .history
        .iter()
        .rev()
        .filter(|m| m.role == Role::Assistant)
        .find_map(|m| m.text().lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string))
        .map(|l| l.chars().take(200).collect());
    GoalLog { cost_usd: (st.cost - st.goal_cost0).max(0.0), last, ..st.goal_log.clone() }
}

/// A tool call made while a goal is set counts as one of its steps.
pub(super) fn count_step(st: &mut State) {
    if st.goal.is_some() {
        st.goal_log.steps += 1;
    }
}

/// Tells the interface the goal, its round and its state.
pub(super) fn emit_goal(st: &State) {
    // the log goes first: the interface has it when the goal it belongs to arrives
    if st.goal.is_some() {
        emit(&st.events, AgentEvent::GoalLog(goal_log(st)));
    }
    let event = AgentEvent::Goal {
        text: st.goal.clone(),
        round: st.goal_round,
        max: goal_rounds(),
        state: st.goal_state.clone(),
    };
    emit(&st.events, event);
}

/// Moves the goal to `to`, saves it with the session (a resumed session finds it as it was) and
/// says why. The rounds start over: a goal that runs again gets all of them.
pub(super) fn goal_to(st: &mut State, to: GoalState, say: String) {
    st.goal_state = to.clone();
    st.goal_round = 0;
    // a goal that runs again (resumed, or with the person's answer) starts with all its reading tools
    if to.is_active() {
        st.goal_idle = GoalIdle::default();
    }
    // every pause is remembered with its time and reason (Esc, a guard, a limit, an error), for the goal panel
    if let GoalState::Paused(why) = &to {
        let stops = &mut st.goal_log.stops;
        stops.push(GoalStop { at_ms: nexo_core::mailbox::now_ms(), why: why.clone() });
        let extra = stops.len().saturating_sub(GOAL_STOPS);
        stops.drain(..extra);
    }
    if let Err(e) = st.session.record_goal_state(to) {
        notice(&st.events, Level::Warn, format!("cannot save the goal: {e}"));
    }
    notice(&st.events, Level::Info, say);
    emit_goal(st);
}

/// A message of the person starts a turn: a goal waiting for their answer has it, and runs again.
pub(super) fn person_spoke(st: &mut State) {
    st.checked_in = false;
    // what the person says is news: the goal's work counts its progress from here
    st.goal_idle = GoalIdle::default();
    // only a person's message gives a request its checkpoints back: a goal's rounds, a mail or a job's end do not
    st.ck_used = 0;
    if st.goal.is_some() && st.goal_state == GoalState::Waiting {
        goal_to(st, GoalState::Active, "goal: resumed with your answer".into());
    }
}

/// What a turn's end does to an active goal, before anything else runs: met, waiting for the
/// person, or paused because the turn did not end on its own (Esc, an error or spend limit, the
/// tool-round limit). A goal that is not active stays as it is.
pub(super) async fn goal_settle(state: &Shared, reason: TurnEnd) {
    let mut st = state.lock().await;
    if st.goal.is_none() || !st.goal_state.is_active() {
        return;
    }
    let paused = |why: &str| GoalState::Paused(why.to_string());
    let (to, say) = match reason {
        TurnEnd::Interrupted => (paused("interrupted"), "goal paused: interrupted · /goal resume goes on".to_string()),
        TurnEnd::Error => {
            (paused("the turn failed"), "goal paused: the turn failed · /goal resume tries again".to_string())
        }
        TurnEnd::MaxTurns => match st.ck_stop.take().or_else(|| st.goal_idle.facts()) {
            // a guard that stopped work bringing nothing new: the person gets the facts of it
            Some(why) => (paused(&why), format!("goal paused: {why} · /goal resume goes on")),
            None => (
                paused("the tool-round limit (agent.max_turns)"),
                "goal paused: the turn reached the tool-round limit (agent.max_turns) · /goal resume goes on"
                    .to_string(),
            ),
        },
        TurnEnd::Done => {
            let from = st.turn_start.min(st.history.len());
            let last = st.history[from..]
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .map(Message::text)
                .unwrap_or_default();
            if goal_done(&last) {
                (GoalState::Done, "goal met · /goal resume reopens it, /goal off clears it".to_string())
            } else if goal_wait(&last) {
                (GoalState::Waiting, "goal waiting for your answer: your next message resumes it".to_string())
            } else {
                return;
            }
        }
    };
    goal_to(&mut st, to, say);
}

/// After a turn that ended on its own, with nothing queued: what to submit to keep working towards
/// an active goal, while rounds are left. A continuation round that called no tool did nothing
/// new, so the goal pauses there instead of asking again; the first continuation always runs.
pub(super) async fn goal_follow_up(state: &Shared) -> Option<String> {
    let mut st = state.lock().await;
    st.goal.as_ref()?;
    if !st.goal_state.is_active() {
        return None;
    }
    let max = goal_rounds();
    // a round counts by its progress, not by its calls: a round that only got back what it had did nothing new
    if st.goal_round > 0 && !st.goal_idle.changed {
        let why = match st.goal_idle.facts() {
            _ if st.goal_idle.steps == 0 => "the last round did nothing new".to_string(),
            Some(f) => format!("the last round made no progress ({f})"),
            None => format!("the last round made no progress ({} steps)", st.goal_idle.steps),
        };
        let say = format!("goal paused: {why} · /goal resume goes on, or say what next");
        goal_to(&mut st, GoalState::Paused(why), say);
        return None;
    }
    if st.goal_round >= max {
        let why = format!("{max} rounds without GOAL DONE");
        let say = format!("goal paused: {why} · /goal resume gives it {max} more");
        goal_to(&mut st, GoalState::Paused(why), say);
        return None;
    }
    st.goal_round += 1;
    emit_goal(&st);
    Some(GOAL_CONTINUE.to_string())
}

/// Steps without progress after which the goal's work stops and the goal pauses, with the facts for the person:
/// three runs of reads as the supervisor's watch counts them.
pub(super) const GOAL_IDLE_STEPS: u32 = 3 * crate::supervise::watch::READ_RUN as u32;
/// Repeats (the same call with the same result) after which the facts ride on each request.
const GOAL_REPEATS_SAID: u32 = 2;
/// The most names the facts list among the repeats.
const GOAL_REPEATS_NAMED: usize = 3;

/// How much of a goal's recent work made no progress: steps (tool calls) since the last progress, whether the turn in progress made any, what each read-only call returned in this goal (by tool and arguments),
/// and which of those came back the same since the last progress, by what they were on, with how many times.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct GoalIdle {
    pub(super) steps: u32,
    pub(super) changed: bool,
    pub(super) seen: HashMap<String, u64>,
    pub(super) again: Vec<(String, u32)>,
}

impl GoalIdle {
    /// The facts of the work without progress, in few tokens: `no progress for 12 steps, same result again: store.rs
    /// ×6, cmd.rs ×5`. None while there is nothing to say.
    pub(super) fn facts(&self) -> Option<String> {
        let repeats: u32 = self.again.iter().map(|(_, n)| n).sum();
        if repeats < GOAL_REPEATS_SAID && self.steps < GOAL_IDLE_STEPS / 3 {
            return None;
        }
        let mut s = format!("no progress for {} steps", self.steps);
        if !self.again.is_empty() {
            let mut top = self.again.clone();
            top.sort_by_key(|t| std::cmp::Reverse(t.1));
            let named: Vec<String> =
                top.iter().take(GOAL_REPEATS_NAMED).map(|(what, n)| format!("{what} ×{n}")).collect();
            s.push_str(&format!(", same result again: {}", named.join(", ")));
        }
        Some(s)
    }
}

/// What one call of a goal's work was: progress (a change, or something new: a file, range, search or command
/// output not seen before in this goal, or one that differs from the last time), a repeat of a call that gave the same
/// result before (on what), or a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CallWas {
    New,
    Again(String),
    Failed,
}

/// What a step of an active goal leads to (see [`goal_watch`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GoalStep {
    /// Go on as before.
    Go,
    /// The work made no progress for too long: the turn ends and the goal pauses, for this reason.
    Stop(String),
}

/// One tool round of an active goal, call by call. Progress is something new, not only a change: reading files not
/// read before is the work of a goal that asks to find out something. A call that gives back what it gave before in
/// this goal is the waste: the facts of it ride on each request (see [`GoalIdle::facts`]), so the model judges for
/// itself, and after [`GOAL_IDLE_STEPS`] steps without progress the turn ends and the goal pauses with the same facts
/// for the person. No tool is taken away: a model without its reading tools edits blind. Any model, any task: only
/// calls, their arguments and their results are compared, never words.
pub(super) fn goal_watch(idle: &mut GoalIdle, calls: &[CallWas]) -> GoalStep {
    if calls.contains(&CallWas::New) {
        let seen = std::mem::take(&mut idle.seen);
        *idle = GoalIdle { changed: true, seen, ..GoalIdle::default() };
        return GoalStep::Go;
    }
    for c in calls {
        idle.steps += 1;
        if let CallWas::Again(what) = c {
            match idle.again.iter_mut().find(|(w, _)| w == what) {
                Some((_, n)) => *n += 1,
                None => idle.again.push((what.clone(), 1)),
            }
        }
    }
    if idle.steps >= GOAL_IDLE_STEPS {
        let facts = idle.facts().unwrap_or_else(|| format!("no progress for {} steps", idle.steps));
        return GoalStep::Stop(facts);
    }
    GoalStep::Go
}

/// What each call of a round was for the goal's progress (see [`CallWas`]); `ok` tells, call by call, whether it
/// succeeded, and `results` holds their outputs. The calls that only read are remembered in `idle.seen`.
pub(super) fn goal_calls(
    st: &State,
    idle: &mut GoalIdle,
    calls: &[(String, String, Value)],
    ok: &[bool],
    results: &[Part],
) -> Vec<CallWas> {
    calls
        .iter()
        .zip(ok)
        .map(|(c, &ok)| {
            if !ok {
                return CallWas::Failed;
            }
            let kind = |n: &str, a: &Value| st.tools.get(n).map(|t| t.call_kind(a));
            if verify::judge(std::slice::from_ref(c), &[true], kind).changed {
                return CallWas::New;
            }
            let (id, name, args) = c;
            let out = results.iter().find_map(|p| match p {
                Part::ToolResult { call_id, content, .. } if call_id == id => {
                    Some(content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n"))
                }
                _ => None,
            });
            let mut h = std::hash::DefaultHasher::new();
            std::hash::Hash::hash(&out.unwrap_or_default(), &mut h);
            let hash = std::hash::Hasher::finish(&h);
            match idle.seen.insert(format!("{name} {args}"), hash) {
                Some(before) if before == hash => CallWas::Again(call_target(name, args)),
                _ => CallWas::New,
            }
        })
        .collect()
}

/// What a call was on, short: the file name of its path, else its pattern or command (cut), else the tool.
fn call_target(name: &str, args: &Value) -> String {
    let get = |k: &str| args.get(k).and_then(Value::as_str);
    if let Some(p) = get("path").or_else(|| get("file_path")) {
        return p.rsplit(['/', '\\']).next().unwrap_or(p).to_string();
    }
    match get("pattern").or_else(|| get("command")) {
        Some(t) if t.chars().count() > 30 => format!("{name} {}…", t.chars().take(30).collect::<String>()),
        Some(t) => format!("{name} {t}"),
        None => name.to_string(),
    }
}

/// `/goal pause`: nothing more starts for the goal; a turn in progress finishes as it is.
pub(super) async fn pause_goal(state: &Shared, running: bool) {
    let mut st = state.lock().await;
    if st.goal.is_none() {
        notice(&st.events, Level::Info, "no goal to pause");
        return;
    }
    if matches!(st.goal_state, GoalState::Paused(_)) {
        notice(&st.events, Level::Info, "the goal is already paused · /goal resume goes on");
        return;
    }
    let say = if running {
        "goal paused: the turn in progress finishes, nothing more starts · /goal resume goes on"
    } else {
        "goal paused · /goal resume goes on"
    };
    goal_to(&mut st, GoalState::Paused("paused by you".into()), say.into());
}

/// `/goal resume`: the goal is active again with all its rounds. True when nothing runs, so the
/// caller starts a continuation now; a turn in progress is followed by one when it ends.
pub(super) async fn resume_goal(state: &Shared, running: bool) -> bool {
    let mut st = state.lock().await;
    if st.goal.is_none() {
        notice(&st.events, Level::Info, "no goal set: /goal <text> sets one");
        return false;
    }
    goal_to(&mut st, GoalState::Active, "goal resumed".into());
    !running && st.inbox.is_empty()
}

#[cfg(test)]
mod goal_tests {
    use super::*;

    #[test]
    fn a_file_that_cannot_be_read_is_not_recorded_as_missing() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(pre_image(&d.path().join("nope")), Some(None));
        std::fs::write(d.path().join("f"), "x").unwrap();
        assert_eq!(pre_image(&d.path().join("f")), Some(Some(b"x".to_vec())));
        // a folder where a file is expected: unreadable, not absent
        assert_eq!(pre_image(d.path()), None);
    }

    #[test]
    fn the_view_window_follows_the_providers_count() {
        // before any count: 15% to spare
        assert_eq!(view_window(10_000, None), 8_500);
        // code counted 1.4 tokens an estimated token: the view gets 0.95/1.4 of the room
        let r = learn_ratio(None, 12_827, 18_158);
        assert!((r.unwrap() - 1.4156).abs() < 0.001);
        assert_eq!(view_window(10_000, r), 6_710);
        // a larger ratio is taken at once, a smaller one slowly
        let up = learn_ratio(Some(1.2), 10_000, 15_000).unwrap();
        assert!((up - 1.5).abs() < 1e-9);
        let down = learn_ratio(Some(1.5), 10_000, 10_000).unwrap();
        assert!((down - 1.4).abs() < 1e-9);
        // tiny requests and missing counts teach nothing; wild counts are bounded
        assert_eq!(learn_ratio(Some(1.3), 200, 900), Some(1.3));
        assert_eq!(learn_ratio(None, 10_000, 0), None);
        assert_eq!(learn_ratio(None, 1_000, 90_000), Some(3.0));
    }

    #[test]
    fn goal_done_reads_the_last_line() {
        assert!(goal_done("all fixed\n\nGOAL DONE\n"));
        assert!(goal_done("done.\ngoal done."));
        assert!(goal_done("all set\nGOAL DONE. Thanks for the task!"), "the words in the tail count");
        assert!(!goal_done(&format!("GOAL DONE is what I will say later\n{}", "still working. ".repeat(20))));
        assert!(!goal_done(""));
    }

    #[test]
    fn goal_wait_reads_a_question_or_the_wait_line() {
        assert!(goal_wait("Which branch should I use?"));
        assert!(goal_wait("Two ways.\n\n**Should I drop the old table?**\n"));
        assert!(goal_wait("¿Lo borro (`rm -rf build`)?"));
        assert!(goal_wait("I need the staging key.\nGOAL WAIT"));
        assert!(goal_wait("I need the staging key.\n\nGOAL WAIT. I will go on once I have it."));
        assert!(!goal_wait("Fixed it. Want more? Not needed: all tests pass."));
        assert!(!goal_wait("```\nwhat?\n```"), "a fence closes the reply, not a question");
        assert!(!goal_wait("all set\nGOAL DONE"));
        assert!(!goal_wait(""));
    }

    #[test]
    fn fresh_lines_tells_an_output_that_brings_a_line_not_seen_before() {
        let out = |text: &str| vec![Part::tool_result("c", text, false)];
        let mut known = known_lines(&[Message::tool_results(out("fn main() {\n    run();\n}"))]);
        // a slice of what was already read, other spacing aside, brings nothing
        assert_eq!(fresh_lines(&mut known, &out("run();\n}")), Some(false));
        // one new line is enough, and it is known from then on
        assert_eq!(fresh_lines(&mut known, &out("fn main() {\nfn exit() {")), Some(true));
        assert_eq!(fresh_lines(&mut known, &out("fn exit() {")), Some(false));
        // blank lines and short markers tell nothing: the round brought no line at all
        assert_eq!(fresh_lines(&mut known, &out("\n}\nok\n...")), None);
    }

    #[test]
    fn looping_sees_a_block_of_rounds_that_brings_nothing_new() {
        // the same round three times: a command run over and over with the same output
        assert_eq!(looping(&[9, 1, 1, 1], 3), Some(1));
        assert_eq!(looping(&[9, 1, 1], 3), None, "twice is not yet a loop");
        // a cycle of three reads that comes back three times
        assert_eq!(looping(&[5, 1, 2, 3, 1, 2, 3, 1, 2, 3], 3), Some(3));
        assert_eq!(looping(&[1, 2, 3, 1, 2, 3, 1, 2], 3), None, "the third time is not over");
        assert_eq!(looping(&[1, 2, 3, 1, 2, 4, 1, 2, 3], 3), None, "an output that changed is progress");
        assert_eq!(looping(&[1, 1, 1, 1], 0), None, "0 turns the guard off");
        // rounds differ by what came back, not by the call ids
        let ask = vec![("bash".to_string(), "{\"command\":\"make test\"}".to_string())];
        let out = |id: &str, text: &str| vec![Part::tool_result(id, text, false)];
        assert_eq!(round_hash(&ask, &out("a", "1 failed")), round_hash(&ask, &out("b", "1 failed")));
        assert_ne!(round_hash(&ask, &out("a", "1 failed")), round_hash(&ask, &out("a", "all passed")));
    }

    #[test]
    fn rounds_default_and_parse() {
        assert_eq!(parse_rounds(None), GOAL_ROUNDS);
        assert_eq!(parse_rounds(Some(" 2 ".into())), 2);
        assert_eq!(parse_rounds(Some("many".into())), GOAL_ROUNDS);
    }

    #[test]
    fn the_goal_is_pinned_at_the_end() {
        assert_eq!(with_goal("base", None), "base");
        let s = with_goal("base", Some("ship it"));
        assert!(s.starts_with("base\n\nGoal, in the person's words: ship it\n") && s.contains("single line GOAL DONE"));
        assert!(s.contains("single line GOAL WAIT") && s.contains("call `goal_brief` once"));
        // once restated, the restatement is the goal and nothing asks for it again
        let s = with_goal("base", Some("Want: a\nWhy: b\nHow: c\nDone when: d"));
        assert!(
            s.starts_with("base\n\nGoal, as the person means it:\nWant: a\n") && !s.contains("call `goal_brief` once")
        );
        assert!(s.contains("`[goal: continue]` comes from the loop"));
    }

    #[test]
    fn a_goal_from_disk_shows_as_the_words_it_was_saved_in() {
        assert_eq!(resumed_log(None), GoalLog::default());
        let own = resumed_log(Some("tests green pls"));
        assert_eq!((own.original.as_deref(), own.brief), (Some("tests green pls"), None));
        let restated = resumed_log(Some("Want: green\nWhy: x\nHow: y\nDone when: z"));
        assert_eq!(
            (restated.original, restated.brief.as_deref()),
            (None, Some("Want: green\nWhy: x\nHow: y\nDone when: z"))
        );
        assert_eq!(restated.started_ms, 0, "when it was set is not known");
    }
}

#[cfg(test)]
mod todo_tests {
    use super::*;
    use nexo_core::tool::{TodoItem, TodoStatus};

    fn item(text: &str, status: TodoStatus) -> TodoItem {
        TodoItem { text: text.into(), status }
    }

    #[test]
    fn the_todo_block_lists_each_item_with_its_mark_inside_the_tag() {
        let b = todo_block(&[
            item("pending item", TodoStatus::Pending),
            item("working item", TodoStatus::InProgress),
            item("finished item", TodoStatus::Done),
        ]);
        assert!(b.starts_with("<todo-list>\n") && b.ends_with("\n</todo-list>"));
        assert!(b.contains("\n[ ] pending item\n[>] working item\n[x] finished item\n"));
    }

    #[test]
    fn a_tool_without_items_adds_no_todo_section() {
        let set = ToolSet::new().with(Arc::new(nexo_tools::todo::Todo::new()));
        assert!(set.get("todo").and_then(|t| t.todo_items()).is_none());
    }
}
