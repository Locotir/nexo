//! What each event the agent sends does to the state of the screen.

use super::*;

mod mail;
mod session;
mod setup;
mod side;
mod spend;
mod stream;

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
        self.nexo_src.observe(&ev, nexo_supervise::run::now_secs());
        // the heartbeat tells the other sessions the step, model, cost and goal (metadata only)
        if let Some(m) = crate::mailbox() {
            m.observe(&ev);
        }
        match ev {
            AgentEvent::Ready { session_id, provider, model, cwd, permission_mode } => {
                self.on_ready(session_id, provider, model, cwd, permission_mode)
            }
            AgentEvent::History { index, message } => self.on_history(index, message),
            AgentEvent::Added { index, parts } => self.added(index, parts),
            AgentEvent::Sent { model, sections } => self.on_sent(model, sections),
            AgentEvent::Preparing { what } => self.on_preparing(what),
            AgentEvent::TurnStart { .. } => self.on_turn_start(),
            // a reply that starts while one is still open: the open one was dropped and the request sent again
            // (a stream cut inside its reasoning), so what it streamed is closed and not joined to the new one
            AgentEvent::AssistantStart => self.on_assistant_start(),
            AgentEvent::TextDelta { text } => self.on_text_delta(text),
            AgentEvent::ReasoningDelta { text } => self.on_reasoning_delta(text),
            AgentEvent::ToolCall { id, name, args, summary } => self.on_tool_call(id, name, args, summary),
            AgentEvent::ToolStart { id } => self.on_tool_start(id),
            AgentEvent::ToolProgress { id, line } => self.on_tool_progress(id, line),
            AgentEvent::ToolDone { id, output, elapsed_ms } => self.on_tool_done(id, output, elapsed_ms),
            AgentEvent::AssistantDone { message } => self.on_assistant_done(message),
            AgentEvent::PermissionAsk { request } => self.on_permission_ask(request),
            AgentEvent::Question { question } => self.on_question(question),
            AgentEvent::QuestionResolved { id, .. } => self.on_question_resolved(id),
            AgentEvent::PermissionResolved { id, decision } => self.on_permission_resolved(id, decision),
            AgentEvent::Usage { turn, call, session, cost_usd } => self.on_usage(turn, call, session, cost_usd),
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
            } => self.on_context(
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
            ),
            AgentEvent::ContextMap { cells, zones, calls, call_tokens } => {
                self.on_context_map(cells, zones, calls, call_tokens)
            }
            AgentEvent::Curated { items } => self.note_curated(items),
            AgentEvent::Notice { problem: Some(problem), .. } => self.push_problem(problem),
            AgentEvent::Notice { level, text, .. } => self.notice(level, text),
            AgentEvent::UpdateAvailable { behind } => self.update_ready(behind),
            AgentEvent::ModelChanged { provider, model } => self.on_model_changed(provider, model),
            AgentEvent::ModelsListed { models } => self.on_models_listed(models),
            AgentEvent::SaveFailed { name, model, reason } => self.on_save_failed(name, model, reason),
            AgentEvent::PermissionModeChanged { mode } => self.on_permission_mode_changed(mode),
            AgentEvent::ShellOutput { command, output, exit_code } => {
                self.push(Block::Shell { command, output, exit_code });
            }
            AgentEvent::ModelLimits { provider, model, pricing, context_window, max_output, source } => {
                self.on_model_limits(provider, model, pricing, context_window, max_output, source)
            }
            AgentEvent::Budget { session_cost, day_cost, session_limit, day_limit } => {
                self.budget = Some((session_cost, day_cost, session_limit, day_limit));
            }
            AgentEvent::LocalServers { found } => self.on_local_servers(found),
            AgentEvent::LoginPrompt { url, code } => self.on_login_prompt(url, code),
            AgentEvent::LoggedIn { result, .. } => self.on_logged_in(result),
            AgentEvent::Probed { name, seq, result } => self.on_probed(name, seq, result),
            AgentEvent::ModelChecked { name, model, seq, check } => self.on_model_checked(name, model, seq, check),
            AgentEvent::Effort { current, levels, .. } => self.on_effort(current, levels),
            AgentEvent::Fast { on } => self.on_fast(on),
            AgentEvent::GoalLog(log) => self.goal_log = log,
            AgentEvent::Goal { text, round, max, state } => self.on_goal(text, round, max, state),
            AgentEvent::SystemPrompt { text } => self.on_system_prompt(text),
            AgentEvent::TurnEnd { reason } => self.on_turn_end(reason),
            AgentEvent::Suggestion { text } => self.on_suggestion(text),
            AgentEvent::HistoryCleared => self.on_history_cleared(),
            AgentEvent::SessionChanged { session_id, title } => self.on_session_changed(session_id, title),
            AgentEvent::SessionDeleted { id, title } => self.on_session_deleted(id, title),
            AgentEvent::SessionRestored { id, title } => self.on_session_restored(id, title),
            AgentEvent::SessionTree { entries } => self.on_session_tree(entries),
            AgentEvent::SessionList { sessions } => self.on_session_list(sessions),
            AgentEvent::Trash { sessions } => self.on_trash(sessions),
            AgentEvent::MemoryShared { pending } => self.on_memory_shared(pending),
            AgentEvent::Queued { pending } => self.on_queued(pending),
            AgentEvent::Jobs { jobs } => self.on_jobs(jobs),
            AgentEvent::Chrome(info) => self.on_chrome(info),
            AgentEvent::Mcp { servers } => self.on_mcp(servers),
            AgentEvent::JobTails { tails } => self.on_job_tails(tails),
            // a request taken and then stopped before its turn (a hook refused it): nothing works any more
            AgentEvent::Idle { .. } => self.on_idle(),
            AgentEvent::MailSent { id, to_name, to, summary, .. } => self.on_mail_sent(id, to, to_name, summary),
            AgentEvent::MailFailed { to, reason } => self.on_mail_failed(to, reason),
            AgentEvent::MailArrived { id, from, from_name, summary, held } => {
                self.on_mail_arrived(id, from, from_name, summary, held)
            }
            AgentEvent::MailDelivered { ids } => self.on_mail_delivered(ids),
            AgentEvent::MailRead { ids } => self.on_mail_read(ids),
            AgentEvent::MailStage { id, stage } => self.mail_stage(&id, stage),
            AgentEvent::Peers { peers } => self.on_peers(peers),
            AgentEvent::Retrying { attempt, retries, wait_secs, reason, .. } => {
                self.on_retrying(attempt, retries, wait_secs, reason)
            }
            AgentEvent::Compacted { .. } | AgentEvent::TodoChanged { .. } => {}
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
