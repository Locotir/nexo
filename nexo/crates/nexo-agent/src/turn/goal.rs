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
    // the task list is not here: a change in it would change the system text and with it the cached
    // conversation after it. It rides on each request instead (`todo_note`). Goal words are fixed for its life.
    with_goal(&base, goal_words(st), st.goal_log.brief.as_deref())
}

/// The person's own words of the goal set, as they typed them: the ones the system prompt pins and the request
/// leaves out (`bare_goal_set`). Legacy sessions without recoverable original words fall back to their saved goal.
pub(super) fn goal_words(st: &State) -> Option<&str> {
    st.goal.as_ref()?;
    st.goal_log.original.as_deref().or(st.goal.as_deref())
}

/// The task list as it stands, for the request about to be sent. It rides with that request only, so a change
/// in it costs its own few tokens and not the cached conversation; and as history compaction never touches
/// a request's own notes, it stays in front of the model however long the work runs.
pub(super) fn todo_block(items: &[nexo_core::tool::TodoItem]) -> String {
    let body = nexo_tools::todo::render(items);
    format!("<todo-list>\n{body}\n</todo-list>")
}

/// [`todo_block`] of the live list, when the `todo` tool holds one.
pub(super) fn todo_note(st: &State) -> Option<String> {
    let items = st.tools.get("todo")?.todo_items()?;
    (!items.is_empty()).then(|| todo_block(&items))
}

/// The system prompt with the goal pinned at its end. It stays the same whatever the goal's state,
/// so pausing or resuming does not cost the cached prefix; what the state is reaches the model as a
/// line in the person's next message (`goal_state_line`), not as a change here.
pub(super) fn with_goal(system: &str, goal: Option<&str>, brief: Option<&str>) -> String {
    let Some(g) = goal else { return system.to_string() };
    let mut head = format!("Goal, in the person's words: {g}");
    // the check of the goal's words found them unordered or open: its restatement rides beside them, for the goal's life
    if let Some(b) = brief {
        head.push_str(&format!(
            "\nThe same goal ordered by a quick check (unverified: where it differs from the person's words, theirs win):\n{b}"
        ));
    }
    format!(
        "{system}\n\n{head}\nKeep working towards this goal across turns. `{GOAL_CONTINUE_MARK}` comes from the loop, \
         not the person: the goal is not marked done yet, so go on. When it is fully met, end your reply with the \
         single line GOAL DONE; if nothing is left to do, say so and end with that line. When you cannot go on \
         without the person (a question only they can answer, a decision, an approval, access you lack), ask it and \
         end your reply with the single line GOAL WAIT: the work waits for their answer. Do not stop for what you \
         can decide or check yourself. A message the person sends while you work comes first: answer it plainly, \
         then go on. Only an explicit new /goal changes the goal."
    )
}

/// The one line a person's message carries when a goal exists and is not running (met, or paused with its
/// reason), so the model does not take a finished goal for work in hand. None without a goal, for an active
/// goal, and for a waiting one (the message answers it: `person_spoke` has already resumed it).
pub(super) fn goal_state_line(st: &State) -> Option<String> {
    st.goal.as_ref()?;
    match &st.goal_state {
        GoalState::Done => Some(format!("{}; /goal resume reopens it]", nexo_core::agent::GOAL_MET_HEAD)),
        // one line, so the readers find the whole of it (`goal_state_line_of`)
        GoalState::Paused(why) => Some(format!(
            "{}{}), not running; /goal resume goes on]",
            nexo_core::agent::GOAL_PAUSED_HEAD,
            nexo_core::agent::one_line(why, 300)
        )),
        GoalState::Active | GoalState::Waiting => None,
    }
}

/// Whether a reply declares the goal met: `GOAL DONE` opens one of its last few non-empty lines (`signal`).
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

/// How many of a reply's last non-empty lines may hold `GOAL DONE` or `GOAL WAIT` (a closing sentence or two
/// can follow the line).
const SIGNAL_LINES: usize = 3;

/// Words that, right after `GOAL DONE` or `GOAL WAIT` and its separator, make the line a promise or a condition
/// and not the signal (`GOAL DONE, cuando acabe la build`), in English and Spanish.
const SIGNAL_HEDGES: [&str; 15] = [
    "yet", "once", "when", "if", "will", "not", "until", "after", "aún", "todavía", "cuando", "si", "no", "hasta",
    "después",
];

