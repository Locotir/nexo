//! Status messages, the frame tick and what the agent's events do to the state.

use super::*;

mod agent_events;

impl App {
    pub(super) fn set_skills(&mut self, skills: Vec<nexo_core::skills::Skill>) {
        for s in &skills {
            let name = s.name.to_ascii_lowercase();
            if crate::commands::lookup(&name).is_some() || self.custom_commands.iter().any(|c| c.name == name) {
                continue;
            }
            // the text is read when the command runs, so an edited skill is used as it is then
            self.custom_commands.push(crate::custom::CustomCommand {
                name,
                description: s.description.clone(),
                body: String::new(),
                path: s.file(),
                hint: String::new(),
                allowed_tools: String::new(),
                origin: "skill".into(),
            });
        }
        self.skills = skills;
    }

    /// `/skills`: what was found and where from.
    pub(super) fn show_skills(&mut self) {
        if self.skills.is_empty() {
            self.notice(
                Level::Info,
                "no skills found · put a folder with a SKILL.md in .nexo/skills, .agents/skills or .claude/skills (project or home)",
            );
            return;
        }
        let mut lines = vec![format!("{}:", nexo_core::count(self.skills.len(), "skill"))];
        for s in &self.skills {
            lines.push(format!("  /{}  ({})  {}", s.name, s.source, s.description));
        }
        self.notice(Level::Info, lines.join("\n"));
    }

    pub fn flash(&mut self, text: &str) {
        self.flash_level(Level::Info, text);
    }

    /// A flash message that says how serious it is (warn and error are coloured; error stays longer).
    pub fn flash_level(&mut self, level: Level, text: &str) {
        self.status = Some((text.to_string(), self.now()));
        self.status_level = level;
        self.status_toasted = false;
        self.dirty = true;
    }

    /// Drops the flash if it is a progress text ("exporting…", "compacting…").
    pub fn clear_progress_flash(&mut self) {
        if self.status.as_ref().is_some_and(|(t, _)| t.ends_with('…')) {
            self.status = None;
        }
    }

    /// Drops a flash whose time is up, even when no tick came to do it.
    pub fn expire_status(&mut self) {
        if let Some((_, t)) = &self.status {
            if self.now().saturating_duration_since(*t).as_millis() >= status_life(self.status_level) as u128 {
                self.status = None;
            }
        }
    }

    /// A newer nexo is on `main`: a card in the conversation, a toast, and a badge that stays
    /// in the title bar until nexo restarts.
    /// nexo checks for a newer release at start and then every hour: the card is shown once, and a
    /// later check that finds still more commits only says so in a toast.
    pub fn update_ready(&mut self, behind: u32) {
        if behind == 0 || self.update_behind.is_some_and(|b| b >= behind) {
            return;
        }
        let first = self.update_behind.is_none();
        self.update_behind = Some(behind);
        if first {
            self.push(Block::Update { behind });
            self.toast(crate::toast::Kind::Info, "nexo update ready: run `nexo update`");
        } else {
            let s = if behind == 1 { "" } else { "s" };
            self.toast(crate::toast::Kind::Info, &format!("nexo update ready: {behind} new commit{s} · `nexo update`"));
        }
    }

    /// A failure as a card. The same one again in the same turn is counted on the card it repeats.
    fn push_problem(&mut self, problem: Box<nexo_core::Problem>) {
        let turn = self.blocks.iter().rposition(|b| matches!(b, Block::User { .. })).map_or(0, |i| i + 1);
        let same = (turn..self.blocks.len())
            .rev()
            .find(|&i| matches!(&self.blocks[i], Block::Problem { problem: p, .. } if **p == *problem));
        match same {
            Some(i) => {
                if let Block::Problem { count, .. } = &mut self.blocks[i] {
                    *count += 1;
                }
                self.invalidate(i);
            }
            None => {
                self.push(Block::Problem { problem, count: 1, folded: true });
            }
        }
    }

    pub fn notice(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        // an error is a card; one with no cause of its own (an old session) has its text as the title
        if level == Level::Error {
            self.push_problem(Box::new(nexo_core::Problem::from_text(&text)));
        } else {
            self.push(Block::Notice { level, text });
        }
    }

