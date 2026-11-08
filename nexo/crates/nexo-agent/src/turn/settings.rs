//! Part of the agent loop; see the parent module.

use super::*;

pub(super) async fn set_model(state: &Shared, provider: Option<String>, model: String) {
    let mut st = state.lock().await;
    let provider = provider.unwrap_or_else(|| st.target.provider());
    if let Err(e) = st.config.check_model(&provider, &model) {
        notice(&st.events, Level::Error, e);
        return;
    }
    match st.config.providers.get(&provider).cloned() {
        None => notice(&st.events, Level::Error, format!("unknown provider `{provider}` (see `nexo providers`)")),
        Some(cfg) => {
            let was_fast = fast_on(&st);
            st.limits = limits::resolve(&provider, &cfg, &model, None);
            // another model may count with another tokenizer
            st.tok_ratio = None;
            // the fast request option belongs to the provider that was asked for it
            if provider != st.target.provider() {
                st.fast = false;
            }
            st.target.set(&provider, cfg, &model);
            let _ = st.session.record_model(&provider, &model);
            emit(&st.events, AgentEvent::ModelChanged { provider: provider.clone(), model: model.clone() });
            emit(&st.events, st.limits.event(&provider, &model));
            emit(&st.events, effort_event(&st));
            if fast_on(&st) != was_fast {
                emit(&st.events, fast_event(&st));
            }
            drop(st);
            spawn_vendor_fill(state);
        }
    }
}

pub(super) fn budget_event(st: &State, day: f64) -> AgentEvent {
    AgentEvent::Budget {
        session_cost: st.cost,
        day_cost: day,
        session_limit: st.budget,
        day_limit: st.config.agent.max_cost_day,
    }
}

/// `/budget`: what was spent and what may be.
pub(super) fn budget_text(st: &State, day: f64, changed: bool) -> String {
    let limit = |l: Option<f64>| l.map(|l| format!(" of ${l:.2}")).unwrap_or_else(|| " (no limit)".into());
    let head = if changed { "spend limit set" } else { "spending" };
    format!(
        "{head}\nthis session ${:.4}{}\ntoday        ${day:.4}{}\n`/budget <dollars>` sets this session's limit, `/budget off` lifts it; [agent] max_cost and max_cost_day keep them",
        st.cost,
        limit(st.budget),
        limit(st.config.agent.max_cost_day)
    )
}

/// `/status`: what nexo is running with, one fact a line.
pub(super) fn status_text(st: &State) -> String {
    let (provider, cfg, model) = st.target.get();
    let key = match (&cfg.api_key_env, &cfg.api_key_cmd) {
        _ if cfg.api_key_file.is_some() => {
            format!("from the file {}", cfg.api_key_file.as_ref().map(|f| f.display().to_string()).unwrap_or_default())
        }
        (_, Some(c)) => format!("from the command `{c}`"),
        (Some(v), None) if std::env::var(v).is_ok_and(|k| !k.trim().is_empty()) => format!("${v} ✓"),
        (Some(v), None) => format!("${v} is not set"),
        (None, None) => "none needed".into(),
    };
    let user = nexo_core::paths::user_config_file();
    let project = nexo_core::paths::project_config_file(&st.cwd);
    let short = nexo_core::paths::shown;
    let file = |p: &Path| format!("{}{}", short(p), if p.is_file() { "" } else { " (none)" });
    let effort = setup::effort_of(&cfg, &model, &st.config.agent.reasoning).unwrap_or_else(|| "default".into());
    let mut lines = vec![
        format!(
            "nexo {} · {}{}",
            nexo_core::version::get(),
            std::env::consts::OS,
            if nexo_core::paths::is_root() { " · running as root" } else { "" }
        ),
        {
            let sh = nexo_core::shell::shell();
            let program = sh.program.display().to_string();
            // `sh (sh)` says the same thing twice
            if program == sh.kind {
                format!("shell        {program}")
            } else {
                format!("shell        {} ({program})", sh.kind)
            }
        },
        format!("model        {provider}/{model} · effort {effort}"),
        format!("endpoint     {} ({})", cfg.base_url, cfg.protocol),
        format!("key          {key}"),
        {
            // the long reason for the label is /context's to give
            let (label, _why) = crate::context::describe(st.policy.as_ref(), st.last_view.0, st.last_view.1);
            format!("context      {label} · {} messages in memory (/context says why)", st.history.len())
        },
        format!("permissions  {}", st.perm.mode.as_str()),
        format!("sandbox      {}", sandbox_line(st).0),
        format!("session      {} ({})", st.session.id(), short(st.session.path())),
        format!("directory    {}", short(&st.cwd)),
        format!("config       {}", file(&user)),
    ];
    if let Some(p) = project {
        lines.push(format!("             {}", file(&p)));
    }
    if let Some((name, branch)) = nexo_core::worktree::current(&st.cwd) {
        lines.push(format!("worktree     {name} on branch {branch} (nexo worktree list)"));
    }
    lines.push(format!("tools        {} (/tools lists them)", st.tools.specs().len()));
    if let Some(p) = &st.config.managed {
        lines.extend(p.lines());
    }
    lines.extend(compat_lines(st));
    lines.join("\n")
}