/// `word` (upper case) as the start of one of the last `SIGNAL_LINES` non-empty lines of a reply, read last first.
/// The line may carry markdown or quote marks in front and a short note after (`GOAL DONE — all tests pass`,
/// `GOAL DONE (42 green)`, `GOAL DONE ✅`), but the word must stand apart: not the start of a longer word, not
/// followed by plain words, and not by a hedge (`GOAL DONE, cuando acabe la build`).
pub(super) fn signal(text: &str, word: &str) -> bool {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).rev().take(SIGNAL_LINES).any(|l| signal_line(l, word))
}

/// Whether `line` opens with `word` as a signal; see [`signal`].
fn signal_line(line: &str, word: &str) -> bool {
    let line = line.trim_start_matches(['*', '_', '`', '"', '\'', '>', '#', '-', ' ']);
    // `get` is None inside a multi-byte character, and `word` is ASCII
    let Some(head) = line.get(..word.len()) else { return false };
    if !head.eq_ignore_ascii_case(word) {
        return false;
    }
    let rest = line[word.len()..].trim_start();
    // plain words after the word (or a longer word, `GOAL DONEX`) are no signal: a separator or the end follows
    if rest.chars().next().is_some_and(char::is_alphanumeric) {
        return false;
    }
    let next: String = rest.chars().skip_while(|c| !c.is_alphanumeric()).take_while(|c| c.is_alphanumeric()).collect();
    !SIGNAL_HEDGES.contains(&next.to_lowercase().as_str())
}

