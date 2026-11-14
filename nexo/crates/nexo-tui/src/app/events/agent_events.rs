//! What each event the agent sends does to the state of the screen.

use super::*;

impl App {
    pub(super) fn handle_main(&mut self, ev: AgentEvent) {
        self.dirty = true;
        // the work a "…" flash announced is over (or told its result in the transcript): drop it
        if matches!(
            ev,
            AgentEvent::Notice { .. }
                | AgentEvent::Compacted { .. }
                | AgentEvent::TurnEnd { .. }
                | AgentEvent::HistoryCleared
        ) {
            self.clear_progress_flash();
        }
        self.nexo_src.observe(&ev, nexo_agent::supervise::run::now_secs());
        match ev {
            AgentEvent::Ready { session_id, provider, model, cwd, permission_mode } => {
                self.session_id = Some(session_id);
                // nothing chosen yet: the agent runs on a placeholder that the person is never shown
                if !self.nothing_chosen() {
                    self.provider = provider;
                    self.model = model;
                }
                self.cwd = cwd;
                self.permission_mode = permission_mode;
                self.read_branch();
                self.refresh_welcome();
            }
            AgentEvent::History { index, message } => {
                // the curator answers with a `Context` once the loaded history is counted
                self.context_wait = self.context.is_none();
                self.history(index, message)
            }
            AgentEvent::Added { parts, .. } => self.added(parts),
            AgentEvent::Sent { model, sections } => {
                self.modal = Some(Modal::Sent(Box::new(crate::sent_view::SentView::new(model, sections))));
            }
            AgentEvent::Preparing { what } => {
                // the agent took the request: the work line runs from now, not from its first model call
                if !self.busy {
                    self.busy = true;
                    self.turn_started = Some(self.now());
                    self.streamed_chars = 0;
                }
                self.prep = Some(what);
            }
            AgentEvent::TurnStart { .. } => {
                // announced by `Preparing`: the clock already runs, and the agent's own `Queued` counts hold
                let announced = self.busy && self.prep.is_some();
                self.todo_done_live = false;
                self.suggestion = None;
                self.busy = true;
                if !announced || self.turn_started.is_none() {
                    self.turn_started = Some(self.now());
                }
                self.streamed_chars = 0;
                self.turn_tally = Default::default();
                self.turn_usage = Usage::default();
                self.turn_cost_start = self.cost;
                if !announced {
                    self.queued = self.queued.saturating_sub(1);
                    self.trim_queue();
                }
            }
            // a reply that starts while one is still open: the open one was dropped and the request sent again
            // (a stream cut inside its reasoning), so what it streamed is closed and not joined to the new one
            AgentEvent::AssistantStart => self.close_streaming(),
            AgentEvent::TextDelta { text } => {
                self.prep = None;
                self.streamed_chars += text.len();
                if self.fx_on() {
                    let now = self.now();
                    self.fx.ink.note(now, text.chars().filter(|c| *c != '\n').count());
                }
                self.close_reasoning();
                match self.blocks.last_mut() {
                    Some(Block::Assistant { text: t, streaming: true, .. }) => {
                        t.push_str(&text);
                        let i = self.blocks.len() - 1;
                        self.invalidate(i);
                    }
                    _ => {
                        let (at, started) = (Some(self.stamp_now()), Some(self.now()));
                        self.push(Block::Assistant { text, streaming: true, at, started, took_ms: None });
                    }
                }
            }
            AgentEvent::ReasoningDelta { text } => {
                self.prep = None;
                self.streamed_chars += text.len();
                match self.blocks.last_mut() {
                    Some(Block::Reasoning { text: t, streaming: true, .. }) => {
                        t.push_str(&text);
                        let i = self.blocks.len() - 1;
                        self.invalidate(i);
                    }
                    _ => {
                        self.push(Block::Reasoning {
                            text,
                            streaming: true,
                            started: Some(self.now()),
                            elapsed_ms: None,
                            folded: true,
                        });
                    }
                }
            }
            AgentEvent::ToolCall { id, name, args, summary } => {
                self.prep = None;
                self.close_streaming();
                // a replayed call already has its card from the message: give it the tool's summary
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { summary: s, .. } = &mut self.blocks[i] {
                        *s = summary;
                    }
                    self.invalidate(i);
                    return;
                }
                self.turn_tally.count_call(&name);
                // a plan stays open in the transcript: it is what was agreed
                let folded = name != "exit_plan";
                self.push(Block::Tool {
                    id,
                    name,
                    summary,
                    args,
                    status: ToolStatus::Pending,
                    progress: Vec::new(),
                    output: None,
                    started: None,
                    elapsed_ms: None,
                    folded,
                });
            }
            AgentEvent::ToolStart { id } => {
                if let Some(i) = self.tool_index(&id) {
                    let now = self.now();
                    if let Block::Tool { status, started, .. } = &mut self.blocks[i] {
                        *status = ToolStatus::Running;
                        *started = Some(now);
                    }
                    self.invalidate(i);
                }
            }
            AgentEvent::ToolProgress { id, line } => {
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { progress, .. } = &mut self.blocks[i] {
                        progress.push(line);
                        if progress.len() > MAX_PROGRESS {
                            progress.remove(0);
                        }
                    }
                    self.invalidate(i);
                }
            }
            AgentEvent::ToolDone { id, output, elapsed_ms } => {
                if let ToolDisplay::Todo { items } = &output.display {
                    // the card says what changed; the whole list stays in the window and the strip
                    let said = crate::todo_strip::change_line(&self.todo, items);
                    if let Some(i) = self.tool_index(&id) {
                        if let Block::Tool { summary, .. } = &mut self.blocks[i] {
                            *summary = said;
                        }
                    }
                    self.todo = items.clone();
                    self.todo_done_live = !items.is_empty() && crate::todo_strip::done_count(items) == items.len();
                    let at = self.now();
                    if let Some(Modal::Todo(v)) = &mut self.modal {
                        v.set_items(items.clone(), at);
                        self.dirty = true;
                    }
                }
                if let ToolDisplay::Remembered { scope, items, .. } = &output.display {
                    let place = if scope == "project" { "this project" } else { "global" };
                    let what = match items.as_slice() {
                        [one] => crate::render::wrap::truncate(one, 48),
                        more => nexo_core::count(more.len(), "rule"),
                    };
                    self.flash(&format!("✦ remembered ({place}): {what}"));
                }
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { status, output: o, elapsed_ms: e, .. } = &mut self.blocks[i] {
                        *status = status_of(&output);
                        self.words.feed(&output.content, crate::complete::Src::Other);
                        *o = Some(output);
                        *e = (elapsed_ms > 0).then_some(elapsed_ms); // a replay has no timing
                    }
                    self.note_change(i);
                    self.invalidate(i);
                }
            }
            AgentEvent::AssistantDone { message } => {
                let streamed_text = matches!(self.blocks.last(), Some(Block::Assistant { .. }))
                    || self
                        .blocks
                        .iter()
                        .rev()
                        .take_while(|b| !matches!(b, Block::User { .. }))
                        .any(|b| matches!(b, Block::Assistant { .. }));
                self.close_streaming();
                if !streamed_text {
                    let text = message.text();
                    if !text.trim().is_empty() {
                        let at = Some(self.stamp_now());
                        self.push(Block::Assistant { text, streaming: false, at, started: None, took_ms: None });
                    }
                }
            }
            AgentEvent::PermissionAsk { request } => {
                if self.modal.is_none() {
                    self.modal = Some(Modal::permission(request));
                } else {
                    self.pending_permissions.push_back(request);
                }
            }
            AgentEvent::Question { question } => {
                if self.modal.is_none() {
                    self.modal = Some(Modal::question(question));
                } else {
                    self.pending_questions.push_back(question);
                }
            }
            AgentEvent::QuestionResolved { id, .. } => {
                self.pending_questions.retain(|q| q.id != id);
                if matches!(&self.modal, Some(Modal::Question(a)) if a.q.id == id) {
                    self.modal = None;
                    self.next_permission();
                }
            }
            AgentEvent::PermissionResolved { id, decision } => {
                self.pending_permissions.retain(|r| r.id != id);
                if self.modal.as_ref().and_then(Modal::id) == Some(id.as_str()) {
                    self.modal = None;
                    self.next_permission();
                }
                if let Some(i) = self.tool_index(&id) {
                    if matches!(decision, PermissionDecision::Deny { .. }) {
                        if let Block::Tool { status, .. } = &mut self.blocks[i] {
                            *status = ToolStatus::Denied;
                        }
                        self.invalidate(i);
                    }
                }
            }
            AgentEvent::Usage { turn, call, session, cost_usd } => {
                // one event per model call: its input is what that call sent (the attempt that
                // answered), while the turn counts all it spent
                if call.total_input() > 0 {
                    self.last_call_input = call.total_input();
                    self.last_call_cached = call.cache_read;
                    self.call_inputs.push(call.total_input());
                }
                self.turn_tally.steps += 1;
                self.turn_usage += turn;
                self.session_usage = session;
                self.cost = cost_usd;
            }
            AgentEvent::Context {
                view_tokens,
                history_tokens,
                history_bytes,
                view_bytes,
                messages_in_view,
                messages_total,
                policy,
                note,
                label,
                detail,
            } => {
                // the view shrank while the history did not: the curator let something go
                if let Some(prev) = &self.context {
                    let dropped = prev.view_tokens.saturating_sub(view_tokens);
                    if dropped >= crate::motion::CUT_LEAST && history_tokens >= prev.history_tokens {
                        self.fx.cut = Some((dropped, self.now()));
                    }
                }
                self.context_wait = false;
                self.context = Some(ContextInfo {
                    view_tokens,
                    history_tokens,
                    history_bytes,
                    view_bytes,
                    messages_in_view,
                    messages_total,
                    policy: crate::render::wrap::one_line(&policy),
                    note: note.map(|n| crate::render::wrap::one_line(&n)),
                    label: crate::render::wrap::one_line(&label),
                    detail,
                });
            }
            AgentEvent::ContextMap { cells, zones, calls, call_tokens } => {
                let marks: std::collections::HashMap<String, CallMark> = calls.into_iter().collect();
                let call_tokens: std::collections::HashMap<String, u64> = call_tokens.into_iter().collect();
                // a card whose word changed is drawn again
                for i in 0..self.blocks.len() {
                    if let Block::Tool { id, .. } = &self.blocks[i] {
                        if marks.get(id) != self.marks.get(id) || call_tokens.get(id) != self.call_tokens.get(id) {
                            self.cache[i] = None;
                            self.dirty = true;
                        }
                    }
                }
                self.marks = marks;
                self.call_tokens = call_tokens;
                self.map_changed = cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| self.map.get(i).is_none_or(|old| old.state != c.state))
                    .collect();
                self.map = cells;
                self.map_zones = zones;
                self.map_at = Some(self.now());
            }
            AgentEvent::Curated { items } => self.note_curated(items),
            AgentEvent::Notice { problem: Some(problem), .. } => self.push_problem(problem),
            AgentEvent::Notice { level, text, .. } => self.notice(level, text),
            AgentEvent::UpdateAvailable { behind } => self.update_ready(behind),
            AgentEvent::ModelChanged { provider, model } => {
                // a model was chosen in the setup: the home opens
                self.unlock();
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    // the switcher offers a model only once it was saved, so after its check passed
                    if s.saved(&provider, &model) {
                        let pair = (provider.clone(), model.clone());
                        if !self.opts.models.contains(&pair) {
                            self.opts.models.push(pair);
                        }
                    }
                }
                self.flash(&format!("model: {provider}/{model}"));
                self.provider = provider;
                self.model = model;
                self.limits = None;
                self.refresh_welcome();
            }
            AgentEvent::ModelsListed { models } => {
                // what the next `/model` offers; a pair the person has but the list lacks stays
                let mut all = models;
                for pair in std::mem::take(&mut self.opts.models) {
                    if !all.contains(&pair) {
                        all.push(pair);
                    }
                }
                self.opts.models = all;
            }
            AgentEvent::SaveFailed { name, model, reason } => {
                // the wizard that waits for the save says it on its screen; with none open the failure is
                // still told, in one line
                let shown = match &mut self.modal {
                    Some(Modal::Setup(s)) => s.save_failed(&name, &model, reason.clone()),
                    _ => false,
                };
                if !shown {
                    self.notice(Level::Warn, reason);
                }
            }
            AgentEvent::PermissionModeChanged { mode } => {
                if mode != self.permission_mode && self.opts.animations {
                    self.fx.mode_from = Some(self.permission_mode);
                    self.fx.mode_at = Some(self.now());
                }
                self.permission_mode = mode;
                match mode {
                    PermissionMode::Plan => {
                        self.flash("plan mode: reads only, then a plan to approve · Shift+Tab leaves")
                    }
                    _ => self.flash(&format!("permissions: {}", mode.as_str())),
                }
                self.refresh_welcome();
            }
            AgentEvent::ShellOutput { command, output, exit_code } => {
                self.push(Block::Shell { command, output, exit_code });
            }
            AgentEvent::ModelLimits { provider, model, pricing, context_window, max_output, source } => {
                if self.nothing_chosen() {
                    return;
                }
                self.provider = provider.clone();
                self.model = model.clone();
                self.limits = Some(Limits { pricing, context_window, max_output, source: source.clone() });
                self.invalidate_all();
                self.refresh_welcome();
                // the card is worth a line after a change or a question, not at every start
                if !self.fresh() || source == "error" {
                    self.push(Block::Model { provider, model, pricing, context_window, max_output, source });
                }
            }
            AgentEvent::Budget { session_cost, day_cost, session_limit, day_limit } => {
                self.budget = Some((session_cost, day_cost, session_limit, day_limit));
            }
            AgentEvent::LocalServers { found } => {
                self.scan = Some(found.clone());
                if self.locked && found.iter().any(|(p, _)| *p == self.provider) {
                    self.unlock();
                }
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.found_local(found);
                }
            }
            AgentEvent::LoginPrompt { url, code } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.login_prompt(url, code);
                }
            }
            AgentEvent::LoggedIn { result, .. } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    if let crate::modal::setup::SetupOut::Send(cmd) = s.logged_in(result) {
                        self.send(cmd);
                    }
                }
            }
            AgentEvent::Probed { name, seq, result } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.probed_from(&name, seq, result);
                }
            }
            AgentEvent::ModelChecked { name, model, seq, check } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.checked_from(&name, &model, seq, check);
                }
            }
            AgentEvent::Effort { current, levels, .. } => {
                if self.effort != current && current.is_some() && self.effort_levels == levels {
                    self.flash(&format!("effort: {}", current.as_deref().unwrap_or("default")));
                }
                let (was_top, same_list) = (self.effort_top(), self.effort_levels == levels);
                self.effort = current;
                self.effort_levels = levels;
                self.note_effort_event(was_top, same_list);
            }
            AgentEvent::Fast { on } => {
                if self.fast != on {
                    self.flash(if on { "fast: on" } else { "fast: off" });
                }
                self.fast = on;
            }
            AgentEvent::GoalLog(log) => self.goal_log = log,
            AgentEvent::Goal { text, round, max, state } => {
                match (&text, &self.goal) {
                    (None, Some(_)) => self.flash("goal cleared"),
                    (Some(t), None) => self.flash(&format!("goal: {}", crate::render::wrap::truncate(t, 40))),
                    (Some(_), Some(g)) if g.state != state => self.flash(match &state {
                        GoalState::Active => "goal resumed",
                        GoalState::Waiting => "goal: waiting for your answer",
                        GoalState::Paused(_) => "goal paused · /goal resume",
                        GoalState::Done => "goal met ✓",
                    }),
                    (Some(_), Some(g)) if round > g.round => {
                        self.goal_round_said = Some((round, max));
                        self.flash(&format!("goal: continuing, round {round}/{max}"))
                    }
                    _ => {}
                }
                // a goal met bursts green; one stopped by a failure (not by the person) red
                let outcome = match (&text, &self.goal, &state) {
                    (Some(_), Some(g), GoalState::Done) if g.state != GoalState::Done => {
                        Some(crate::fx_burst::Kind::Met)
                    }
                    (Some(_), Some(g), GoalState::Paused(why))
                        if g.state.is_active() && crate::fx_burst::failed(why) =>
                    {
                        Some(crate::fx_burst::Kind::Failed)
                    }
                    _ => None,
                };
                if let Some(kind) = outcome {
                    self.burst(kind);
                }
                self.goal = text.map(|text| Goal { text, round, max, state });
                if self.goal.is_none() {
                    self.goal_log = Default::default();
                }
                self.sync_goal_view();
            }
            AgentEvent::SystemPrompt { text } => {
                self.system_prompt = Some(text.clone());
                self.push(Block::Shell { command: "system prompt".to_string(), output: text, exit_code: None });
            }
            AgentEvent::TurnEnd { reason } => {
                self.todo_done_live = false;
                self.close_streaming();
                self.busy = false;
                self.prep = None;
                if matches!(reason, TurnEnd::Done) {
                    self.turn_glow();
                }
                // an error or the turn limit: the red burst (an interrupt is the person's own stop)
                if matches!(reason, TurnEnd::Error | TurnEnd::MaxTurns) {
                    self.burst(crate::fx_burst::Kind::Failed);
                }
                self.turns_ended += 1;
                let elapsed_ms = self.turn_started.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
                // a long turn rings when it ends: the person may be in another window
                if self.opts.notify_after > 0 && elapsed_ms >= self.opts.notify_after * 1000 {
                    let what = match reason {
                        TurnEnd::Done => "done",
                        TurnEnd::Interrupted => "interrupted",
                        TurnEnd::Error => "stopped on an error",
                        TurnEnd::MaxTurns => "stopped at the turn limit",
                    };
                    self.bell_request = Some(format!("nexo: {what} after {}", fmt_ms(elapsed_ms)));
                }
                if !self.turn_usage.is_empty() {
                    let usage = self.turn_usage;
                    let cost_usd = (self.cost - self.turn_cost_start).max(0.0);
                    let tally = std::mem::take(&mut self.turn_tally);
                    let plan = self.on_plan();
                    self.push(Block::Usage { usage, cost_usd, elapsed_ms, tally, plan, folded: true });
                }
                match reason {
                    TurnEnd::Done => {}
                    TurnEnd::Interrupted => {
                        // the stop is done: "interrupting…" would say it is still going on
                        if self.status.as_ref().is_some_and(|(t, _)| t.starts_with("interrupting…")) {
                            self.status = None;
                        }
                        self.stop_rail();
                        self.notice(Level::Warn, "interrupted")
                    }
                    TurnEnd::Error => {}
                    TurnEnd::MaxTurns => {
                        self.notice(Level::Warn, "stopped: the turn limit was reached; send a message to go on")
                    }
                }
                for b in self.blocks.iter_mut() {
                    if let Block::Tool { status, .. } = b {
                        if matches!(status, ToolStatus::Pending | ToolStatus::Running) {
                            *status = ToolStatus::Failed;
                        }
                    }
                }
                self.invalidate_all();
                // a turn that ended normally asks for a guess at the next message (nothing waits in the queue)
                if matches!(reason, TurnEnd::Done) && self.opts.suggest && self.queued == 0 {
                    self.send(AgentCommand::Suggest);
                }
            }
            AgentEvent::Suggestion { text } => {
                // only into an empty box, between turns; a reply that comes late is let go
                // (and not while a sub-agent's view is open: its box is not the main chat's)
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if self.opts.suggest
                    && !self.busy
                    && self.sub_view.is_none()
                    && self.input.is_empty()
                    && !text.is_empty()
                {
                    self.suggestion = Some(text);
                    self.dirty = true;
                }
            }
            AgentEvent::HistoryCleared => {
                self.prep = None;
                self.clear_transcript()
            }
            AgentEvent::SessionChanged { session_id, title } => {
                self.flash(&format!("session: {}", if title.is_empty() { session_id.clone() } else { title.clone() }));
                self.session_id = Some(session_id);
                self.session_title = title;
            }
            AgentEvent::SessionDeleted { id, title } => {
                self.opts.sessions.retain(|(i, _)| *i != id);
                let name = if title.is_empty() { id.clone() } else { title };
                self.flash(&format!("deleted {name} · /resume undelete brings it back"));
                self.trashed.push(id);
            }
            AgentEvent::SessionRestored { id, title } => {
                self.trashed.retain(|i| *i != id);
                let name = if title.is_empty() { id.clone() } else { title.clone() };
                if !self.opts.sessions.iter().any(|(i, _)| *i == id) {
                    self.opts.sessions.insert(0, (id, title));
                }
                self.flash(&format!("restored {name}"));
            }
            AgentEvent::SessionTree { entries } => {
                if entries.len() < 2 {
                    self.notice(Level::Info, "no forks yet: /fork starts one");
                } else {
                    let here = self.session_id.clone().unwrap_or_default();
                    let n = entries.len();
                    let items = entries
                        .into_iter()
                        .map(|(depth, id, title)| {
                            let mark = if id == here { " ◀ here" } else { "" };
                            let branch =
                                if depth == 0 { String::new() } else { format!("{}└ ", "  ".repeat(depth - 1)) };
                            PickItem { label: format!("{branch}{id}{mark}"), value: id, hint: title }
                        })
                        .collect();
                    self.modal = Some(Modal::Picker(Picker::new(
                        PickKind::Tree,
                        &format!("Sessions · {n} · Enter jump"),
                        items,
                        None,
                    )));
                }
            }
            AgentEvent::Trash { sessions } => {
                if sessions.is_empty() {
                    self.notice(Level::Info, "the trash is empty");
                } else {
                    let n = sessions.len();
                    let items = sessions
                        .into_iter()
                        .map(|(id, title)| PickItem { label: id.clone(), value: id, hint: title })
                        .collect();
                    self.modal = Some(Modal::Picker(Picker::new(
                        PickKind::Trash,
                        &format!("Trash · {n} · Enter restore"),
                        items,
                        None,
                    )));
                }
            }
            AgentEvent::MemoryShared { pending } => {
                if let Some((_, title, tok)) = pending.iter().find(|p| !self.shared.iter().any(|s| s.0 == p.0)) {
                    self.flash(&format!(
                        "⊕ {} · {} tok join your next message",
                        if title.is_empty() { "memory" } else { title },
                        crate::render::blocks::fmt_k(*tok as u64)
                    ));
                }
                self.shared = pending;
            }
            AgentEvent::Queued { pending } => {
                self.queued = pending;
                self.trim_queue();
            }
            AgentEvent::Jobs { jobs } => {
                for j in &jobs {
                    let was_running = self.jobs.iter().any(|o| o.id == j.id && o.status.running());
                    if was_running && !j.status.running() {
                        let first: String = j.label.lines().next().unwrap_or("").chars().take(40).collect();
                        self.flash(&format!("job {} {}: {first}", j.id, j.status.plain()));
                    }
                }
                if let Some(Modal::Jobs(v)) = &mut self.modal {
                    v.set_jobs(jobs.clone());
                    self.commands.send(AgentCommand::JobTails).ok();
                }
                self.jobs = jobs;
                self.dirty = true;
            }
            AgentEvent::Mcp { servers } => {
                if let Some(Modal::Mcp(v)) = &mut self.modal {
                    v.set_servers(servers.clone());
                }
                self.set_mcp(servers);
                self.dirty = true;
            }
            AgentEvent::JobTails { tails } => {
                if let Some(Modal::Jobs(v)) = &mut self.modal {
                    v.set_tails(tails);
                }
            }
            // a request taken and then stopped before its turn (a hook refused it): nothing works any more
            AgentEvent::Idle { .. } => {
                if self.prep.take().is_some() {
                    self.busy = false;
                }
            }
            AgentEvent::MailSent { id, to_name, to, summary, .. } => {
                let peer = if to_name.is_empty() { to } else { to_name };
                self.push(Block::Mail { id, outgoing: true, peer, text: summary, stage: nexo_core::mail::Stage::Sent });
            }
            AgentEvent::MailFailed { to, reason } => {
                self.notice(Level::Warn, format!("not sent to {to}: {reason}"));
                self.flash_level(Level::Warn, &format!("message not sent: {reason}"));
            }
            AgentEvent::MailArrived { id, from, from_name, summary, held } => {
                let peer = if from_name.is_empty() { from } else { from_name };
                let stage = if held { nexo_core::mail::Stage::Held } else { nexo_core::mail::Stage::Sent };
                self.push(Block::Mail { id, outgoing: false, peer: peer.clone(), text: summary, stage });
                self.flash(&format!("message from {peer}"));
            }
            AgentEvent::MailDelivered { ids } => {
                for id in ids {
                    self.mail_stage(&id, nexo_core::mail::Stage::Delivered);
                }
            }
            AgentEvent::MailRead { ids } => {
                for id in ids {
                    self.mail_stage(&id, nexo_core::mail::Stage::Read);
                }
            }
            AgentEvent::MailStage { id, stage } => self.mail_stage(&id, stage),
            AgentEvent::Peers { peers } => {
                self.peers = peers;
                self.dirty = true;
            }
            AgentEvent::Compacted { .. } | AgentEvent::Retrying { .. } | AgentEvent::TodoChanged { .. } => {}
            // handled before this match (see `handle_sub`)
            AgentEvent::Sub { .. }
            | AgentEvent::SubDelta { .. }
            | AgentEvent::SubState { .. }
            | AgentEvent::SubNotDelivered { .. }
            | AgentEvent::SubTranscript { .. }
            | AgentEvent::FlowRun { .. } => {}

            AgentEvent::Shutdown => self.should_quit = true,
        }
    }
}