    /// Reads the sessions for the usage window's insights when its tab is on show, and takes the result.
    pub fn poll_insights(&mut self) {
        if matches!(&self.modal, Some(Modal::Info(i)) if i.tab == 1) {
            self.insights.ensure();
        }
        if self.insights.poll() {
            self.dirty = true;
        }
    }

    /// Runs the bar's script (`[ui] status_command`) when a turn ended or the model changed,
    /// at most once a second, and takes its line when it comes.
    pub fn poll_status_cmd(&mut self) {
        let Some(mut sc) = self.status_cmd.take() else { return };
        let key = format!(
            "{}/{}|{}|{}",
            self.provider,
            self.model,
            self.turns_ended,
            self.session_id.as_deref().unwrap_or("")
        );
        if sc.poll(&key, || self.status_json()) {
            self.dirty = true;
        }
        self.status_cmd = Some(sc);
    }

    /// What the bar's script gets on stdin: one JSON line with what nexo knows of the session.
    pub fn status_json(&self) -> String {
        let (used, window) = match self.gauge() {
            Some((n, d)) => (n, Some(d)),
            None => (self.ctx_tokens().unwrap_or(0), None),
        };
        let percent = window.map(|d| (used as f64 * 100.0 / d as f64).round() as u64);
        let u = &self.session_usage;
        let v = serde_json::json!({
            "session_id": self.session_id,
            "cwd": self.cwd,
            "branch": self.branch,
            "model": { "id": self.model, "provider": self.provider, "effort": self.effort },
            "permission_mode": self.permission_mode.as_str(),
            "context": { "used_tokens": used, "window": window, "percent": percent },
            "tokens": {
                "input": u.input,
                "output": u.output,
                "cache_read": u.cache_read,
                "cache_write": u.cache_write,
                "total": u.total_input() + u.output,
            },
            "cost": { "usd": self.cost, "limit_usd": self.budget.and_then(|(_, _, l, _)| l) },
            "busy": self.busy,
            "turns": self.turns_ended,
            "version": nexo_core::version::get(),
        });
        format!("{v}\n")
    }

    /// The poller behind `/panel`: the fleet watch of the status bar, completed with the other tool's sessions,
    /// the judge and the mail on first use.
    pub(super) fn panel_handle(&mut self) -> std::sync::Arc<nexo_supervise::Panel> {
        self.panel_with(true)
    }

    /// Starts the watching that feeds the status bar's fleet chip when this process has a mailbox and the mail
    /// root exists: the heartbeats and the lane files only, read once a second, never a model call.
    pub(super) fn start_fleet_watch(&mut self) {
        if self.panel.is_none() && crate::mailbox().is_some() && nexo_core::mailbox::mail_root().is_dir() {
            self.panel_with(false);
        }
    }

    /// The panel's poller. `full` adds what only `/panel` needs: the other tool's logs and list, the mail ledgers
    /// and the judge (a model), once; without it the sources are the fleet's own files.
    fn panel_with(&mut self, full: bool) -> std::sync::Arc<nexo_supervise::Panel> {
        use nexo_supervise::{run::Setup, Policy};
        let p = match &self.panel {
            Some(p) => p.clone(),
            None => {
                let user = nexo_core::paths::config_dir().join("supervisor.toml");
                let project = std::path::Path::new(".nexo/supervisor.toml");
                let loaded = Policy::load(Some(&user), Some(project));
                self.panel_notes = loaded.errors.iter().map(|e| e.to_string()).collect();
                let policy = loaded.policy;
                let setup = Setup::none();
                let started = nexo_supervise::Panel::start_with(
                    &setup,
                    policy.clone(),
                    self.nexo_src.clone(),
                    &crate::supervise_options(),
                )
                .unwrap_or_else(|_| nexo_supervise::Panel::start(&setup, policy.clone(), self.nexo_src.clone()));
                for src in Setup::fleet_sources() {
                    started.add_source(src);
                }
                started.set_notes(self.panel_notes.clone());
                self.panel_policy = Some(policy);
                let p = std::sync::Arc::new(started);
                self.panel = Some(p.clone());
                p
            }
        };
        if full && !self.panel_full {
            self.panel_full = true;
            let policy = self.panel_policy.clone().unwrap_or_default();
            for src in Setup::detect(&policy).other_tool_sources() {
                p.add_source(src);
            }
            if let Some(m) = policy.model.clone() {
                match tokio::runtime::Handle::try_current() {
                    Ok(rt) => {
                        let client = nexo_supervise::model::default_client();
                        match nexo_supervise::model::judge_for(&m, &self.opts.providers, client, rt) {
                            Ok(j) => p.set_judge(j),
                            Err(e) => self.panel_notes.push(e),
                        }
                    }
                    Err(_) => self.panel_notes.push("the model needs the async runtime, which is not running".into()),
                }
            }
            // the other sessions' mail (the ledgers and receipts on disk), only where this process has a mailbox
            if crate::mailbox().is_some() {
                p.add_source(Box::new(nexo_supervise::source_mail::MailSource::new(nexo_core::mailbox::mail_root())));
            }
            p.set_notes(self.panel_notes.clone());
        }
        p
    }