/// Where the project instructions, custom commands and custom sub-agents were found, briefly.
pub(super) fn compat_lines(st: &State) -> Vec<String> {
    use nexo_core::compat;
    let short = nexo_core::paths::shown;
    let home = compat::foreign_home();
    let mut lines = Vec::new();
    if !st.config.agent.hermetic {
        let found = compat::instructions(&st.cwd, &nexo_core::paths::config_dir(), home.as_deref());
        if !found.is_empty() {
            let list: Vec<String> = found.iter().map(|i| format!("{} ({})", short(&i.path), i.origin)).collect();
            lines.push(format!("instructions {}", list.join(", ")));
        }
    }
    let trust = nexo_core::paths::trusted_projects_file();
    let cfg = nexo_core::paths::config_dir();
    let cmds = compat::commands(&st.cwd, &cfg, &trust, home.as_deref(), &|_| false);
    if !cmds.is_empty() {
        let mut by: Vec<(String, usize)> = Vec::new();
        for c in &cmds {
            match by.iter_mut().find(|(o, _)| *o == c.origin) {
                Some(e) => e.1 += 1,
                None => by.push((c.origin.clone(), 1)),
            }
        }
        let list: Vec<String> = by.iter().map(|(o, n)| format!("{n} from {o}")).collect();
        lines.push(format!("commands     {} ({})", cmds.len(), list.join(", ")));
    }
    let sources = compat::sources(&st.cwd, &cfg, &trust, home.as_deref(), "agents", Some("agents"), None);
    let agents: Vec<String> = crate::subagent::load(&st.cwd)
        .into_iter()
        .filter_map(|p| {
            let dir = p.source.as_deref()?.parent()?;
            let o = sources.iter().find(|s| s.dir == dir).map_or("custom", |s| s.origin.as_str());
            Some(format!("{} ({o})", p.name))
        })
        .collect();
    if !agents.is_empty() {
        lines.push(format!("sub-agents   {}", agents.join(", ")));
    }
    lines
}

/// `/tools`: the tools by kind.
pub(super) fn jobs_text(jobs: &Jobs) -> String {
    let all = jobs.list();
    if all.is_empty() {
        return "no background jobs".into();
    }
    let mut lines = vec![format!("{} ({} running)", nexo_core::count(all.len(), "background job"), jobs.running())];
    for j in all {
        let first: String = j.label.lines().next().unwrap_or("").chars().take(70).collect();
        lines.push(format!(
            "  {:>3}  {:<14} {:>10}  {}",
            j.id,
            j.status.plain(),
            nexo_core::jobs::duration(j.elapsed_ms),
            first
        ));
    }
    lines.push("stop one with /jobs kill <id>, all with /jobs kill".into());
    lines.join("\n")
}

pub(super) fn tools_text(tools: &ToolSet) -> String {
    let mut by_kind: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for spec in tools.specs() {
        let kind = tools.get(&spec.name).map(|t| t.kind().label()).unwrap_or("Other");
        by_kind.entry(kind).or_default().push(spec.name.clone());
    }
    let mut lines = vec![format!("{} tools", tools.specs().len())];
    for (k, names) in by_kind {
        lines.push(format!("{k:<13} {}", names.join(", ")));
    }
    lines.join("\n")
}

