//! The agent's setup: built-in tools, MCP servers and the headless permission gate.

use super::*;

/// The built-in tools of a session. A hermetic one (a benchmark arm) has no web tools: the model could
/// fetch the task's own solution.
pub(super) fn session_builtins(cfg: &Config) -> ToolSet {
    let mut tools = nexo_tools::builtin_with(&cfg.web);
    tools.add(Arc::new(nexo_tools::browser::BrowserTool::with_config(cfg.browser.clone())));
    if cfg.agent.hermetic {
        tools.remove("web_fetch");
        tools.remove("web_search");
    }
    // only where the debugger has a backend
    let debug = cfg.tools.debug && nexo_tools::debug::available();
    // reads a compiled program as bytes, on every platform; its text names `debug` only where that tool is here
    if cfg.tools.bin {
        tools.add(Arc::new(crate::bin_tool::Bin::new(debug)));
    }
    if debug {
        tools.add(Arc::new(nexo_tools::debug::Debug::new()));
    }
    // the API behind a website: reads a HAR file and calls the endpoints of its map. It goes with `bin` (the RE kit
    // switch) and, like the web tools, is not in a hermetic session: a call reaches the network
    if cfg.tools.bin && !cfg.agent.hermetic {
        tools.add(Arc::new(crate::api_tool::Api));
    }
    tools
}

/// The tools a session starts with from `cwd` (what `nexo prompt` describes): the built-in ones, the skill
/// tool, `task` when sub-agents are on and `ask_user`. MCP servers and mail are not started for it.
pub fn prompt_tools(cfg: &Config, cwd: &Path) -> ToolSet {
    prompt_tools_with(cfg, cwd, || nexo_agent::subagent::load(cwd))
}

/// [`prompt_tools`] with the sub-agent kinds given (read only when sub-agents are on): `load` reads the person's
/// own agent files, which a test of the tool text must not depend on.
fn prompt_tools_with(cfg: &Config, cwd: &Path, kinds: impl FnOnce() -> Vec<nexo_agent::subagent::Profile>) -> ToolSet {
    nexo_tools::edit_check::set_enabled(cfg.tools.edit_diagnostics);
    let mut tools: ToolSet = session_builtins(cfg);
    if !nexo_tools::lsp::available(cwd) {
        tools.remove("diagnostics");
    }
    if let Some(t) = nexo_tools::skill::tool(nexo_core::skills::discover(cwd, !cfg.agent.hermetic)) {
        tools.add(t);
    }
    // as `nexo_agent` adds them to a session; the model named is the one `nexo` would start with
    if cfg.agent.subagents && tools.specs().iter().any(|s| s.name == "read" || s.name == "grep") {
        let (provider, model) = cfg.pick().unwrap_or_else(|| ("provider".into(), "model".into()));
        let pcfg =
            cfg.providers.get(&provider).cloned().unwrap_or_else(|| ProviderConfig::new("openai-chat", "", None));
        let client: Arc<dyn nexo_agent::client::ModelClient> =
            Arc::new(nexo_providers::Client::new(nexo_providers::registry()));
        let sub = nexo_agent::subagent::SubAgent::new(
            client,
            nexo_agent::Target::new(provider, pcfg, model),
            &tools,
            kinds(),
            Arc::default(),
        )
        .with_providers(cfg.providers.clone())
        .with_parallel(cfg.agent.parallel_tasks);
        tools.add(Arc::new(sub));
    }
    let (_unused, ends) = nexo_core::agent::channels();
    tools.add(Arc::new(nexo_agent::ask::AskUser::new(nexo_agent::ask::ChannelAsker::new(ends.events))));
    tools
}

pub async fn agent_setup(
    cfg: &Config,
    cwd: &Path,
    picked: &Picked,
    resume: Option<String>,
    gate: Option<Arc<dyn PermissionGate>>,
    mail: Option<Arc<nexo_core::mailbox::Mailbox>>,
) -> anyhow::Result<nexo_agent::AgentSetup> {
    agent_setup_with(cfg, cwd, picked, resume, gate, mail, false).await
}

/// As `agent_setup`, but the MCP servers connect while the interface starts: the setup does not wait for
/// them, and each one's tools join the next turn's request (the first request waits for any still connecting).
pub async fn agent_setup_in_background(
    cfg: &Config,
    cwd: &Path,
    picked: &Picked,
    resume: Option<String>,
    mail: Option<Arc<nexo_core::mailbox::Mailbox>>,
) -> anyhow::Result<nexo_agent::AgentSetup> {
    agent_setup_with(cfg, cwd, picked, resume, None, mail, true).await
}

