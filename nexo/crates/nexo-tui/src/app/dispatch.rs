//! Running a slash command, setup, models and themes.

use super::*;

impl App {
    pub fn run_command(&mut self, cmd: Command) {
        // the first screen runs what needs no model; the rest asks for one
        if self.locked && !cmd.works_without_model() {
            return self.open_setup();
        }
        match cmd {
            Command::Help => self.modal = Some(Modal::Help { scroll: 0 }),
            Command::Model(Some(name)) => {
                let (provider, model) = self.model_target(name);
                self.send(AgentCommand::SetModel { provider, model });
            }
            Command::Model(None) => self.open_setup(),
            Command::Login => {
                self.open_setup();
                self.flash("pick the provider to sign in or add its key");
            }
            Command::Logout(name) => self.logout(name.as_deref()),
            Command::Doctor => self.run_doctor(),
            Command::Effort(Some(level)) => {
                let level = level.trim().to_ascii_lowercase();
                let effort = (level != "default" && level != "reset").then_some(level);
                self.send(AgentCommand::SetEffort { effort });
            }
            Command::Effort(None) => self.open_effort_picker(),
            Command::Fast(arg) => match arg.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None => self.send(AgentCommand::SetFast { on: None }),
                Some("on") => self.send(AgentCommand::SetFast { on: Some(true) }),
                Some("off") => self.send(AgentCommand::SetFast { on: Some(false) }),
                Some(other) => self.notice(Level::Warn, format!("/fast takes on or off, not `{other}`")),
            },
            Command::Permissions(Some(mode)) => self.send(AgentCommand::SetPermissionMode(mode)),
            Command::Sandbox(mode) => self.send(AgentCommand::SetSandbox(mode)),
            Command::AddDir(arg) => match arg.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
                Some(dir) => self.send(AgentCommand::AddDir(dir.into())),
                None => self.notice(Level::Warn, "/add-dir <folder>: name the folder to add"),
            },
            Command::Plan(arg) => match arg.as_deref().map(str::trim) {
                Some("off") | Some("exit") | Some("done") => {
                    self.send(AgentCommand::SetPermissionMode(PermissionMode::Ask));
                }
                Some(text) if !text.is_empty() => {
                    if self.permission_mode != PermissionMode::Plan {
                        self.send(AgentCommand::SetPermissionMode(PermissionMode::Plan));
                    }
                    self.input.set_text(text);
                    self.submit();
                }
                _ if self.permission_mode == PermissionMode::Plan => {
                    self.notice(Level::Info, "already in plan mode · /plan off or Shift+Tab leaves it")
                }
                _ => self.send(AgentCommand::SetPermissionMode(PermissionMode::Plan)),
            },
            Command::Prompt => self.send(AgentCommand::ShowPrompt),
            Command::PromptEdit => self.prompt_edit(),
            Command::Memory { user } => self.memory_edit(user),
            Command::Remember(text) => self.remember(text.as_deref()),
            Command::Pricing(None) => self.send(AgentCommand::ShowModel),
            Command::Pricing(Some(args)) => match parse_pricing(&args) {
                Ok(pricing) => {
                    self.send(AgentCommand::SetPricing { pricing });
                    self.flash_toast("prices saved");
                }
                Err(e) => self.notice(Level::Warn, format!("/pricing: {e}")),
            },
            Command::Mouse => self.toggle_mouse(),
            Command::Timestamps => {
                self.timestamps = !self.timestamps;
                self.invalidate_all();
                self.flash(if self.timestamps { "timestamps on" } else { "timestamps off" });
            }
            Command::ContextLive => self.toggle_live(self.term_width),
            Command::Goal(GoalArg::Set(text)) => self.send(AgentCommand::SetGoal { text: Some(text) }),
            Command::Goal(GoalArg::Clear) => self.send(AgentCommand::SetGoal { text: None }),
            Command::Goal(GoalArg::Pause) => self.send(AgentCommand::PauseGoal),
            Command::Goal(GoalArg::Resume) => self.send(AgentCommand::ResumeGoal),
            Command::Goal(GoalArg::Show) => match &self.goal {
                Some(g) => {
                    let t = &g.text;
                    let text = match &g.state {
                        GoalState::Active if g.round > 0 => {
                            format!(
                                "goal: {t} (round {}/{}) · /goal pause holds it, /goal off clears it",
                                g.round, g.max
                            )
                        }
                        GoalState::Active => format!("goal: {t} · /goal pause holds it, /goal off clears it"),
                        GoalState::Waiting => {
                            format!("goal: {t} · waiting for your answer: your next message resumes it")
                        }
                        GoalState::Paused(why) => format!("goal: {t} · paused ({why}) · /goal resume goes on"),
                        GoalState::Done => format!("goal: {t} · met · /goal resume reopens it, /goal off clears it"),
                    };
                    self.notice(Level::Info, text)
                }
                None => self.notice(Level::Info, "no goal set: /goal <text> keeps the agent on it across turns"),
            },
            Command::Permissions(None) => {
                let items = [
                    PermissionMode::Ask,
                    PermissionMode::AcceptEdits,
                    PermissionMode::Plan,
                    PermissionMode::Auto,
                    PermissionMode::ReadOnly,
                ]
                .iter()
                .map(|m| PickItem {
                    label: m.as_str().to_string(),
                    value: m.as_str().to_string(),
                    hint: mode_hint(*m).to_string(),
                })
                .collect();
                let cur = self.permission_mode.as_str().to_string();
                self.modal =
                    Some(Modal::Picker(Picker::new(PickKind::PermissionMode, "Permission mode", items, Some(&cur))));
            }
            Command::Compact if self.user_messages.is_empty() => {
                self.notice(Level::Info, "nothing to compact");
            }
            Command::Compact => {
                self.send(AgentCommand::Compact);
                self.flash("compacting…");
            }
            Command::Clear => {
                self.clear_transcript();
                self.send(AgentCommand::Clear);
            }
            Command::Rewind => self.open_rewind_picker(),
            Command::Fork => self.open_fork_picker(),
            Command::Tree => self.send(AgentCommand::ShowTree),
            Command::Undo => self.undo(),
            Command::Init => {
                // the session is named for the command, not for the long prompt it sends
                if self.session_title.is_empty() {
                    self.send(AgentCommand::Rename { title: "/init".into() });
                }
                self.input.set_text(INIT_PROMPT);
                self.submit();
            }
            Command::Review(arg) => self.git_prompt(
                "/review",
                nexo_agent::gitcmds::Target::parse(arg.as_deref(), &self.cwd)
                    .and_then(|t| nexo_agent::gitcmds::review_prompt(&self.cwd, &t)),
            ),
            Command::Commit(arg) => {
                self.git_prompt("/commit", nexo_agent::gitcmds::commit_prompt(&self.cwd, arg.as_deref()))
            }
            Command::Pr(arg) => self.git_prompt("/pr", nexo_agent::gitcmds::pr_prompt(&self.cwd, arg.as_deref(), "gh")),
            Command::Status => self.send(AgentCommand::ShowStatus),
            Command::Tools => self.send(AgentCommand::ShowTools),
            Command::Sent => self.send(AgentCommand::ShowSent),
            Command::Hooks => self.send(AgentCommand::ShowHooks),
            Command::Mcp => self.open_mcp(),
            Command::Skills => self.show_skills(),
            Command::Plugins => {
                let text = nexo_core::plugins::describe(&nexo_core::paths::config_dir(), &Default::default());
                self.notice(Level::Info, text);
            }
            Command::Jobs(None) => self.open_jobs(),
            Command::Jobs(Some(id)) => self.send(AgentCommand::KillJob { id }),
            Command::Budget(None) => self.send(AgentCommand::ShowBudget),
            Command::Budget(Some(arg)) => {
                let a = arg.trim().trim_start_matches('$').to_ascii_lowercase();
                match a.as_str() {
                    "off" | "none" | "0" => self.send(AgentCommand::SetBudget { limit: None }),
                    _ => match a.replace(',', ".").parse::<f64>() {
                        Ok(v) if v > 0.0 && v.is_finite() => self.send(AgentCommand::SetBudget { limit: Some(v) }),
                        _ => self.notice(
                            Level::Warn,
                            format!("/budget: '{arg}' is not an amount of dollars (e.g. /budget 1.50, /budget off)"),
                        ),
                    },
                }
            }
            Command::Vim => {
                let on = self.input.vim.is_none();
                self.input.vim = on.then_some(crate::input::VimMode::Insert);
                let _ = crate::config_edit::set_raw(
                    &nexo_core::paths::user_config_file(),
                    "ui",
                    "vim",
                    if on { "true" } else { "false" },
                );
                self.flash(if on {
                    "vim keys on (saved): Esc for normal mode, i/a/o to type"
                } else {
                    "vim keys off (saved)"
                });
            }
            Command::Export(arg) => {
                // `/export html`, `/export json out.json`, `/export notes.md`
                let arg = arg.unwrap_or_default();
                let (first, rest) = arg.trim().split_once(char::is_whitespace).unwrap_or((arg.trim(), ""));
                let (format, path) =
                    if ["html", "md", "markdown", "json"].contains(&first.to_ascii_lowercase().as_str()) {
                        (Some(first.to_ascii_lowercase()), rest.trim())
                    } else {
                        (None, arg.trim())
                    };
                let path = (!path.is_empty()).then(|| PathBuf::from(path));
                self.send(AgentCommand::Export { path, format });
                self.flash("exporting…");
            }
            Command::Msg(None) => {
                self.send(AgentCommand::ListPeers);
                self.input.set_text("/msg ");
                self.refresh_palette();
            }
            Command::Msg(Some(arg)) => match arg.trim().split_once(char::is_whitespace) {
                Some((to, text)) if !text.trim().is_empty() => {
                    self.send(AgentCommand::SendMail { to: to.to_string(), text: text.trim().to_string() })
                }
                _ => self.notice(Level::Info, "write to a session: /msg <session> <text>"),
            },
            Command::Rename(None) => self.notice(Level::Info, "give the session a name: /rename <name>"),
            Command::Rename(Some(title)) => self.send(AgentCommand::Rename { title }),
            Command::Diff => self.open_diffview(),
            Command::Resume(what) => {
                let w = what.trim();
                let found = self.opts.sessions.iter().find(|(id, _)| id == w).or_else(|| {
                    let mut by_title = self.opts.sessions.iter().filter(|(_, t)| t.eq_ignore_ascii_case(w));
                    by_title.next().filter(|_| by_title.next().is_none())
                });
                match found {
                    Some((id, _)) => {
                        let id = id.clone();
                        self.send(AgentCommand::Resume { id });
                    }
                    None => self.notice(Level::Warn, format!("/resume: no session '{w}' here · /resume lists them")),
                }
            }
            Command::Sessions => {
                if self.opts.sessions.is_empty() {
                    self.notice(Level::Info, "no earlier sessions here");
                } else {
                    let items = self
                        .opts
                        .sessions
                        .iter()
                        .map(|(id, title)| PickItem { label: id.clone(), value: id.clone(), hint: title.clone() })
                        .collect();
                    self.modal = Some(Modal::Picker(Picker::new(PickKind::Session, "Sessions", items, None)));
                }
            }
            Command::Mem(None) => {
                let here = self.session_id.clone().unwrap_or_default();
                let items: Vec<PickItem> = self
                    .opts
                    .sessions
                    .iter()
                    .filter(|(id, _)| *id != here)
                    .map(|(id, title)| PickItem { label: id.clone(), value: id.clone(), hint: title.clone() })
                    .collect();
                if items.is_empty() {
                    self.notice(Level::Info, "no other session to share from");
                } else {
                    self.modal = Some(Modal::Picker(Picker::new(PickKind::Memory, "Share memory from", items, None)));
                }
            }
            Command::Mem(Some(arg)) if arg == "list" => self.open_shared(),
            Command::Todo => self.open_todo(),
            Command::Queue => self.open_queue(),
            Command::Mem(Some(arg)) => {
                let (id, focus) = match arg.split_once(char::is_whitespace) {
                    Some((i, f)) => (i.to_string(), Some(f.trim().to_string()).filter(|f| !f.is_empty())),
                    None => (arg.clone(), None),
                };
                if id == "off" || id == "clear" {
                    self.send(AgentCommand::ShareMemory { id: None, focus: None });
                } else {
                    self.send(AgentCommand::ShareMemory { id: Some(id), focus });
                }
            }
            Command::Undelete(id) => match id.or_else(|| self.trashed.last().cloned()) {
                Some(id) => self.send(AgentCommand::RestoreSession { id }),
                // nothing deleted in this run: show what the trash holds
                None => self.send(AgentCommand::ListTrash),
            },
            Command::Panel => {
                let panel = self.panel_handle();
                let view = crate::agents_view::AgentsView::new(Some(panel), !self.fx_on())
                    .remembering(self.panel_filter.clone());
                self.modal = Some(Modal::Agents(Box::new(view)))
            }
            Command::Cost => {
                self.insights.invalidate();
                self.modal = Some(Modal::Info(Box::new(crate::overlay::Info::new(1).still(!self.opts.animations))))
            }
            Command::Context => {
                self.insights.invalidate();
                self.modal = Some(Modal::Info(Box::new(crate::overlay::Info::new(0).still(!self.opts.animations))))
            }
            Command::Theme(Some(name)) => self.set_theme(&name, true),
            Command::Theme(None) => {
                let items = crate::theme::NAMES
                    .iter()
                    .map(|n| PickItem {
                        label: n.to_string(),
                        value: n.to_string(),
                        hint: Theme::by_name(n).map(|t| t.description.to_string()).unwrap_or_default(),
                    })
                    .collect();
                let cur = self.theme.name.to_string();
                self.theme_before_preview = Some(self.theme.clone());
                self.modal = Some(Modal::Picker(Picker::new(PickKind::Theme, "Theme", items, Some(&cur))));
            }
            Command::Copy(what) => self.copy_last(what.as_deref() == Some("code")),
            Command::Quit => self.quit(),
            Command::Unknown(name) => {
                let hint = match crate::commands::closest(&name) {
                    Some(s) => format!("did you mean /{s}? (/help lists the commands)"),
                    None => "try /help".to_string(),
                };
                self.notice(Level::Warn, format!("unknown command /{name} ({hint})"));
            }
            Command::BadArgument(cmd, arg) => {
                let valid: Vec<&str> = crate::commands::fixed_args(&cmd).iter().map(|(v, _)| *v).collect();
                let tail = if valid.is_empty() { String::new() } else { format!(". Use one of: {}", valid.join(", ")) };
                self.notice(Level::Warn, format!("/{cmd}: cannot read '{arg}'{tail}"))
            }
        }
        self.dirty = true;
    }

    /// `/doctor`: the checks of `nexo doctor --offline`, run by this same program and shown here.
    /// The live call to the provider stays with the shell command.
    pub(super) fn run_doctor(&mut self) {
        let exe = std::env::current_exe().ok().filter(|e| {
            e.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s == "nexo" || s.starts_with("nexo-cli"))
        });
        let out = exe.and_then(|e| {
            std::process::Command::new(e).args(["doctor", "--offline"]).current_dir(&self.cwd).output().ok()
        });
        match out {
            Some(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).trim_end().to_string();
                text.push_str("\n(`nexo doctor` in a shell also calls the provider)");
                self.notice(if o.status.success() { Level::Info } else { Level::Warn }, text);
            }
            None => self.notice(Level::Warn, "/doctor: run `nexo doctor` in a shell"),
        }
    }

    /// `/model`: the configured models to switch to, and the guided setup of a new one.
    /// The provider in use is a subscription (a login): its prices are API value, not money spent.
    /// The interface started with no provider chosen and none has been since.
    /// The request given on the command line: sent at once, or left in the input while no model is
    /// chosen yet, to send once one is.
    pub fn start_with(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.input.set_text(text);
        if !self.opts.provider.is_empty() {
            self.submit();
        }
    }

    /// The line printed when the interface closes on a session something was said in: how to come back to it.
    pub fn resume_hint(&self) -> Option<String> {
        let id = self.session_id.as_deref()?;
        self.blocks
            .iter()
            .any(|b| matches!(b, Block::User { .. }))
            .then(|| format!("nexo --resume {id}   (or nexo -c here)"))
    }

    pub(super) fn nothing_chosen(&self) -> bool {
        self.locked && self.opts.provider.is_empty()
    }

    /// On the first screen, when the provider that was chosen (flag or config) is not usable: what
    /// was tried and why it did not work, from what the start-up already knows (no new call).
    pub fn unreachable_note(&self) -> Option<Vec<String>> {
        let name = self.opts.provider.as_str();
        if !self.locked || name.is_empty() {
            return None;
        }
        let (_, c) = self.opts.providers.iter().find(|(n, _)| n == name)?;
        if let Some(var) = c.api_key_env.as_deref() {
            return Some(vec![format!("{name} has no key: set ${var} or sign in with the setup")]);
        }
        let found = self.scan.as_ref()?;
        if found.iter().any(|(p, _)| p == name) {
            return None;
        }
        let mut out = vec![format!("{name} did not answer at {}", c.base_url)];
        if name == "ollama" {
            out.push("Ollama does not seem to be running: start it with `ollama serve`".into());
        }
        Some(out)
    }

    pub fn on_plan(&self) -> bool {
        self.opts.providers.iter().any(|(n, c)| *n == self.provider && c.login.is_some())
    }

    /// Takes the latest reading of the current provider's plan from the store the providers fill. A
    /// provider on a key has no plan, and a reading that did not change costs nothing.
    pub fn sync_plan_usage(&mut self) {
        let kind = self
            .opts
            .providers
            .iter()
            .find(|(n, _)| *n == self.provider)
            .and_then(|(_, c)| nexo_providers::usage::kind_of(c));
        let now = kind.and_then(nexo_providers::usage::get);
        if now != self.plan_usage {
            self.plan_usage = now;
            self.dirty = true;
        }
    }

    /// A model works: the locked home gives way to the normal one (at once without animations).
    pub fn unlock(&mut self) {
        if self.locked {
            self.locked = false;
            if self.fx_on() {
                self.fx.unlock_at = Some(self.now());
            }
            self.dirty = true;
        }
    }

    pub fn open_setup(&mut self) {
        let cur = format!("{}/{}", self.provider, self.model);
        let mut setup = Setup::new(self.opts.providers.clone(), self.opts.models.clone(), &cur);
        // a scan that answered before the dialog opened is not waited for again
        if let Some(found) = self.scan.clone() {
            setup.found_local(found);
        }
        self.modal = Some(Modal::Setup(Box::new(setup)));
        self.send(AgentCommand::ScanLocal);
    }

    /// `/effort`: the levels the model takes, the default first.
    pub(super) fn open_effort_picker(&mut self) {
        let levels = if self.effort_levels.is_empty() {
            vec!["off".to_string(), "low".into(), "medium".into(), "high".into()]
        } else {
            self.effort_levels.clone()
        };
        let mut items = vec![PickItem {
            label: "default".into(),
            value: "default".into(),
            hint: "what the model does without being told".into(),
        }];
        let known = !self.effort_levels.is_empty();
        items.extend(levels.into_iter().map(|l| PickItem {
            hint: if known { String::new() } else { "not checked for this model (/model checks it)".into() },
            label: l.clone(),
            value: l,
        }));
        let cur = self.effort.clone().unwrap_or_else(|| "default".into());
        let title = format!("Effort · {}", self.model);
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Effort, &title, items, Some(&cur))));
    }

    /// Switches the colours; `persist` remembers the choice in the user's configuration.
    pub fn set_theme(&mut self, name: &str, persist: bool) {
        match Theme::by_name(name) {
            Some(t) => {
                let t = if self.theme.degraded || !crate::theme::truecolor_terminal() { t.downsampled() } else { t };
                let degraded = t.degraded;
                self.apply_theme(t);
                if degraded {
                    self.notice(
                        Level::Info,
                        "this terminal reports no 24-bit colour: the theme is mapped to the 256-colour cube",
                    );
                }
                let name = self.theme.name;
                if persist {
                    match crate::config_edit::set_string(&nexo_core::paths::user_config_file(), "ui", "theme", name) {
                        Ok(()) => self.flash_toast(&format!("theme: {name} (saved)")),
                        Err(e) => {
                            self.flash_level(Level::Error, &format!("theme: {name} (not saved: {e})"));
                            self.toast(crate::toast::Kind::Error, &format!("theme not saved: {e}"));
                        }
                    }
                } else {
                    self.flash(&format!("theme: {name}"));
                }
            }
            None => self.notice(Level::Warn, format!("no theme '{name}' ({})", crate::theme::NAMES.join(", "))),
        }
    }

    pub(super) fn apply_theme(&mut self, t: Theme) {
        if self.theme == t {
            return;
        }
        self.theme = t;
        self.input.set_placeholder_style(self.theme.dim());
        self.invalidate_all();
    }

    /// While the theme picker is open, the transcript shows the hovered theme.
    pub fn sync_theme_preview(&mut self) {
        let Some(Modal::Picker(p)) = &self.modal else { return };
        if p.kind != PickKind::Theme {
            return;
        }
        if let Some(t) = p.current().and_then(|it| Theme::by_name(&it.value)) {
            if self.theme_before_preview.is_none() {
                self.theme_before_preview = Some(self.theme.clone());
            }
            let t = if self.theme.degraded { t.downsampled() } else { t };
            self.apply_theme(t);
        }
    }

    /// Esc on the theme picker: back to the theme before the preview.
    pub(super) fn end_theme_preview(&mut self, keep: bool) {
        if let Some(t) = self.theme_before_preview.take() {
            if !keep {
                self.apply_theme(t);
            }
        }
    }
}