/// Pins (or clears) the goal; gives back the goal set, for the caller to start the work.
pub(super) async fn set_goal(state: &Shared, text: Option<String>) -> Option<String> {
    let mut st = state.lock().await;
    let text = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    st.goal = text.clone();
    // `/goal <text>` is the person's own command: it starts work, so it lifts a stop; a bare `/goal` only clears
    if text.is_some() {
        st.held = false;
    }
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
    st.goal_counts = GoalCounts::new(st.cost);
    // a new goal owes nothing to an earlier one; one set during a turn asks for its own request (see `on_set_goal`)
    st.goal_unread = false;
    st.goal_pending = None;
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

/// The longest the check of a goal's words may take before the goal goes ahead as typed.
const GOAL_CHECK_SECS: u64 = 20;

/// Asks the model once whether the goal just set needs restructuring (`goal_brief::ask`) and, when it does, keeps the
/// brief in the goal's log: the system prompt then holds it beside the person's words. A check that fails, runs
/// out of time, is switched off (`agent.goal_check`) or is not affordable leaves the goal as it was, quietly.
pub(super) async fn check_goal(state: &Shared, goal: &str) {
    let (client, target, on) = {
        let st = state.lock().await;
        (st.client.clone(), st.target.clone(), st.config.agent.goal_check && !super::background::over_budget(&st))
    };
    if !on {
        return;
    }
    let ask = crate::goal_brief::ask(&*client, &target, 1, goal);
    let Ok(Ok((brief, usage, model))) = tokio::time::timeout(Duration::from_secs(GOAL_CHECK_SECS), ask).await else {
        return;
    };
    let mut st = state.lock().await;
    let events = st.events.clone();
    add_usage(&mut st, &events, &model, usage, Usage::default(), UsageKind::GoalCheck);
    // another goal was set while the check ran: its words are not these
    if brief.is_some() && st.goal_log.original.as_deref() == Some(goal) {
        st.goal_log.brief = brief;
        emit_goal(&st);
    }
}

/// Reject stale or unsolicited calls, including calls replayed by an older model request.
pub(super) async fn goal_brief(_state: &Shared, _args: &Value) -> ToolOutput {
    ToolOutput::error("goal_brief is retired; only the person's /goal sets the goal")
}

/// Why a goal found active on disk is paused when its session opens: nothing runs for it until the person says so.
const REOPENED: &str = "session reopened";

/// The log of a goal found on disk. The person's own words are the ones saved with it; a file saved before they were
/// kept has them as the goal itself, or, when the goal is a restatement, as the words of the last `[goal set]`
/// message in `history`. A restated goal is the `brief`. The progress saved with the goal comes back as it was.
fn resumed_log(goal: Option<&str>, original: Option<&str>, progress: GoalProgress, history: &[Message]) -> GoalLog {
    let mut log = GoalLog::default();
    if let Some(g) = goal {
        let restated = g.starts_with(GOAL_BRIEF_HEAD);
        log.original = match original {
            Some(o) => Some(o.to_string()),
            None if restated => goal_set_words(history),
            None => Some(g.to_string()),
        };
        if restated && log.original.is_none() {
            log.brief = Some(g.to_string());
        }
    }
    log.with_progress(progress)
}

/// The words of the latest `[goal set]` request in `history`: what follows the mark, up to the start note and the
/// files nexo put after them (`goal_request`). None when no such request is left (a compaction removes it).
pub(super) fn goal_set_words(history: &[Message]) -> Option<String> {
    history.iter().rev().filter(|m| m.role == Role::User).find_map(|m| {
        let text = m.text();
        let rest = nexo_core::agent::goal_request(&text)?.strip_prefix(GOAL_SET_MARK)?;
        Some(rest.trim().to_string()).filter(|w| !w.is_empty())
    })
}

/// The goal of a session just opened, for the loop that now holds it. One found active was running when the session
/// closed and nothing runs it now, so it is paused (and saved so) instead of shown as working; one waiting, met or
/// paused stays as it was. Read-only opens (lists, search) never come here, so they write nothing.
pub(super) fn reopened_goal(session: &mut Session, events: &EventTx) -> GoalLog {
    let (goal, original) = (session.goal(), session.goal_original());
    let mut log = resumed_log(goal, original, session.goal_progress().clone(), session.messages());
    // words found again in the history of an older file: the session keeps them, so a compaction does not lose them
    if let Some(words) = &log.original {
        session.adopt_goal_original(words);
    }
    if session.goal().is_some() && session.goal_state().is_active() {
        remember_stop(&mut log.stops, REOPENED);
        if let Err(e) = session.record_goal_state(GoalState::Paused(REOPENED.into()), log.progress()) {
            notice(events, Level::Warn, format!("cannot save the goal: {e}"));
        }
    }
    log
}

/// Adds a stop to a goal's list, which keeps the last [`GOAL_STOPS`].
fn remember_stop(stops: &mut Vec<GoalStop>, why: &str) {
    stops.push(GoalStop { at_ms: nexo_core::mailbox::now_ms(), why: why.to_string() });
    let extra = stops.len().saturating_sub(GOAL_STOPS);
    stops.drain(..extra);
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

/// What a goal's work did since the goal was set or resumed, counted from the calls that ran: the facts the model
/// reads back on each request (see [`goal_note`]), so a long run is not blind to its own shape. Files are told by
/// the path the call was given.
#[derive(Debug, Clone)]
pub(super) struct GoalCounts {
    started: std::time::Instant,
    /// The session's cost when the count began.
    cost0: f64,
    calls: u32,
    read: HashSet<String>,
    /// The files read since a call last changed a file.
    read_since_edit: HashSet<String>,
    edited: HashSet<String>,
    commands: u32,
    commands_failed: u32,
}

impl GoalCounts {
    pub(super) fn new(cost: f64) -> Self {
        Self {
            started: std::time::Instant::now(),
            cost0: cost,
            calls: 0,
            read: HashSet::new(),
            read_since_edit: HashSet::new(),
            edited: HashSet::new(),
            commands: 0,
            commands_failed: 0,
        }
    }

    /// Counts the calls of one tool round: `ok` tells, call by call, whether each succeeded; `kind` gives the kind of
    /// a call by its name and arguments (None: unknown tool).
    pub(super) fn round(
        &mut self,
        calls: &[(String, String, Value)],
        ok: &[bool],
        kind: impl Fn(&str, &Value) -> Option<ToolKind>,
    ) {
        for (i, (_, name, args)) in calls.iter().enumerate() {
            let done = ok.get(i).copied().unwrap_or(false);
            self.calls += 1;
            if name == "bash" {
                self.commands += 1;
                self.commands_failed += u32::from(!done);
            }
            if !done {
                continue;
            }
            match kind(name, args) {
                Some(ToolKind::Write) => {
                    let paths = written_paths(args);
                    if !paths.is_empty() {
                        self.read_since_edit.clear();
                    }
                    self.edited.extend(paths);
                }
                Some(ToolKind::Read) if name == "read" => {
                    if let Some(p) = args.get("path").and_then(Value::as_str) {
                        self.read.insert(p.to_string());
                        self.read_since_edit.insert(p.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    /// The counts as one line of facts, `[nexo, not the person: goal so far: 23 min, round 3, 187 tool calls; read 96 files
    /// (41 since the last edit), edited 3; ran 12 commands, 2 failed; $1.84.]`: the spend only where the session priced it. None
    /// before the first call, when there is nothing to count.
    pub(super) fn line(&self, round: u32, cost: f64) -> Option<String> {
        if self.calls == 0 {
            return None;
        }
        let many = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
        let mut read = format!("read {}", many(self.read.len(), "file"));
        if !self.edited.is_empty() {
            read.push_str(&format!(" ({} since the last edit)", self.read_since_edit.len()));
        }
        let mut s = format!(
            "[nexo, not the person: goal so far: {} min, round {}, {}; {read}, edited {}; ran {}, {} failed",
            self.started.elapsed().as_secs() / 60,
            round + 1,
            many(self.calls as usize, "tool call"),
            self.edited.len(),
            many(self.commands as usize, "command"),
            self.commands_failed,
        );
        let spent = cost - self.cost0;
        if spent >= 0.005 {
            s.push_str(&format!("; ${spent:.2}"));
        }
        s.push_str(".]");
        Some(s)
    }
}

/// The files a write call names: its `path`, or the `path` of each entry of `files`.
fn written_paths(args: &Value) -> Vec<String> {
    let one = args.get("path").and_then(Value::as_str).map(str::to_string);
    let many = args
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("path").and_then(Value::as_str).map(str::to_string));
    one.into_iter().chain(many).collect()
}

/// Facts about active goal work, sent with each request without changing the goal.
pub(super) fn goal_note(st: &State) -> Option<String> {
    st.goal.as_ref()?;
    let (counts, idle) = if st.goal_state.is_active() {
        // an active goal's work that brings nothing new: the plain facts of it, in place of a generic note
        let idle = st.goal_idle.facts().map(|f| format!("[nexo, not the person: goal work: {f}]"));
        (st.goal_counts.line(st.goal_round, st.cost), idle)
    } else {
        (None, None)
    };
    let lines: Vec<String> = counts.into_iter().chain(idle).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Takes the person's words out of the goal request of `messages`: in each user text whose goal request is
/// `[goal set] <words>`, that request becomes `[goal set]`, and what nexo put around it (notes before, the start note
/// and files after) stays. The system prompt holds the words for the goal's life (`goal_words`), so a request carries
/// them once. Only the request changes: the history, the session file, compaction and the curator keep the words. An
/// earlier goal's request has other words and stays whole. A compaction's summary keeps the request as a part of its
/// own (`[request] [goal set] <words>`), which becomes `[request] [goal set]` the same way.
pub(super) fn bare_goal_set(messages: &mut [Message], words: &str) {
    let whole = format!("{GOAL_SET_MARK} {words}");
    for part in messages.iter_mut().filter(|m| m.role == Role::User).flat_map(|m| m.content.iter_mut()) {
        let Part::Text { text } = part else { continue };
        if text.strip_prefix(crate::compact::REQUEST) == Some(whole.as_str()) {
            *text = format!("{}{GOAL_SET_MARK}", crate::compact::REQUEST);
            continue;
        }
        let found = nexo_core::agent::goal_mark_at(text.as_str()).zip(nexo_core::agent::goal_request(text.as_str()));
        if let Some((at, request)) = found.filter(|(_, request)| *request == whole) {
            let end = at + request.len();
            text.replace_range(at..end, GOAL_SET_MARK);
        }
    }
}

/// Tells the interface the goal, its round and its state.
pub(super) fn emit_goal(st: &State) {
    // the log goes first: the interface has it when the goal it belongs to arrives
    if st.goal.is_some() {
        emit(&st.events, AgentEvent::GoalLog(goal_log(st)));
    }
    let event = AgentEvent::Goal {
        text: goal_words(st).map(str::to_string).or_else(|| st.goal.clone()),
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
        st.goal_counts = GoalCounts::new(st.cost);
    }
    // every pause is remembered with its time and reason (Esc, a guard, a limit, an error), for the goal panel
    if let GoalState::Paused(why) = &to {
        remember_stop(&mut st.goal_log.stops, why);
    }
    if let Err(e) = st.session.record_goal_state(to, st.goal_log.progress()) {
        notice(&st.events, Level::Warn, format!("cannot save the goal: {e}"));
    }
    // nothing to say: the screen shows the change already (the reply's own mark, the goal chip)
    if !say.is_empty() {
        notice(&st.events, Level::Info, say);
    }
    emit_goal(st);
}

/// A message of the person starts a turn: a goal waiting for their answer has it, and runs again.
pub(super) fn person_spoke(st: &mut State) {
    st.checked_in = false;
    // the person is back: what waited for them may start again
    st.held = false;
    // what the person says is news: the goal's work counts its progress from here
    st.goal_idle = GoalIdle::default();
    // only a person's message gives a request its checkpoints back: a goal's rounds, a mail or a job's end do not
    st.ck_used = 0;
    if st.goal.is_some() && st.goal_state == GoalState::Waiting {
        goal_to(st, GoalState::Active, String::new());
    }
}

/// What a turn's end does to an active goal, before anything else runs: met, waiting for the
/// person, or paused because the turn did not end on its own (Esc, an error or spend limit, the
/// tool-round limit). A goal that is not active stays as it is.
///
/// A goal set or resumed while the turn ran is not judged by that turn's reply, which was written before the model
/// knew of it. Enter on a queued message (`run_queue`) stops the turn to run that message at once: the goal goes on
/// after it, so it is not paused.
pub(super) async fn goal_settle(state: &Shared, reason: TurnEnd, run_queue: bool) {
    let mut st = state.lock().await;
    let unread = std::mem::take(&mut st.goal_unread);
    let typed = std::mem::take(&mut st.goal_typed);
    if st.goal.is_none() || !st.goal_state.is_active() {
        return;
    }
    if reason == TurnEnd::Interrupted && run_queue && !st.inbox.is_empty() {
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
            // a goal whose own request has not gone yet is not what any reply answers
            if unread || st.goal_pending.is_some() {
                return;
            }
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
                // the reply ends with its own waiting mark: no second line that says the same
                (GoalState::Waiting, String::new())
            } else if (typed || st.person_at > st.turn_start) && answered_in_words(&st) {
                // the person wrote during the round (their words joined it, or ran right after it) and the model
                // answered them in words (a stop, a question): a continuation would talk over them, so the goal
                // waits for their next message
                (
                    GoalState::Waiting,
                    "goal waiting: you wrote during the round · your next message resumes it".to_string(),
                )
            } else {
                return;
            }
        }
    };
    goal_to(&mut st, to, say);
}

/// After a turn that ended on its own, with nothing queued: what to submit to keep working towards
/// an active goal. Its continuation window renews when the last round made observable progress;
/// a round with no new successful tool result pauses instead. The first continuation always runs unless
/// NEXO_GOAL_ROUNDS=0 explicitly disables it. A goal set during a
/// turn has not been asked yet: its own request comes first, with its files, and is no continuation round.
pub(super) async fn goal_follow_up(state: &Shared) -> Option<(String, Vec<Attachment>)> {
    let mut st = state.lock().await;
    st.goal.as_ref()?;
    if !st.goal_state.is_active() {
        return None;
    }
    if st.goal_pending.is_some() {
        return Some(goal_request_now(&mut st));
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
    if max == 0 {
        let why = "automatic continuation disabled (NEXO_GOAL_ROUNDS=0)".to_string();
        let say = format!("goal paused: {why} · set a positive continuation window to continue automatically");
        goal_to(&mut st, GoalState::Paused(why), say);
        return None;
    }
    if st.goal_round >= max {
        // The no-progress check above passed: renew only the display window, not the idle watch,
        // counts, spend/checkpoint budgets or remembered results. Novel tool output is a heuristic,
        // not proof that the goal advanced; existing loop/error and manual-pause guards still apply.
        st.goal_round = 0;
    }
    st.goal_round += 1;
    emit_goal(&st);
    Some((GOAL_CONTINUE.to_string(), Vec::new()))
}

/// The request that goes to the model for the goal now: the `[goal set]` request of a goal set during a turn, with the
/// files the person attached to it (taken: it is asked once), else a continuation.
pub(super) fn goal_request_now(st: &mut State) -> (String, Vec<Attachment>) {
    match st.goal_pending.take() {
        Some(attachments) => {
            let words = goal_words(st).unwrap_or_default();
            (format!("{GOAL_SET_MARK} {words}"), attachments)
        }
        None => (GOAL_CONTINUE.to_string(), Vec::new()),
    }
}

/// Steps without progress after which the goal's work stops and the goal pauses, with the facts for the person:
/// three runs of reads as the supervisor's watch counts them.
pub(super) const GOAL_IDLE_STEPS: u32 = 3 * nexo_supervise::watch::READ_RUN as u32;
/// Repeats (the same call with the same result) after which the facts ride on each request.
const GOAL_REPEATS_SAID: u32 = 2;
/// The most names the facts list among the repeats.
const GOAL_REPEATS_NAMED: usize = 3;

/// How much of a goal's recent work made no progress: steps (tool calls) since the last progress, whether the turn in progress made any, what each non-write call returned in this goal (by tool and arguments),
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
/// succeeded, and `results` holds their outputs. All calls except explicit writes are remembered in `idle.seen`.
/// Exec is only a conservative permission/verification classification, not evidence of a mutation: an unknown
/// command can just query data. Repeating identical arguments and output must not renew the goal forever.
/// Explicit writes still count as progress; commands can show progress through changed arguments/output or
/// another novel result in the round. A silent repeated real mutation may therefore need an observable read.
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
            let (id, name, args) = c;
            if st.tools.get(name).is_some_and(|t| t.call_kind(args) == ToolKind::Write) {
                return CallWas::New;
            }
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

/// What `/goal resume` leaves to do (see [`resume_goal`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Resumed {
    /// Nothing runs and nothing is kept: the goal's request starts now.
    Start,
    /// Nothing runs but messages are kept (Esc left them): they start now, and the goal goes on after them.
    Queued,
    /// A turn is running: the goal goes on when it ends.
    Later,
    /// No goal is set.
    No,
}

/// `/goal resume`: the goal is active again with all its rounds. Nothing running starts the goal's request now, or
/// the messages Esc kept (the goal goes on after them); a turn in progress is followed by a continuation when it ends,
/// and its own reply says nothing about the goal (see [`goal_settle`]).
pub(super) async fn resume_goal(state: &Shared, running: bool) -> Resumed {
    let mut st = state.lock().await;
    if st.goal.is_none() {
        notice(&st.events, Level::Info, "no goal set: /goal <text> sets one");
        return Resumed::No;
    }
    let was_active = st.goal_state.is_active();
    let resumed = if running {
        // a turn of a goal that was running is the goal's own: only a goal that was not running had no say in it
        st.goal_unread |= !was_active;
        Resumed::Later
    } else {
        // the stop is lifted only when something starts now
        st.held = false;
        if st.inbox.is_empty() {
            Resumed::Start
        } else {
            Resumed::Queued
        }
    };
    goal_to(&mut st, GoalState::Active, "goal resumed".into());
    resumed
}

#[cfg(test)]
mod goal_tests {
    use super::*;
    use serde_json::json;

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
        assert!(goal_done("all set\nGOAL DONE. Thanks for the task!"), "words after the line count");
        assert!(!goal_done(&format!("GOAL DONE is what I will say later\n{}", "still working. ".repeat(20))));
        assert!(!goal_done(""));
    }

    #[test]
    fn a_goal_word_inside_a_sentence_is_no_signal() {
        assert!(!goal_done("Not GOAL DONE yet: sections 3 and 4 remain."));
        assert!(!goal_done("I'll write GOAL DONE once saved"));
        assert!(!goal_done("GOAL DONE once the build passes"));
        assert!(!goal_done("GOAL DONE, cuando acabe la build"));
        assert!(!goal_done("Aún no: GOAL DONE cuando termine"));
        assert!(!goal_wait("No GOAL WAIT needed, carrying on."));
    }

    #[test]
    fn a_goal_word_far_from_the_end_is_no_signal() {
        assert!(!goal_done("GOAL DONE\na\nb\nc\nd"));
        assert!(!goal_wait("GOAL WAIT\na\nb\n\nc\nd"));
        // blank lines do not count as lines
        assert!(goal_done("GOAL DONE\n\n\na\n\nb"));
    }

    #[test]
    fn a_goal_word_on_its_own_line_still_counts() {
        assert!(goal_done("all set\nGOAL DONE. Thanks for the task!"));
        assert!(goal_done("**GOAL DONE**"));
        assert!(goal_done("done\nGOAL DONE\n\nLet me know if you need more."));
        assert!(goal_wait("GOAL WAIT: which port should I use"));
    }

    #[test]
    fn a_goal_word_with_a_dash_bracket_or_emoji_still_counts() {
        assert!(goal_done("GOAL DONE — all tests pass"));
        assert!(goal_done("GOAL DONE - summary"));
        assert!(goal_done("GOAL DONE (42 green)"));
        assert!(goal_done("GOAL DONE ✅"));
        assert!(!goal_done("GOAL DONE once the build passes"));
        assert!(!goal_done("GOAL DONE — once the build passes"));
        assert!(!goal_done("GOAL DONE, aún no"));
        assert!(!goal_done("GOAL DONEX"));
        assert!(!goal_done("GOAL DONE2"));
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
        assert_eq!(with_goal("base", None, None), "base");
        let s = with_goal("base", Some("ship it"), None);
        assert!(s.starts_with("base\n\nGoal, in the person's words: ship it\n") && s.contains("single line GOAL DONE"));
        assert!(s.contains("single line GOAL WAIT") && !s.contains("goal_brief"));
        assert!(s.contains("`[goal: continue]` comes from the loop"));
    }

    #[test]
    fn the_goals_request_carries_the_mark_and_not_the_words() {
        let mut m = vec![Message::user("[goal set] make the tests pass")];
        bare_goal_set(&mut m, "make the tests pass");
        assert_eq!(m[0].text(), "[goal set]");
        // what nexo put around the request stays: a note before it, the start note and a file after it
        let around = "[background jobs ended]\njob 1 done\n\n[goal set] make the tests pass\n\n\
                      [project at the start of this request]\nGit: clean\n\n\
                      @notes.md (attached in full, as it is now):\n```md\nmake the tests pass\n```";
        let mut m = vec![Message::user(around)];
        bare_goal_set(&mut m, "make the tests pass");
        let want = around.replacen("[goal set] make the tests pass", "[goal set]", 1);
        assert_eq!(m[0].text(), want);
        assert!(m[0].text().contains("```md\nmake the tests pass\n```"), "a file's own words are not the goal's");
        // words of several paragraphs go whole
        let mut m = vec![Message::user("[goal set] fix x\n\nand then y\n\n[project at the start of this request]\nc")];
        bare_goal_set(&mut m, "fix x\n\nand then y");
        assert_eq!(m[0].text(), "[goal set]\n\n[project at the start of this request]\nc");
        // a summary's kept request of the goal: the mark stays, the words go; another kept request stays whole
        let mut m = vec![Message::new(
            Role::User,
            vec![
                Part::text("[request] [goal set] make the tests pass"),
                Part::text("[request] [goal set] an older goal"),
            ],
        )];
        bare_goal_set(&mut m, "make the tests pass");
        let texts: Vec<String> = m[0].content.iter().filter_map(|p| p.as_text().map(str::to_string)).collect();
        assert_eq!(texts, ["[request] [goal set]", "[request] [goal set] an older goal"]);
    }

    #[test]
    fn an_older_goals_request_and_other_messages_keep_their_words() {
        let mut m = vec![
            Message::user("[goal set] first goal"),
            Message::assistant("[goal set] first goal"),
            Message::user("please [goal set] first goal"),
            Message::user("[goal: continue]"),
            Message::user("[goal set] second goal"),
        ];
        bare_goal_set(&mut m, "second goal");
        let texts: Vec<String> = m.iter().map(Message::text).collect();
        let want = [
            "[goal set] first goal",
            "[goal set] first goal",
            "please [goal set] first goal",
            "[goal: continue]",
            "[goal set]",
        ];
        assert_eq!(texts, want);
        // the words of the goal must be the whole request: a request that only starts with them is another
        let mut m = vec![Message::user("[goal set] fix the parser and the lexer")];
        bare_goal_set(&mut m, "fix the parser");
        assert_eq!(m[0].text(), "[goal set] fix the parser and the lexer");
    }

    #[test]
    fn a_goal_from_disk_shows_as_the_words_it_was_saved_in() {
        let log = |goal, original, history: &[Message]| resumed_log(goal, original, GoalProgress::default(), history);
        let want = "Want: green\nWhy: x\nHow: y\nDone when: z";
        assert_eq!(log(None, None, &[]), GoalLog::default());
        let own = log(Some("tests green pls"), None, &[]);
        assert_eq!((own.original.as_deref(), own.brief), (Some("tests green pls"), None));
        // a restated goal: the person's words are the ones saved beside it
        let saved = log(Some(want), Some("tests green pls"), &[]);
        assert_eq!((saved.original.as_deref(), saved.brief.as_deref()), (Some("tests green pls"), None));
        // a file saved before the words were kept: the last `[goal set]` request holds them, without its start note
        let note = crate::session::START_NOTE_HEAD;
        let asked = |w: &str| Message::user(format!("{GOAL_SET_MARK} {w}\n\n{note}\nGit: 2 files"));
        let history =
            [asked("older goal"), Message::user("thanks"), asked("tests green pls"), Message::user(GOAL_CONTINUE_MARK)];
        let old = log(Some(want), None, &history);
        assert_eq!((old.original.as_deref(), old.brief.as_deref()), (Some("tests green pls"), None));
        // nor the files after them, with no start note between
        let filed = format!("{GOAL_SET_MARK} sum it up\n\n@notes.md {}\n```md\nx\n```", nexo_core::agent::ATTACHED);
        assert_eq!(log(Some(want), None, &[Message::user(filed)]).original.as_deref(), Some("sum it up"));
        // nothing left of that request (a compaction took it): the restatement is all there is
        let gone = log(Some(want), None, &[Message::user("thanks")]);
        assert_eq!((gone.original, gone.brief.as_deref()), (None, Some(want)));
        // the progress saved with the goal comes back
        let stop = GoalStop { at_ms: 9, why: "Esc".into() };
        let p = GoalProgress { started_ms: 5, steps: 3, stops: vec![stop] };
        let back = resumed_log(Some("tests green pls"), None, p.clone(), &[]);
        assert_eq!((back.started_ms, back.steps, back.progress()), (5, 3, p));
        assert_eq!(own.started_ms, 0, "a file without progress does not know when it was set");
    }

    #[test]
    fn the_counts_line_names_the_spend_only_when_it_shows_in_cents() {
        let call = |name: &str, args: Value| (format!("{name}-id"), name.to_string(), args);
        let kind = |n: &str, _: &Value| match n {
            "read" => Some(ToolKind::Read),
            "edit" => Some(ToolKind::Write),
            _ => Some(ToolKind::Exec),
        };
        let mut c = GoalCounts::new(1.0);
        assert_eq!(c.line(0, 1.0), None, "nothing is counted before the first call");
        let calls = [
            call("read", json!({"path": "a.rs"})),
            call("edit", json!({"files": [{"path": "a.rs"}, {"path": "b.rs"}]})),
            call("read", json!({"path": "c.rs"})),
            call("bash", json!({"command": "make"})),
        ];
        c.round(&calls, &[true, true, true, false], kind);
        assert_eq!(
            c.line(2, 1.004).as_deref(),
            Some("[nexo, not the person: goal so far: 0 min, round 3, 4 tool calls; read 2 files (1 since the last edit), edited 2; ran 1 command, 1 failed.]")
        );
        assert!(c.line(2, 2.84).unwrap().ends_with("1 failed; $1.84.]"));
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
        // the facts only: the items, their marks and the count, with no instruction around them
        assert_eq!(b, "<todo-list>\n[ ] pending item\n[>] working item\n[x] finished item\n1/3 done\n</todo-list>");
    }

    #[test]
    fn a_tool_without_items_adds_no_todo_section() {
        let set = ToolSet::new().with(Arc::new(nexo_tools::todo::Todo::new()));
        assert!(set.get("todo").and_then(|t| t.todo_items()).is_none());
    }
}