pub(super) async fn agent_setup_with(
    cfg: &Config,
    cwd: &Path,
    picked: &Picked,
    resume: Option<String>,
    gate: Option<Arc<dyn PermissionGate>>,
    mail: Option<Arc<nexo_core::mailbox::Mailbox>>,
    mcp_in_background: bool,
) -> anyhow::Result<nexo_agent::AgentSetup> {
    let client: Arc<dyn nexo_agent::client::ModelClient> =
        Arc::new(nexo_providers::Client::new(nexo_providers::registry()));
    nexo_tools::edit_check::set_enabled(cfg.tools.edit_diagnostics);
    let mut tools: ToolSet = session_builtins(cfg);
    if !nexo_tools::lsp::available(cwd) {
        tools.remove("diagnostics");
    }
    if let Some(t) = nexo_tools::skill::tool(nexo_core::skills::discover(cwd, !cfg.agent.hermetic)) {
        tools.add(t);
    }
    if let Some(m) = &mail {
        let mt = nexo_tools::mail::tools(m.clone());
        let names: Vec<String> = mt.names().map(String::from).collect();
        for n in names {
            if let Some(t) = mt.get(&n) {
                tools.add(t.clone());
            }
        }
    }
    // a server the managed policy does not list is never started, whatever file named it
    let mut servers = cfg.mcp.clone();
    if let Some(p) = &cfg.managed {
        servers.retain(|name, server| match p.check_mcp(name, server) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("nexo: {e}");
                false
            }
        });
    }
    let mcp = if servers.is_empty() {
        None
    } else {
        let project = cwd.to_path_buf();
        let persist: nexo_tools::mcp_hub::Persist =
            Arc::new(move |name, on| crate::mcp_cmd::persist_enabled(&project, name, on));
        let auth = nexo_core::auth::file();
        let hub = if mcp_in_background {
            // the tools come with the first request (`McpHub::sync_tools`), the failures with the /mcp window
            nexo_tools::mcp_hub::McpHub::start_in_background(&servers, &auth, Some(persist))
        } else {
            let hub = nexo_tools::mcp_hub::McpHub::start(&servers, &auth, Some(persist)).await;
            for (name, why) in hub.failures() {
                eprintln!("nexo: MCP server {name:?} not started: {why}");
            }
            for t in hub.tools() {
                tools.add(t);
            }
            hub
        };
        Some(hub)
    };
    Ok(nexo_agent::AgentSetup {
        config: cfg.clone(),
        cwd: cwd.to_path_buf(),
        provider: picked.provider.clone(),
        model: picked.model.clone(),
        tools,
        client,
        resume,
        gate,
        mail,
        mcp,
        model_pinned: picked.named,
    })
}

/// Headless runs cannot ask: what the policy does not allow is denied, with a hint.
pub struct HeadlessGate;

#[async_trait]
impl PermissionGate for HeadlessGate {
    async fn ask(&self, req: PermissionRequest) -> PermissionDecision {
        PermissionDecision::Deny {
            reason: Some(match req.suggested_rules.first() {
                Some(rule) => format!(
                    "{} not allowed in a non-interactive run; use --permission-mode yolo or an allow rule such as {rule}",
                    req.summary
                ),
                // a call that is always asked about (outside the sandbox, detached): no rule covers it
                None => format!(
                    "{} not allowed in a non-interactive run; no allow rule covers it, use --permission-mode yolo",
                    req.summary
                ),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_hermetic_session_has_no_web_tools() {
        let mut c = super::Config::default();
        let names = |c: &super::Config| -> Vec<String> {
            super::session_builtins(c).specs().iter().map(|s| s.name.clone()).collect()
        };
        let normal = names(&c);
        assert!(normal.iter().any(|n| n == "web_fetch") && normal.iter().any(|n| n == "web_search"), "{normal:?}");
        c.agent.hermetic = true;
        let h = names(&c);
        assert!(!h.iter().any(|n| n.starts_with("web_")), "{h:?}");
        assert!(h.iter().any(|n| n == "read"));
    }

    /// Characters (name, description, parameter schema) of the tools a session has that `builtin()` does not: the
    /// ones `prompt_tools` adds (`ask_user`, `task`, `bin` and, where there is a backend, `debug`), as a model is
    /// offered them in a normal window and in a small one. The tool budget in `nexo-agent` counts `builtin()` only.
    fn session_extras(small: bool) -> usize {
        let mut cfg = super::Config::default();
        // no skills from the person's home, and the sub-agent kinds are the built-in ones, not the person's own
        // agent files: the count must not depend on whose machine runs it
        cfg.agent.hermetic = true;
        let dir = tempfile::tempdir().unwrap();
        let tools = super::prompt_tools_with(&cfg, dir.path(), nexo_agent::subagent::builtin);
        let counted: Vec<String> = nexo_tools::builtin().names().map(String::from).collect();
        let window = small.then_some(nexo_agent::prompt::COMPACT_WINDOW);
        tools
            .specs_for(small)
            .iter()
            .filter(|s| !counted.contains(&s.name) && !nexo_agent::prompt::left_out(window, &s.name))
            .map(|s| s.name.len() + s.description.len() + serde_json::to_string(&s.parameters).unwrap().len())
            .sum()
    }

    /// 2045 characters (about 520 tokens) when this budget was set: `ask_user` 832, `task` 646 (its short text, as
    /// it is a rare tool), `debug` 300 and `bin` 267; every request carries them. It may grow 5% and no more.
    /// `the_tool_text_stays_within_its_budget` in `nexo-agent` is the same guard for `builtin()`.
    #[test]
    fn the_session_tools_beyond_the_built_in_ones_stay_within_their_budget() {
        let n = session_extras(false);
        // `debug` (300) is there only where it has a backend
        let budget = if nexo_tools::debug::available() { 2_150 } else { 1_830 };
        assert!(n <= budget, "ask_user, task, bin and debug take {n} characters; the budget is {budget}");
        assert!(n > 1_000, "the extra tools were not counted: {n} characters");
    }

    /// In a 32k window only `ask_user` of them is offered: 452 characters when this budget was set, 5% more at most.
    #[test]
    fn the_session_tools_beyond_the_built_in_ones_stay_within_their_small_window_budget() {
        let n = session_extras(true);
        assert!(n <= 480, "the extra tools of a small window take {n} characters; the budget is 480");
        assert!(n > 200, "the extra tools were not counted: {n} characters");
    }
}
