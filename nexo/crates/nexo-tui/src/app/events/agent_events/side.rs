use super::*;

impl App {
    pub(super) fn on_sent(&mut self, model: String, sections: Vec<nexo_core::agent::SentSection>) {
        self.modal = Some(Modal::Sent(Box::new(crate::sent_view::SentView::new(model, sections))));
    }

    pub(super) fn on_permission_ask(&mut self, request: PermissionRequest) {
        if self.modal.is_none() {
            self.modal = Some(Modal::permission(request));
            self.permission_at = Some(self.now());
        } else {
            self.pending_permissions.push_back(request);
        }
    }

    pub(super) fn on_question(&mut self, question: nexo_core::permission::Question) {
        if self.modal.is_none() {
            self.modal = Some(Modal::question(question));
        } else {
            self.pending_questions.push_back(question);
        }
    }

    pub(super) fn on_question_resolved(&mut self, id: String) {
        self.pending_questions.retain(|q| q.id != id);
        if matches!(&self.modal, Some(Modal::Question(a)) if a.q.id == id) {
            self.modal = None;
            self.next_permission();
        }
    }

    pub(super) fn on_permission_resolved(&mut self, id: String, decision: PermissionDecision) {
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

    pub(super) fn on_goal(&mut self, text: Option<String>, round: u32, max: u32, state: GoalState) {
        // another goal than the one on show (the agent restating a goal is no other goal): it says its words, and
        // the state, the round and the outcome of the goal before it are none of its own
        let new_goal = matches!(
            (&text, &self.goal),
            (Some(t), Some(g)) if *t != g.text && !t.starts_with(nexo_core::agent::GOAL_BRIEF_HEAD)
        );
        match (&text, &self.goal) {
            (Some(t), Some(_)) if new_goal => self.flash(&format!("goal: {}", crate::render::wrap::truncate(t, 40))),
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
            _ if new_goal => None,
            (Some(_), Some(g), GoalState::Done) if g.state != GoalState::Done => Some(crate::fx_burst::Kind::Met),
            (Some(_), Some(g), GoalState::Paused(why)) if g.state.is_active() && crate::fx_burst::failed(why) => {
                Some(crate::fx_burst::Kind::Failed)
            }
            _ => None,
        };
        // the more rounds the goal took, the bigger the burst
        let rounds = round.max(self.goal.as_ref().map_or(0, |g| g.round));
        if let Some(kind) = outcome {
            self.burst_sized(kind, 1 + (rounds / 2).min(2) as u8);
        }
        self.goal = text.map(|text| Goal { text, round, max, state });
        if self.goal.is_none() {
            self.goal_log = Default::default();
        }
        self.sync_goal_view();
    }

    pub(crate) fn on_jobs(&mut self, jobs: Vec<nexo_core::jobs::JobInfo>) {
        for j in &jobs {
            let was_running = self.jobs.iter().any(|o| o.id == j.id && o.status.running());
            if was_running && !j.status.running() {
                let first: String = j.label.lines().next().unwrap_or("").chars().take(40).collect();
                // a toast with a mark says how it ended; a job the person stopped is no failure
                let (kind, word) = match &j.status {
                    nexo_core::jobs::JobStatus::Done { ok: true, .. } => (crate::toast::Kind::Success, "done"),
                    nexo_core::jobs::JobStatus::Killed => (crate::toast::Kind::Info, "stopped"),
                    _ => (crate::toast::Kind::Error, "failed"),
                };
                self.toast(kind, &format!("job {} {word} · {first}", j.id));
            }
        }
        if let Some(Modal::Jobs(v)) = &mut self.modal {
            if !v.is_monitors() {
                v.set_jobs(jobs.clone());
                self.commands.send(AgentCommand::JobTails).ok();
            }
        }
        self.jobs = jobs;
        self.sync_monitors();
        self.dirty = true;
    }

    pub(super) fn on_monitor_ids(&mut self, ids: Vec<u32>, states: Vec<(u32, nexo_core::agent::MonitorStatus)>) {
        self.monitor_ids = ids;
        self.monitor_states = states;
        self.sync_monitors();
    }

    fn sync_monitors(&mut self) {
        let monitors = self.jobs.iter().filter(|j| self.monitor_ids.contains(&j.id)).cloned().collect();
        let tails = self.monitor_tails.iter().filter(|t| self.monitor_ids.contains(&t.id)).cloned().collect();
        self.on_monitors(monitors, tails);
    }

    /// Apply one coherent monitor snapshot, preserving selection in an open window.
    pub fn on_monitors(&mut self, monitors: Vec<nexo_core::jobs::JobInfo>, tails: Vec<nexo_core::jobs::JobTail>) {
        if let Some(Modal::Jobs(v)) = &mut self.modal {
            if v.is_monitors() {
                v.set_jobs(monitors.clone());
                v.set_monitor_states(self.monitor_states.clone());
                v.set_tails(tails.clone());
            }
        }
        self.monitors = monitors;
        if let Some(Modal::Agents(v)) = &mut self.modal {
            v.set_monitor_count(self.monitors.len());
        }
        for back in &mut self.back {
            if let crate::app::types::Back::Window(Modal::Agents(v)) = back {
                v.set_monitor_count(self.monitors.len());
            }
        }
        self.dirty = true;
    }

    pub(super) fn on_chrome(&mut self, info: nexo_core::agent::ChromeInfo) {
        if let Some(Modal::Chrome(v)) = &mut self.modal {
            v.set(info);
            self.dirty = true;
        }
    }

    pub(super) fn on_mcp(&mut self, servers: Vec<nexo_core::agent::McpServerInfo>) {
        if let Some(Modal::Mcp(v)) = &mut self.modal {
            v.set_servers(servers.clone());
        }
        self.set_mcp(servers);
        self.dirty = true;
    }

    pub(super) fn on_job_tails(&mut self, tails: Vec<nexo_core::jobs::JobTail>) {
        // Keep the complete snapshot: monitor IDs may arrive after this event.
        self.monitor_tails = tails.clone();
        match &mut self.modal {
            Some(Modal::Jobs(v)) if v.is_monitors() => {
                v.set_tails(tails);
                self.dirty = true;
            }
            Some(Modal::Jobs(v)) => v.set_tails(tails),
            Some(Modal::Agents(v)) => {
                v.set_tails(tails);
                self.dirty = true;
            }
            _ => {}
        }
    }
}
