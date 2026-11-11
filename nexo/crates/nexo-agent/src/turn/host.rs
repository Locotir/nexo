//! Part of the agent loop; see the parent module.

use super::*;

/// Adds what a model call spent to the session and the day, and says so; `call` is the usage of the
/// attempt that answered, without the attempts the client retried, and empty for a side call (a
/// sub-agent, the reader, a compaction) whose request is not the conversation's.
pub(super) fn add_usage(st: &mut State, events: &EventTx, model: &str, usage: Usage, call: Usage, kind: UsageKind) {
    add_usage_priced(st, events, model, usage, call, None, kind);
}

/// [`add_usage`] with the prices of another provider (a sub-agent sent there); none: the main agent's.
pub(super) fn add_usage_priced(
    st: &mut State,
    events: &EventTx,
    model: &str,
    usage: Usage,
    call: Usage,
    other: Option<Pricing>,
    kind: UsageKind,
) -> f64 {
    st.usage += usage;
    let (prov, cfg, main) = st.target.get();
    let sent_to_other = other.is_some();
    let pricing = other.or_else(|| {
        if model == main {
            st.limits.pricing.or_else(|| cfg.pricing_for(model))
        } else {
            cfg.pricing_for(model)
        }
    });
    let cost = pricing.map(|p| p.cost(&usage)).unwrap_or(0.0);
    st.cost += cost;
    // what the view left out of the history at this call (the conversation's own calls only)
    let held = (kind == UsageKind::Main).then(|| st.last_view.0.saturating_sub(st.last_view.1));
    let meta = UsageMeta { model: Some(model.to_string()), kind: Some(kind), held };
    let _ = st.session.record_usage_meta(usage, cost, meta);
    if let Some(audit) = &st.audit {
        let call = match kind {
            UsageKind::Main => "main",
            UsageKind::Sub => "sub",
            UsageKind::Reader => "reader",
            UsageKind::Compact => "compact",
            UsageKind::Suggest => "suggest",
            UsageKind::CheckIn => "check_in",
            UsageKind::Review => "review",
            UsageKind::Sum => "sum",
        };
        let provider = if sent_to_other { "other" } else { prov.as_str() };
        audit.request(RequestRecord {
            session: st.session.id(),
            cwd: &st.cwd,
            provider,
            model,
            call,
            usage,
            cost_usd: cost,
        });
    }
    emit(events, AgentEvent::Usage { turn: usage, call, session: st.usage, cost_usd: st.cost });
    let day = budget::add(cost);
    emit(events, budget_event(st, day));
    cost
}

/// The sub-agents' way to the agent: its permissions and its bill.
pub(super) struct StateHost {
    pub(super) state: Weak<Mutex<State>>,
    pub(super) events: EventTx,
}

#[async_trait::async_trait]
impl crate::subagent::Host for StateHost {
    async fn permit(
        &self,
        call_id: &str,
        agent: &str,
        tool: &dyn Tool,
        args: &Value,
        cancel: &CancellationToken,
        hook: Option<crate::hooks::Decision>,
    ) -> Result<Why, (String, Why)> {
        let Some(state) = self.state.upgrade() else {
            return Err(("the session has ended".into(), Why::denied("rule")));
        };
        let (gate, cwd) = {
            let st = state.lock().await;
            (st.gate.clone(), st.cwd.clone())
        };
        let name = tool.name().to_string();
        match permit_why(&state, &self.events, &gate, &cwd, call_id, &name, tool, args, cancel, hook, Some(agent)).await
        {
            (Verdict::Run, why) => Ok(why),
            (Verdict::Denied(r) | Verdict::Blocked(r), why) => Err((r, why)),
        }
    }

    async fn hook(&self, event: &str, subject: &str, fields: Value) -> crate::hooks::Outcome {
        match self.state.upgrade() {
            Some(state) => fire(&state, event, Some(subject), fields).await,
            None => Default::default(),
        }
    }

    async fn audit(&self, call: crate::subagent::AuditCall<'_>) {
        let Some(state) = self.state.upgrade() else { return };
        let st = state.lock().await;
        let Some(audit) = st.audit.clone() else { return };
        let (provider, _, model) = st.target.get();
        audit.tool(ToolRecord {
            session: st.session.id(),
            cwd: &st.cwd,
            provider: &provider,
            model: &model,
            tool: call.tool,
            args: call.args,
            summary: call.summary,
            why: call.why,
            status: call.status,
            exit_code: None,
            duration_ms: call.duration_ms,
        });
    }

