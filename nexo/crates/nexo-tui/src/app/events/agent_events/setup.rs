use super::*;

impl App {
    pub(super) fn on_model_changed(&mut self, provider: String, model: String) {
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
        if self.provider != provider || self.model != model {
            self.cache_baseline = None;
            self.cache_miss_fresh = None;
        }
        self.provider = provider;
        self.model = model;
        self.limits = None;
        self.refresh_welcome();
    }

    pub(super) fn on_models_listed(&mut self, models: Vec<(String, String)>) {
        // what the next `/model` offers; a pair the person has but the list lacks stays
        let mut all = models;
        for pair in std::mem::take(&mut self.opts.models) {
            if !all.contains(&pair) {
                all.push(pair);
            }
        }
        self.opts.models = all;
    }

    pub(super) fn on_save_failed(&mut self, name: String, model: String, reason: String) {
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

    pub(super) fn on_permission_mode_changed(&mut self, mode: PermissionMode) {
        if mode != self.permission_mode && self.opts.animations {
            self.fx.mode_from = Some(self.permission_mode);
            self.fx.mode_at = Some(self.now());
        }
        self.permission_mode = mode;
        self.refresh_settings();
        match mode {
            PermissionMode::Plan => self.flash("plan mode: reads only, then a plan to approve · Shift+Tab leaves"),
            _ => self.flash(&format!("permissions: {}", mode.as_str())),
        }
        self.refresh_welcome();
    }

    pub(super) fn on_model_limits(
        &mut self,
        provider: String,
        model: String,
        pricing: Option<Pricing>,
        context_window: Option<u32>,
        max_output: Option<u32>,
        source: String,
    ) {
        if self.nothing_chosen() {
            return;
        }
        if self.provider != provider || self.model != model {
            self.cache_baseline = None;
            self.cache_miss_fresh = None;
        }
        self.provider = provider.clone();
        self.model = model.clone();
        self.limits = Some(Limits { pricing, context_window, max_output, source: source.clone() });
        // Only tool price tags and group API-value labels use the current model.
        // Both decorate an existing header without adding rows. Keep their heights:
        // discarding them makes layout re-render even far-away, evicted tool output.
        // Drop lines and hit regions so visible cards use the new prices immediately.
        for i in 0..self.blocks.len() {
            let affected = match &self.blocks[i] {
                Block::Tool { id, .. } => self.marks.contains_key(id),
                Block::Group { .. } => true,
                _ => false,
            };
            if affected {
                if let Some((_, _, lines, meta)) = self.cache[i].as_mut() {
                    *lines = None;
                    *meta = Default::default();
                }
            }
        }
        self.dirty = true;
        self.refresh_welcome();
        // the card is worth a line after a change or a question, not at every start
        if !self.fresh() || source == "error" {
            self.push(Block::Model { provider, model, pricing, context_window, max_output, source });
        }
    }

    pub(super) fn on_local_servers(&mut self, found: Vec<(String, String)>) {
        self.scan = Some(found.clone());
        if self.locked && found.iter().any(|(p, _)| *p == self.provider) {
            self.unlock();
        }
        if let Some(Modal::Setup(s)) = &mut self.modal {
            s.found_local(found);
        }
    }

    pub(super) fn on_login_prompt(&mut self, url: String, code: String) {
        if let Some(Modal::Setup(s)) = &mut self.modal {
            s.login_prompt(url, code);
        }
    }

    pub(super) fn on_logged_in(&mut self, result: Result<(), String>) {
        if let Some(Modal::Setup(s)) = &mut self.modal {
            if let crate::modal::setup::SetupOut::Send(cmd) = s.logged_in(result) {
                self.send(cmd);
            }
        }
    }

    pub(super) fn on_probed(
        &mut self,
        name: String,
        seq: u64,
        result: Result<nexo_core::provider::ServerProbe, String>,
    ) {
        if let Some(Modal::Setup(s)) = &mut self.modal {
            s.probed_from(&name, seq, result);
        }
    }

    pub(super) fn on_model_checked(
        &mut self,
        name: String,
        model: String,
        seq: u64,
        check: nexo_core::provider::ModelCheck,
    ) {
        if let Some(Modal::Setup(s)) = &mut self.modal {
            s.checked_from(&name, &model, seq, check);
        }
    }

    pub(super) fn on_effort(&mut self, current: Option<String>, levels: Vec<String>) {
        if self.effort != current && current.is_some() && self.effort_levels == levels {
            self.flash(&format!("effort: {}", current.as_deref().unwrap_or("default")));
        }
        let (was_top, same_list) = (self.effort_top(), self.effort_levels == levels);
        self.effort = current;
        self.effort_levels = levels;
        self.note_effort_event(was_top, same_list);
    }

    pub(super) fn on_fast(&mut self, on: bool) {
        if self.fast != on {
            self.flash(if on { "fast: on" } else { "fast: off" });
        }
        self.fast = on;
    }
}