/// The reasoning level of the model in use and the levels it takes.
pub(super) fn effort_event(st: &State) -> AgentEvent {
    let (_, cfg, model) = st.target.get();
    AgentEvent::Effort {
        current: setup::effort_of(&cfg, &model, &st.config.agent.reasoning),
        levels: cfg.efforts.get(&model).cloned().unwrap_or_default(),
        model,
    }
}

/// Sets the reasoning level of the model in use and remembers it in the user's configuration.
pub(super) async fn set_effort(state: &Shared, effort: Option<String>) {
    let mut st = state.lock().await;
    let (provider, mut cfg, model) = st.target.get();
    let effort = effort.map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty());
    let levels = cfg.efforts.get(&model).cloned().unwrap_or_default();
    if let Some(e) = &effort {
        let known = levels.iter().any(|l| l == e) || (levels.is_empty() && reasoning_of(&Some(e.clone())).is_some());
        if !known {
            let take = if levels.is_empty() {
                "off, minimal, low, medium, high or a token budget".to_string()
            } else {
                levels.join(", ")
            };
            notice(&st.events, Level::Warn, format!("{model} does not take `{e}`: {take}"));
            return;
        }
    }
    let value = effort.clone().unwrap_or_default();
    cfg.effort.insert(model.clone(), value.clone());
    if let Some(c) = st.config.providers.get_mut(&provider) {
        c.effort.insert(model.clone(), value.clone());
    }
    st.target.set(&provider, cfg, &model);
    let file = nexo_core::paths::user_config_file();
    if let Err(e) =
        config_edit::set(&file, &["providers", &provider, "effort"], &model, toml_edit::Value::from(value.as_str()))
    {
        notice(&st.events, Level::Warn, format!("effort set for this session only ({e})"));
    }
    emit(&st.events, effort_event(&st));
}

/// Whether the model in use runs fast: the provider's fast request option is on, or the model is
/// the fast twin of one the provider lists.
pub(super) fn fast_on(st: &State) -> bool {
    let (provider, cfg, model) = st.target.get();
    if st.fast && fast::option(&cfg).is_some() {
        return true;
    }
    let live = st.live.iter().filter(|(p, _)| *p == provider).flat_map(|(_, l)| l.iter());
    matches!(fast::twin(&model, cfg.models.iter().chain(live).map(String::as_str)), Twin::Base(_))
}

pub(super) fn fast_event(st: &State) -> AgentEvent {
    AgentEvent::Fast { on: fast_on(st) }
}

/// `/fast`: the model in use to its fast twin on the same provider, or back to the base; where the
/// provider takes speed as a request option instead (`fast::option`), that option. `on` None
/// switches to the other of the two.
pub(super) async fn set_fast(state: &Shared, on: Option<bool>) {
    let (provider, cfg, model, client, events) = {
        let st = state.lock().await;
        let (p, c, m) = st.target.get();
        (p, c, m, st.client.clone(), st.events.clone())
    };
    let option = fast::option(&cfg);
    let listed = |st: &State| -> Vec<String> {
        let live = st.live.iter().filter(|(p, _)| *p == provider).flat_map(|(_, l)| l.iter().cloned());
        cfg.models.iter().cloned().chain(live).collect()
    };
    let mut found = fast::twin(&model, listed(&*state.lock().await).iter().map(String::as_str));
    if found == Twin::None && option.is_none() {
        // the models the provider offers now may hold the twin the configuration does not
        if let Ok(Ok(list)) = tokio::time::timeout(Duration::from_secs(8), client.list_models(&cfg)).await {
            let mut st = state.lock().await;
            st.live = Some((provider.clone(), list.into_iter().map(|m| m.id).collect()));
            found = fast::twin(&model, listed(&st).iter().map(String::as_str));
        }
    }
    match found {
        Twin::Base(base) => match on {
            Some(true) => notice(&events, Level::Info, format!("{model} is already a fast version")),
            _ => set_model(state, Some(provider), base).await,
        },
        _ if option.is_some() => {
            let mut st = state.lock().await;
            st.fast = on.unwrap_or(!st.fast);
            emit(&st.events, fast_event(&st));
        }
        Twin::Fast(fast) => match on {
            Some(false) => notice(&events, Level::Info, "fast is already off"),
            _ => set_model(state, Some(provider), fast).await,
        },
        Twin::None => match on {
            Some(false) => notice(&events, Level::Info, "fast is already off"),
            _ => notice(&events, Level::Info, format!("No fast version of {model} on {provider}.")),
        },
    }
}

