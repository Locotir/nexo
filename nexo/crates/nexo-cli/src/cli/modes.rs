//! The interactive interface and the collected headless run.

use super::*;

pub(super) async fn interactive(
    config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    resume: Option<String>,
    unset: bool,
    first: Option<String>,
) -> anyhow::Result<i32> {
    let (handle, ends) = channels();
    #[cfg(unix)]
    let observation_events = ends.events.clone();
    #[cfg(unix)]
    let _observation_host = nexo_agent::observe_remote::serve_with_errors(
        nexo_agent::observe_remote::default_root(),
        handle.observation.clone(),
        move |text| {
            let _ = observation_events.send(nexo_core::agent::AgentEvent::Notice {
                level: nexo_core::agent::Level::Warn,
                text,
                problem: None,
            });
        },
    );
    #[cfg(unix)]
    let _intervention_host = nexo_agent::observe_remote::serve_interventions_with_errors(
        nexo_agent::observe_remote::default_root(),
        handle.observation.clone(),
        handle.commands.clone(),
        {
            let events = ends.events.clone();
            move |text| {
                let _ = events.send(nexo_core::agent::AgentEvent::Notice {
                    level: nexo_core::agent::Level::Warn,
                    text,
                    problem: None,
                });
            }
        },
    );
    let models = setup::model_choices(&config);
    // The session files are read while the agent is set up, not before it.
    let listing = tokio::task::spawn_blocking(nexo_agent::session::list);
    // sessions write to each other through the mailbox; it takes its session when the agent has one
    let mailbox = config.mail.enabled.then(|| {
        nexo_core::mailbox::Mailbox::unbound(
            &nexo_core::mailbox::mail_root(),
            &cwd.display().to_string(),
            false,
            nexo_core::mail::Inbound::parse(&config.mail.inbound),
        )
    });
    let agent_setup = setup::agent_setup_in_background(&config, &cwd, &picked, resume.clone(), mailbox.clone()).await?;
    // A session nothing was said in has nothing to resume.
    let sessions = listing
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.messages > 0 && !m.headless)
        .map(|m| (m.id, m.title))
        .collect();
    let opts = nexo_tui::TuiOptions {
        cwd: cwd.clone(),
        // nothing chosen: the interface names none and opens on the locked home
        provider: if unset { String::new() } else { picked.provider.clone() },
        model: if unset { String::new() } else { picked.model.clone() },
        models,
        providers: config.providers.iter().map(|(n, c)| (n.clone(), c.clone())).collect(),
        theme: config.ui.theme.clone(),
        mouse: config.ui.mouse,
        show_reasoning: config.ui.show_reasoning,
        timestamps: config.ui.timestamps,
        fold_after: config.ui.fold_after,
        group_tools: config.ui.group_tools,
        complete_words: config.ui.complete_words,
        suggest: config.ui.suggest,
        hints: config.ui.hints,
        keys: config.ui.keys.clone(),
        animations: config.ui.animations,
        figure: config.ui.figure,
        home_stars: config.ui.home_stars,
        bursts: config.ui.bursts,
        detail: config.ui.detail,
        todo_strip: config.ui.todo_strip,
        status_line: config.ui.status_line.clone(),
        status_command: config.ui.status_command.clone(),
        notify_after: config.ui.notify_after,
        vim: config.ui.vim,
        prompt: first,
        session_id: resume,
        sessions,
        skills: nexo_core::skills::discover(&cwd, !config.agent.hermetic),
    };
    if !crate::update::off(&config) {
        let events = ends.events.clone();
        // at start, then every hour while nexo runs (a web redirect, and the API only for the commit
        // count); the interface says each time more commits are out
        tokio::spawn(async move {
            let mut told = None;
            loop {
                if let Ok(Some(n)) = crate::update::newer_release().await {
                    let sent = match n.count {
                        Some(behind) => events.send(AgentEvent::UpdateAvailable { behind }),
                        // no count to show (GitHub's API did not answer): say it once per release
                        None if told.as_ref() != Some(&n.tag) => {
                            told = Some(n.tag);
                            let text = "A newer nexo release is out. Run `nexo update`, then restart nexo.".to_string();
                            events.send(AgentEvent::Notice {
                                level: nexo_core::agent::Level::Info,
                                text,
                                problem: None,
                            })
                        }
                        None => Ok(()),
                    };
                    if sent.is_err() {
                        return;
                    }
                }
                tokio::time::sleep(crate::update::EVERY).await;
            }
        });
    }
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));
    nexo_tui::set_supervise_options(nexo_supervise::Options::detect());
    if let Some(m) = mailbox {
        nexo_tui::set_mailbox(m);
    }
    let ui = nexo_tui::run(handle, opts).await;
    agent.abort();
    ui?;
    Ok(0)
}

/// Runs one request without the interface and returns the reply text and the exit code (0 done, 2 not done, 1 agent failure).
pub(super) async fn headless_collect(
    config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    prompt: String,
) -> anyhow::Result<(String, i32)> {
    nexo_agent::session::mark_headless();
    let (mut handle, ends) = channels();
    let gate: std::sync::Arc<dyn nexo_core::PermissionGate> = std::sync::Arc::new(setup::HeadlessGate);
    let agent_setup = setup::agent_setup(&config, &cwd, &picked, None, Some(gate), None).await?;
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));
    let sent = handle.commands.send(AgentCommand::Submit { text: prompt, attachments: vec![] }).is_ok();
    let (mut text, mut code) = (String::new(), 0);
    while let Some(ev) = handle.events.recv().await {
        match &ev {
            AgentEvent::TextDelta { text: t } => text.push_str(t),
            AgentEvent::AssistantStart if !text.is_empty() => text.clear(),
            AgentEvent::TurnEnd { reason } => {
                code = if matches!(reason, nexo_core::agent::TurnEnd::Done) { 0 } else { 2 };
            }
            AgentEvent::Idle { .. } | AgentEvent::Shutdown => break,
            _ => {}
        }
    }
    let _ = handle.commands.send(AgentCommand::Shutdown);
    let failed = match tokio::time::timeout(std::time::Duration::from_secs(2), agent).await {
        Ok(Ok(Err(e))) => Some(format!("{e:#}")),
        Ok(Err(e)) => Some(format!("the agent stopped: {e}")),
        _ if !sent => Some("the agent stopped before the request".to_string()),
        _ => None,
    };
    if let Some(e) = failed {
        eprintln!("nexo: {e}");
        return Ok((text, 1));
    }
    Ok((text, code))
}
