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
    let listing = {
        let cwd = cwd.clone();
        tokio::task::spawn_blocking(move || nexo_agent::session::picker_rows(&cwd))
    };
    // what a crash left next to them goes in the background too, never before the first frame
    nexo_agent::session::sweep_orphans_in_background();
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
    // The rows leave out the sessions nothing was said in and the ones programs run, and put this folder's first.
    let sessions = listing.await.unwrap_or_default();
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
    let mut agent = tokio::spawn(nexo_agent::run(ends, agent_setup));
    let commands = handle.commands.clone();
    nexo_tui::set_supervise_options(nexo_supervise::Options::detect());
    if let Some(m) = mailbox {
        nexo_tui::set_mailbox(m);
    }
    let ui = nexo_tui::run(handle, opts).await;
    finish(&mut agent, &commands, CLOSE_WAIT).await;
    ui?;
    Ok(0)
}

/// Runs one request without the interface and returns the reply text and the exit code (0 done, 2 not done, 1 agent failure).
/// Error notices are printed on stderr as they come.
pub(super) async fn headless_collect(
    config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    prompt: String,
) -> anyhow::Result<(String, i32)> {
    nexo_agent::session::mark_headless();
    nexo_agent::session::sweep_orphans_in_background();
    let (mut handle, ends) = channels();
    let gate: std::sync::Arc<dyn nexo_core::PermissionGate> = std::sync::Arc::new(setup::HeadlessGate);
    let agent_setup = setup::agent_setup(&config, &cwd, &picked, None, Some(gate), None).await?;
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));
    let sent = handle.commands.send(AgentCommand::Submit { text: prompt, attachments: vec![] }).is_ok();
    // the address a failed connection adds to its notice, as `-p` says it
    let reach = config
        .providers
        .get(&picked.provider)
        .map(|p| crate::headless::unreachable_note(&picked.provider, &p.base_url));
    let (mut text, mut code) = (String::new(), 0);
    while let Some(ev) = handle.events.recv().await {
        match &ev {
            // the cause of a failed run (a provider that is off, a bad key) reaches stderr as it does for `-p`,
            // not only the generic line of the caller
            AgentEvent::Notice { level: nexo_core::agent::Level::Error, text: t, problem } => {
                let t = crate::headless::with_reach_note(
                    reach.as_deref(),
                    nexo_core::agent::Level::Error,
                    t,
                    problem.is_some(),
                );
                eprintln!(
                    "{}",
                    crate::headless::notice_line(
                        nexo_core::agent::Level::Error,
                        &t,
                        problem.is_some(),
                        crate::headless::color_ok()
                    )
                );
            }
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

/// How long leaving the interface waits for the agent to close its session. It is a few seconds more than the time
/// the agent gives a stuck turn (6 s), which leaves the save and the release room; a request in flight has to unwind
/// first, and the `session_end` hook comes after them and is cut off with the agent if it is still running. The
/// terminal is already restored, so nothing on screen waits with it.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Asks the agent to close its session and waits up to `wait` for it to finish (the `session_end` hook, the last
/// state write, the scratch wipe), then aborts it so a stuck close cannot keep the process alive. The interface
/// leaves without a `Shutdown` when the person hangs up or the event stream closes, so the close is asked for here.
async fn finish<T>(
    agent: &mut tokio::task::JoinHandle<T>,
    commands: &tokio::sync::mpsc::UnboundedSender<AgentCommand>,
    wait: std::time::Duration,
) {
    let _ = commands.send(AgentCommand::Shutdown);
    if tokio::time::timeout(wait, &mut *agent).await.is_err() {
        agent.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn leaving_waits_for_a_close_that_finishes_in_time() {
        let (commands, _ends) = tokio::sync::mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let mut agent = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            flag.store(true, Ordering::SeqCst);
        });
        finish(&mut agent, &commands, Duration::from_secs(5)).await;
        assert!(closed.load(Ordering::SeqCst), "finish returned before the close ended");
    }

    #[tokio::test]
    async fn leaving_aborts_a_close_that_never_ends_after_the_wait() {
        let (commands, mut ends) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = tokio::spawn(std::future::pending::<()>());
        let started = std::time::Instant::now();
        finish(&mut agent, &commands, Duration::from_millis(150)).await;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(matches!(ends.recv().await, Some(AgentCommand::Shutdown)), "the close was not asked for");
        let joined = agent.await.expect_err("the stuck task was not aborted");
        assert!(joined.is_cancelled());
    }
}