/// Saves what the setup found for a model and switches to it.
pub(super) async fn save_model(state: &Shared, name: String, cfg: ProviderConfig, model: String, key: Option<String>) {
    {
        let st = state.lock().await;
        if let Err(e) = st.config.check_model(&name, &model) {
            notice(&st.events, Level::Error, e);
            return;
        }
    }
    let mut cfg = cfg;
    let mut kept = None;
    {
        let st = state.lock().await;
        if let Some(k) = key.as_deref().filter(|k| !k.trim().is_empty()) {
            match setup::keep_key(&name, k, &mut cfg) {
                Ok(f) => kept = Some(f),
                Err(e) => notice(&st.events, Level::Warn, format!("the key could not be kept: {e}")),
            }
        }
    }
    let mut st = state.lock().await;
    // what was there stays (other models, headers, prices); what the setup found wins
    let mut merged = st.config.providers.get(&name).cloned().unwrap_or_default();
    merged.protocol = cfg.protocol.clone();
    merged.base_url = cfg.base_url.clone();
    if cfg.api_key_env.is_some() || cfg.api_key_cmd.is_some() || cfg.api_key_file.is_some() {
        merged.api_key_env = cfg.api_key_env.clone();
        merged.api_key_cmd = cfg.api_key_cmd.clone();
        merged.api_key_file = cfg.api_key_file.clone();
    }
    for (k, v) in &cfg.headers {
        merged.headers.insert(k.clone(), v.clone());
    }
    if !merged.models.contains(&model) {
        merged.models.push(model.clone());
    }
    merged.default_model = Some(model.clone());
    for (map, from) in [(&mut merged.context_window, &cfg.context_window), (&mut merged.max_output, &cfg.max_output)] {
        if let Some(n) = from.get(&model) {
            map.insert(model.clone(), *n);
        }
    }
    if let Some(p) = cfg.pricing.get(&model) {
        merged.pricing.insert(model.clone(), *p);
    }
    if let Some(l) = cfg.efforts.get(&model) {
        merged.efforts.insert(model.clone(), l.clone());
    }
    if let Some(e) = cfg.effort.get(&model) {
        merged.effort.insert(model.clone(), e.clone());
    }
    let file = nexo_core::paths::user_config_file();
    match setup::save(&file, &name, &merged, &model) {
        Ok(()) => {
            let key_note =
                kept.map(|f| format!("; the key is in {} (only you can read it)", f.display())).unwrap_or_default();
            notice(&st.events, Level::Info, format!("{name}/{model} saved to {}{key_note}", file.display()));
        }
        Err(e) => notice(&st.events, Level::Warn, format!("{name}/{model} is set for this session only ({e})")),
    }
    st.config.providers.insert(name.clone(), merged);
    st.config.provider = Some(name.clone());
    st.config.model = Some(model.clone());
    drop(st);
    set_model(state, Some(name), model).await;
}

/// Remembers the prices of the model in use in the user's configuration.
pub(super) async fn set_pricing(state: &Shared, pricing: Pricing) {
    let mut st = state.lock().await;
    let (provider, mut cfg, model) = st.target.get();
    let file = nexo_core::paths::user_config_file();
    let saved = config_edit::set_pricing(&file, &provider, &model, pricing);
    cfg.pricing.insert(model.clone(), pricing);
    if let Some(c) = st.config.providers.get_mut(&provider) {
        c.pricing.insert(model.clone(), pricing);
    }
    st.target.set(&provider, cfg, &model);
    st.limits.pricing = Some(pricing);
    st.limits.source = "config".into();
    emit(&st.events, st.limits.event(&provider, &model));
    match saved {
        Ok(()) => notice(
            &st.events,
            Level::Info,
            format!(
                "prices of {model} saved to {}: in {} / out {} / cache {} per million",
                file.display(),
                pricing.input,
                pricing.output,
                pricing.cache_read
            ),
        ),
        Err(e) => notice(&st.events, Level::Warn, format!("prices of {model} set for this session only ({e})")),
    }
}