    async fn spent(&self, model: &str, usage: Usage, pricing: Option<Pricing>) -> f64 {
        match self.state.upgrade() {
            Some(state) => {
                let mut st = state.lock().await;
                add_usage_priced(&mut st, &self.events, model, usage, Usage::default(), pricing, UsageKind::Sub)
            }
            None => 0.0,
        }
    }

    async fn history(&self) -> Vec<Message> {
        match self.state.upgrade() {
            Some(state) => state.lock().await.history.to_vec(),
            None => Vec::new(),
        }
    }

    /// A sub-agent's write is put back by a rewind like the main agent's.
    async fn before_write(&self, path: PathBuf) {
        if let Some(state) = self.state.upgrade() {
            let before = std::fs::read(&path).ok();
            let mut st = state.lock().await;
            let at = st.history.len();
            st.checkpoints.push((at, path, before));
        }
    }
}

/// Changes the permission mode; entering or leaving plan mode changes the system prompt.
pub(super) fn set_mode(st: &mut State, mode: PermissionMode) {
    if let Err(e) = st.config.check_mode(mode) {
        notice(&st.events, Level::Error, e);
        // the interface may already show the new mode: say what it is
        emit(&st.events, AgentEvent::PermissionModeChanged { mode: st.perm.mode });
        return;
    }
    let was = st.perm.mode;
    st.perm.mode = mode;
    st.plan.lock().unwrap_or_else(|e| e.into_inner()).set(mode);
    if (was == PermissionMode::Plan) != (mode == PermissionMode::Plan) {
        st.system = full_system(st);
    }
    if let Some(m) = &st.mail {
        m.set_bypass(mode == PermissionMode::Auto);
    }
    emit(&st.events, AgentEvent::PermissionModeChanged { mode });
}

/// What the `bash` tool says of the sandbox, and whether it works (no bash tool: off).
pub(super) fn sandbox_line(st: &State) -> (String, bool) {
    match st.tools.get("bash").and_then(|t| t.sandbox_status(&st.sandbox)) {
        Some(x) => x,
        None => (st.sandbox.mode.as_str().to_string(), true),
    }
}

/// `/add-dir`: the file tools read and write in `dir` without asking, a sandboxed command may write
/// there, and the system prompt says so (the cached prefix starts again once).
pub(super) fn add_dir(st: &mut State, dir: &std::path::Path) {
    let cwd = st.cwd.clone();
    // `~` is the home folder, as in the file tools
    let dir = nexo_tools::util::resolve(&cwd, &dir.to_string_lossy());
    let (real, rules) = match st.config.add_dir(&cwd, &dir) {
        Ok(r) => r,
        Err(e) => return notice(&st.events, Level::Error, format!("add-dir: {e}")),
    };
    for r in rules {
        if !st.perm.allow.contains(&r) {
            st.perm.allow.push(r);
        }
    }
    let sandboxed = st.config.permissions.sandbox_write_dirs.contains(&real);
    if sandboxed && !st.sandbox.extra_dirs.contains(&real) {
        st.sandbox.extra_dirs.push(real.clone());
    }
    let shown = real.display().to_string();
    let note = format!("You may also read and write in this folder: {shown}.");
    let extra = st.config.agent.system_prompt_extra.get_or_insert_with(String::new);
    if !extra.contains(&note) {
        if !extra.trim().is_empty() {
            extra.push_str("\n\n");
        }
        extra.push_str(&note);
        st.system = full_system(st);
    }
    let bash = if sandboxed { "; a sandboxed command may write there too" } else { "" };
    notice(&st.events, Level::Info, format!("added {shown}: the file tools read and write there without asking{bash}"));
}
/// Changes the sandbox mode of the `bash` tool and says what it is now.
pub(super) fn set_sandbox(st: &mut State, mode: Option<nexo_core::sandbox::SandboxMode>) {
    use nexo_core::sandbox::SandboxMode;
    let mode = mode.unwrap_or(if st.sandbox.mode.is_on() { SandboxMode::Off } else { SandboxMode::WorkspaceWrite });
    if let Some(p) = &st.config.managed {
        if let Err(e) = p.check_sandbox(mode) {
            notice(&st.events, Level::Error, e);
            return;
        }
    }
    st.sandbox.mode = mode;
    let (line, ok) = sandbox_line(st);
    let level = if ok { Level::Info } else { Level::Warn };
    notice(&st.events, level, format!("sandbox: {line}"));
}