    pub fn tick(&mut self) {
        self.tick_frame(true)
    }

    /// What the supervisor wants said to this session, once no turn is running: the person's own panel message goes
    /// in as their message; the rest becomes evidence for the session's own check-in.
    pub(super) fn deliver_supervisor_nudges(&mut self) {
        if self.busy || !self.nexo_src.has_nudges() {
            return;
        }
        for n in self.nexo_src.take_nudges() {
            if n.manual {
                self.send(AgentCommand::Submit { text: n.text, attachments: vec![] });
                self.flash("your message was sent to this session");
            } else {
                self.send(AgentCommand::CheckIn { evidence: n.text });
                self.flash("supervisor: checking in with this session");
            }
        }
    }

    /// One tick of the clock; `beat` steps the spinner (the loop ticks faster while things glide).
    /// About once a second: the mailbox beats, takes in what arrived, and hands the agent the mails
    /// to read (only when it said it is idle); what it saw is shown like any other agent event.
    pub(super) fn poll_mailbox(&mut self) {
        let Some(m) = crate::mailbox() else { return };
        let now = self.now();
        if self.mail_polled.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_millis(900)) {
            return;
        }
        self.mail_polled = Some(now);
        m.set_jobs(&self.jobs);
        let t = m.tick();
        for ev in t.events {
            self.handle_agent(ev);
        }
        if !t.deliver.is_empty() {
            self.send(AgentCommand::Deliver { mails: t.deliver });
        }
    }

    pub fn tick_frame(&mut self, beat: bool) {
        self.sub_tick();
        let t = crate::trace::span("index");
        let waiting = self.words.pending();
        let typing = self.last_key.is_some_and(|t| self.now().saturating_duration_since(t) < crate::complete::TYPING);
        let budget = if typing { crate::complete::BUSY_BUDGET } else { crate::complete::IDLE_BUDGET };
        self.words.pump_for(budget);
        if waiting > 0 && self.words.pending() == 0 {
            crate::trace::totals("index done");
        }
        drop(t);
        if beat {
            self.spinner = self.spinner.wrapping_add(1);
        }
        self.deliver_supervisor_nudges();
        self.check_unechoed();
        self.poll_status_cmd();
        self.poll_mailbox();
        self.poll_chrome();
        self.poll_team();
        self.poll_fleet();
        self.poll_diffview();
        self.poll_insights();
        if !self.branch_read.is_some_and(|t| t.elapsed().as_secs() < 5) {
            self.read_branch();
        }
        self.sync_theme_preview();
        let mut any = false;
        let mut live = false;
        for i in 0..self.blocks.len() {
            if self.blocks[i].is_live() {
                self.cache[i] = None;
                live = true;
            }
        }
        // a block fades in as it arrives, and opens line by line
        let settle = FADE_MS.max(OPEN_MS) as u128 + 50;
        let now = self.now();
        if self.looks.iter().any(|l| {
            l.born.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= settle)
                || l.opened.is_some_and(|(t, _)| now.saturating_duration_since(t).as_millis() <= settle)
        }) {
            any = true;
        }
        // the banner's mark lights up during the first moments
        if self.intro_running() {
            self.cache[0] = None;
            any = true;
        }
        // cells of the context map that changed blink a moment
        // (it decays on the beat: a frame per tick, not 60 a second)
        if self.map_flash_live() {
            self.dirty = true;
        }
        // the curator's label fades back to its own colour after a cut, on the beat too
        if self.cut_fading() {
            self.dirty = true;
        }
        // the gauge eases towards the real share
        {
            let (num, den, _) = self.gauge_view();
            let target = (num as f64 / den.max(1) as f64).min(1.0);
            let d = target - self.gauge_shown;
            if d.abs() > 0.002 && self.opts.animations {
                self.gauge_shown += d * 0.25;
                any = true;
            } else {
                self.gauge_shown = target;
            }
        }
        // the cost rolls up to what was spent, like a meter
        // (and the tokens, the toasts and the turn-end glow)
        if self.tick_fx_with(false) {
            any = true;
        }
        // the welcome figure and the stars of an empty session move on the beat: a frame per tick, not 60 a second
        if self.stars_alive(self.now()) || self.home_alive(self.now()) {
            self.dirty = true;
        }
        // the thinking header settles when the reasoning closes, and the rail of an interrupted block warms
        any |= self.stream_tick();
        // tool cards: a finished one pops and eases its rail
        {
            let now = self.now();
            let on = self.opts.animations;
            let settle = crate::motion::RAIL_MS.max(crate::motion::POP_MS) + 30;
            // a group is live while one of its calls is: its line shows the spinner and the clock
            let mut live_groups: Vec<usize> = Vec::new();
            for i in 0..self.blocks.len() {
                if self.blocks[i].is_live() {
                    if let Some(m) = self.group_of(i) {
                        if live_groups.last() != Some(&m) {
                            live_groups.push(m);
                        }
                        self.cache[m] = None;
                    }
                }
            }
            for i in 0..self.blocks.len() {
                if !matches!(self.blocks[i], Block::Tool { .. } | Block::Group { .. }) {
                    continue;
                }
                let live = match self.blocks[i] {
                    Block::Group { .. } => live_groups.contains(&i),
                    _ => self.blocks[i].is_live(),
                };
                let look = &mut self.looks[i];
                if look.was_live && !live && on {
                    look.done_at = Some(now);
                    self.cache[i] = None;
                }
                look.was_live = live;
                if live && on {
                    any = true;
                } else if crate::motion::within(look.done_at, now, settle) {
                    self.cache[i] = None;
                    any = true;
                } else if look.done_at.take().is_some() {
                    self.cache[i] = None;
                }
            }
        }
        // the panel: new rows from the poller, and breathing chips
        let at = self.now();
        if let Some(Modal::Agents(v)) = &mut self.modal {
            if v.refresh() || v.animating(at) {
                any = true;
                self.dirty = true;
            }
        }
        // the todo window: the bar fills, a task that changed glows, the one being worked on spins
        if let Some(Modal::Todo(v)) = &self.modal {
            let live = self.opts.animations && v.order().iter().any(|&i| v.item_running(i));
            if v.animating(at) || live {
                any = true;
                self.dirty = true;
            }
        }
        // the goal window's clock
        if let Some(Modal::Goal(v)) = &mut self.modal {
            if v.tick() {
                self.dirty = true;
            }
        }
        // the panel knows which of its jobs are sub-agents it can write to, and reads the output of the one it shows
        if let Some(Modal::Agents(v)) = &mut self.modal {
            let runs = self
                .sub_runs
                .iter()
                .filter(|r| r.info.state.running())
                .filter_map(|r| Some((crate::app::sub::job_of_run(&r.info.id)?, r.info.id.clone())))
                .collect();
            v.set_runs(runs);
            if v.wants_tails(at) {
                self.commands.send(AgentCommand::JobTails).ok();
            }
        }
        // the jobs window asks for fresh output now and then
        if let Some(Modal::Jobs(v)) = &mut self.modal {
            if let Some(cmd) = v.refresh() {
                self.commands.send(cmd).ok();
            }
        }
        // the usage window grows open and its figures fill in
        if matches!(&self.modal, Some(Modal::Info(i)) if i.animating()) {
            any = true;
        }
        if self.insights.running() && matches!(&self.modal, Some(Modal::Info(_))) {
            any = true;
        }
        // the transcript glides, the thumb beats, a palette or a dialog pops open
        let now = self.now();
        if self.scroll_fx.moving(now)
            || self.unseen_at.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= anim::PULSE_MS as u128)
            || [self.palette_at, self.modal_at]
                .iter()
                .any(|t| t.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= anim::POP_MS as u128 + 50))
        {
            any = true;
        }
        // the frame where a motion ends is drawn too, so nothing rests half lit
        if self.moving && !any {
            self.dirty = true;
        }
        self.moving = any;
        if any || live || self.busy || self.context_wait {
            self.dirty = true;
        }
        if let Some((_, t)) = &self.status {
            if self.now().saturating_duration_since(*t).as_millis() >= status_life(self.status_level) as u128 {
                self.status = None;
                self.dirty = true;
            }
            // a message fades out: every frame of it is drawn (with animations off, only its end)
            if self.opts.animations {
                self.dirty = true;
            }
        }
    }

    /// What the screen asks of the clock now.
    pub fn load(&self) -> crate::frame::Load {
        use crate::frame::Load;
        // words waiting for the index come a slice per tick: a slow tick would leave the completions half built
        // the banner's intro moves from the first frame, before any tick has set `moving`
        // the crest on the work line crosses the rule in 2 s: a frame every 16 ms while it is on the rule,
        // the beat while it rests off it
        if self.words.pending() > 0 {
            Load::Work
        } else if self.moving || self.intro_running() || (self.busy && self.fx_on() && self.crest_on_rule()) {
            Load::Motion
        } else if self.busy
            || self.map_flash_live()
            || self.cut_fading()
            // a toast waits for its time on the beat; it asks for frames only while it moves
            || self.toasts.alive(self.now())
            || self.status.is_some()
            // the welcome figure turns on the beat, and the stars of an empty session drift on it
            || self.stars_alive(self.now())
            || self.home_alive(self.now())
            // the memory figure waits on the curator: its spinner needs the beat
            || self.context_wait
            // a request sent that the agent has not echoed: the clock must wake to say so
            || self.unechoed.iter().any(|(_, _, said)| !said)
            // the answer of the /diff collection is taken on a tick: an idle clock would hold it for seconds
            || self.diffview.as_ref().is_some_and(|v| v.loading)
            || self.tail_is_live()
        {
            Load::Beat
        } else {
            Load::Idle
        }
    }

    /// The crest of the work line is crossing the rule now (not resting between two passes).
    fn crest_on_rule(&self) -> bool {
        let secs = self.turn_started.map_or(0.0, |t| self.now().saturating_duration_since(t).as_secs_f32());
        crate::ui::crest_passing(secs)
    }

    /// A cell of the context map that just changed is still lit: it decays on the beat.
    pub(super) fn map_flash_live(&self) -> bool {
        self.opts.animations && self.show_live && crate::motion::within(self.map_at, self.now(), MAP_FLASH_MS + 100)
    }

    /// Whether a block of the current turn still moves by itself (PF3: only the
    /// tail after the last user message is scanned, never the whole session).
    pub(super) fn tail_is_live(&self) -> bool {
        self.blocks.iter().rev().take_while(|b| !matches!(b, Block::User { .. })).any(|b| b.is_live())
    }

    /// The turn has waited 15 s or more on a provider that runs on this machine.
    fn long_local_wait(&self) -> bool {
        let waited = self.turn_started.is_some_and(|t| self.now().saturating_duration_since(t).as_secs() >= 15);
        waited
            && self.opts.providers.iter().any(|(name, c)| {
                let url = c.base_url.to_ascii_lowercase();
                let host = url.split("://").last().unwrap_or("");
                *name == self.provider && ["localhost", "127.", "[::1]", "0.0.0.0"].iter().any(|h| host.starts_with(h))
            })
    }

    /// What the agent is doing now, as the activity line words it.
    pub fn activity(&self) -> Option<String> {
        if !self.busy {
            return None;
        }
        if matches!(self.modal, Some(Modal::Permission(_))) {
            return Some(crate::render::phase::Phase::Waiting.label().into());
        }
        let live = self.blocks.iter().rev().find(|b| b.is_live());
        use crate::render::phase::{phase_of_tool, Phase};
        Some(
            match live {
                // the model writes the goal again in its own words: the goal shown changes with it
                Some(Block::Tool { name, .. }) if name == "goal_brief" => return Some("Restating the goal".into()),
                Some(Block::Tool { name, .. }) => phase_of_tool(name),
                Some(Block::Assistant { .. }) => Phase::Writing,
                // before the reply's first word: what the agent said it is doing
                _ => match self.prep.as_deref() {
                    // a cold local server loads the model before it answers: say so once the wait is long
                    Some(what) if what == "waiting for the model" && self.long_local_wait() => {
                        return Some("Waiting for the model: the local server may be loading it".into())
                    }
                    Some(what) => return Some(capitalised(what)),
                    None => Phase::Thinking,
                },
            }
            .label()
            .into(),
        )
    }

    pub fn send(&self, cmd: AgentCommand) {
        crate::trace::command(&cmd);
        let _ = self.commands.send(cmd);
    }

    // ------------------------------------------------------------ agent events

    /// Handles `first` and the events queued behind it, for `budget` at most: a resumed history of
    /// thousands of events is shown as it arrives and a key typed meanwhile waits one slice, not the
    /// whole burst. What is left stays queued for the next pass. Returns how many it handled.
    pub fn drain_events(
        &mut self,
        first: AgentEvent,
        rx: &mut nexo_core::agent::EventRx,
        budget: std::time::Duration,
    ) -> usize {
        let until = std::time::Instant::now() + budget;
        self.handle_agent(first);
        let mut n = 1;
        while std::time::Instant::now() < until {
            let Ok(e) = rx.try_recv() else { break };
            self.handle_agent(e);
            n += 1;
        }
        n
    }

    pub fn handle_agent(&mut self, ev: AgentEvent) {
        crate::trace::agent_event(&ev);
        let _t = crate::trace::span("events");
        // the conversations of sub-agents draw nothing, and cause no redraw, unless one is open
        if ev.is_sub() {
            return self.handle_sub(ev);
        }
        // a cleared session has no runs; anything else is the main chat's, whichever view is open
        if matches!(ev, AgentEvent::HistoryCleared) {
            self.close_sub();
            self.sub_runs.clear();
        }
        if self.in_sub() {
            return self.on_main(|a| a.handle_main(ev));
        }
        self.handle_main(ev)
    }

    /// A message of the transcript moved on the receipt ladder (never back).
    pub(super) fn mail_stage(&mut self, id: &str, to: nexo_core::mail::Stage) {
        for i in 0..self.blocks.len() {
            if let Block::Mail { id: bid, stage, .. } = &mut self.blocks[i] {
                if bid == id && stage.advances_to(to) {
                    *stage = to;
                    self.invalidate(i);
                }
            }
        }
    }

    /// What nexo added to the message just shown: one folded line under it. Its mentions were put at the
    /// end of the person's text, so they leave it and are said in that line.
    pub(super) fn added(&mut self, parts: Vec<nexo_core::agent::AddedPart>) {
        use nexo_core::agent::AddedKind;
        if parts.is_empty() {
            return;
        }
        // what a message told as it was built is exact: it takes the place of what was read back from its
        // text; what rides one request only is a line of its own where it came
        let (once, parts): (Vec<_>, Vec<_>) = parts.into_iter().partition(|p| p.request_only);
        if !parts.is_empty() {
            if let Some(i) = self.blocks.iter().rposition(|b| matches!(b, Block::User { .. })) {
                if let Block::User { text, .. } = &mut self.blocks[i] {
                    for p in parts.iter().filter(|p| matches!(p.kind, AddedKind::Mention)) {
                        let left = text.replacen(p.text.trim(), "", 1);
                        if !left.trim().is_empty() {
                            *text = left.trim_end().to_string();
                        }
                    }
                }
                let block = Block::Added { parts, folded: true };
                // the line read back from the message's text, when the history drew one: not always the
                // block next to the message (a message that waited has the warning and the agent's notices
                // under its dim line, which is where it was put back), so it is looked for up to the next message
                let read_back = self.blocks[i + 1..]
                    .iter()
                    .take_while(|b| !matches!(b, Block::User { .. }))
                    .position(|b| matches!(b, Block::Added { parts, .. } if !parts.iter().all(|p| p.request_only)));
                if let Some(at) = read_back.map(|k| i + 1 + k) {
                    self.blocks[at] = block;
                    self.invalidate(at);
                } else {
                    self.invalidate(i);
                    self.push(block);
                }
            } else {
                self.push(Block::Added { parts, folded: true });
            }
        }
        if !once.is_empty() {
            self.push(Block::Added { parts: once, folded: true });
        }
    }

    pub(super) fn history(&mut self, index: usize, m: Message) {
        match m.role {
            Role::User => {
                if m.is_tool_results() {
                    for p in &m.content {
                        if let Part::ToolResult { call_id, content, is_error, .. } = p {
                            if let Some(i) = self.tool_index(call_id) {
                                if let Block::Tool { output, status, .. } = &mut self.blocks[i] {
                                    if output.is_none() {
                                        let text = result_text(content);
                                        self.words.feed(&text, crate::complete::Src::Other);
                                        *status = if *is_error { ToolStatus::Failed } else { ToolStatus::Done };
                                        *output = Some(ToolOutput {
                                            content: text,
                                            is_error: *is_error,
                                            display: ToolDisplay::Plain,
                                            images: Vec::new(),
                                        });
                                    }
                                }
                                self.note_change(i);
                                self.invalidate(i);
                            }
                        }
                    }
                } else {
                    let text = m.text();
                    for p in crate::replay::split_user(&text) {
                        if let crate::replay::Piece::Text(t) = p {
                            self.words.feed(&t, crate::complete::Src::User);
                        }
                    }
                    self.user_messages.retain(|(i, _)| *i != index);
                    // the loop's own goal requests are not the person's: nothing to rewind to or repeat
                    if !text.starts_with(nexo_core::agent::GOAL_SET_MARK)
                        && text.trim() != nexo_core::agent::GOAL_CONTINUE_MARK
                        && text.trim() != nexo_core::agent::SUPERVISOR_RESUME_MARK
                    {
                        // /undo puts back, and the pickers list, what the person wrote, not what nexo put with it
                        self.user_messages.push((index, nexo_core::agent::person_words(&text).to_string()));
                    }
                    let words = nexo_core::agent::person_words(&text);
                    let at = match self.pending_stamps.iter().position(|t| t == words) {
                        Some(i) => {
                            self.pending_stamps.remove(i);
                            Some(self.stamp_now())
                        }
                        None => None,
                    };
                    let mut pending = self.take_pending(words);
                    let pieces = crate::replay::split_user(&text);
                    if pieces.len() == 1 && matches!(&pieces[0], crate::replay::Piece::Text(t) if *t == text) {
                        self.put_user(text, at, &mut pending);
                    } else {
                        let mut at = at;
                        // what nexo put around the words stays behind one folded line, not in the message
                        let mut extra = Vec::new();
                        for p in pieces {
                            use crate::replay::Piece;
                            match p {
                                Piece::JobNote(n) => {
                                    self.push(Block::User { text: n, at: None });
                                }
                                Piece::Added(part) => extra.push(part),
                                Piece::Summary(n) => {
                                    self.push(Block::Notice {
                                        level: Level::Info,
                                        text: format!("earlier conversation summarised · {n} lines"),
                                    });
                                }
                                Piece::Goal(words) => {
                                    let text = match words {
                                        Some(w) => format!("◎ goal set: {}", nexo_core::agent::goal_shown(&w)),
                                        None => match self.goal_round_said.take() {
                                            Some((round, max)) => format!("◎ goal · round {round}/{max} · continuing"),
                                            None => "◎ goal · continuing".to_string(),
                                        },
                                    };
                                    self.push(Block::Notice { level: Level::Info, text });
                                }
                                Piece::Text(t) => self.put_user(t, at.take(), &mut pending),
                            }
                        }
                        // the words were sent but the echo has none of its own: the dim line becomes the message
                        if pending.is_some() {
                            self.put_user(words.to_string(), None, &mut pending);
                        }
                        if !extra.is_empty() {
                            self.push(Block::Added { parts: extra, folded: true });
                        }
                    }
                }
            }
            Role::Assistant => {
                for p in m.content {
                    match p {
                        Part::Text { text } => {
                            if !text.trim().is_empty() {
                                self.push(Block::Assistant {
                                    text,
                                    streaming: false,
                                    at: None,
                                    started: None,
                                    took_ms: None,
                                });
                            }
                        }
                        Part::Reasoning { text, .. } => {
                            if !text.trim().is_empty() {
                                self.push(Block::Reasoning {
                                    text,
                                    streaming: false,
                                    started: None,
                                    elapsed_ms: None,
                                    folded: true,
                                });
                            }
                        }
                        Part::ToolCall { id, name, args } => {
                            // a resumed session has the list only in the call that last wrote it
                            if name == "todo" {
                                if let Some(items) = crate::todo_strip::items_from_args(&args) {
                                    self.todo = items;
                                }
                            }
                            let summary = summarise(&name, &args);
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
                                folded: true,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Records what tool block `i` changed, for the changed-files panel: a diff
    /// display gives exact counts; a replayed result only its text (`edited
    /// path: +n −m lines`).
    pub(super) fn note_change(&mut self, i: usize) {
        let Some(Block::Tool { name, args, output: Some(out), .. }) = self.blocks.get(i) else { return };
        match &out.display {
            ToolDisplay::Diff { path, old, new, .. } => {
                let (a, d) = diff::stats(old, new);
                let path = path.clone();
                self.upsert_change(path, a, d);
            }
            _ if !out.is_error && matches!(name.as_str(), "edit" | "write") => {
                let Some(path) =
                    args.get("path").or_else(|| args.get("file_path")).and_then(|v| v.as_str()).map(String::from)
                else {
                    return;
                };
                let (a, d) = parse_counts(&out.content).unwrap_or((0, 0));
                self.upsert_change(path, a, d);
            }
            _ => {}
        }
    }

    pub(super) fn upsert_change(&mut self, path: String, added: usize, removed: usize) {
        let (mut a, mut d) = (added, removed);
        if let Some(pos) = self.changed.iter().position(|c| c.path == path) {
            let c = self.changed.remove(pos);
            a += c.added;
            d += c.removed;
        }
        self.changed.insert(0, ChangedFile { path, added: a, removed: d });
        self.dirty = true;
    }
}

impl App {
    /// Notices a reasoning block that just closed and keeps the blocks that are still moving fresh
    /// (`fx_stream`); true while one is.
    fn stream_tick(&mut self) -> bool {
        use crate::fx_stream as fx;
        let (now, on) = (self.now(), self.fx_on());
        if !self.blocks.iter().any(|b| matches!(b, Block::Assistant { streaming: true, .. })) {
            self.fx.ink.clear();
        }
        let mut any = false;
        for i in 0..self.blocks.len() {
            if let Block::Reasoning { streaming, .. } = &self.blocks[i] {
                let look = &mut self.looks[i];
                if look.was_live && !*streaming && on {
                    look.ended = Some(now);
                }
                look.was_live = *streaming;
            }
            let look = self.looks[i];
            let moving = on
                && (crate::motion::within(look.ended, now, fx::THINK_MS)
                    || crate::motion::within(look.stopped, now, fx::STOP_MS));
            if moving {
                self.cache[i] = None;
                any = true;
            } else if look.ended.is_some() || look.stopped.is_some() {
                // the last frame is the settled one
                let look = &mut self.looks[i];
                look.ended = None;
                look.stopped = None;
                self.cache[i] = None;
            }
        }
        any
    }

    /// A turn was interrupted: the rail of the block it stopped in warms once.
    fn stop_rail(&mut self) {
        if !self.fx_on() {
            return;
        }
        let now = self.now();
        let from = self.blocks.iter().rposition(|b| matches!(b, Block::User { .. })).unwrap_or(0);
        // a tool after the text means the turn stopped inside that tool, not in the text
        if let Some(i) = (from..self.blocks.len())
            .rev()
            .find(|i| matches!(self.blocks[*i], Block::Assistant { .. } | Block::Reasoning { .. }))
            .filter(|i| !self.blocks[*i + 1..].iter().any(|b| matches!(b, Block::Tool { .. })))
        {
            self.looks[i].stopped = Some(now);
            self.cache[i] = None;
        }
    }
}

/// `what` with its first letter in capitals, as the work line's verbs are.
fn capitalised(what: &str) -> String {
    let mut c = what.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}
