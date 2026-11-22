//! Session scheduler exercised through the real command and event channels.
use super::*;

fn loop_cmd(input: &str) -> AgentCommand {
    AgentCommand::Loop { input: input.into() }
}

async fn notice_containing(r: &mut Running, needle: &str) {
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains(needle))).await;
}

#[tokio::test]
async fn create_list_cancel_while_held() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Interrupt);
    r.send(loop_cmd("60s check things"));
    notice_containing(&mut r, "loop #1 scheduled").await;
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s: check things").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert!(client.requests().is_empty(), "Esc must hold even an already due loop");
    r.send(loop_cmd("cancel 1"));
    notice_containing(&mut r, "loop #1 cancelled").await;
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "no active loops").await;
    r.send(loop_cmd("cancel 1"));
    notice_containing(&mut r, "loop #1 not found").await;
    r.send(AgentCommand::Submit { text: "resume manually".into(), attachments: vec![] });
    r.turn().await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(client.requests().len(), 1, "cancellation prevents a later scheduled run");
    r.stop().await;
}

#[tokio::test]
async fn busy_turn_defers_loop_and_failed_switches_preserve_it() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Hang(vec![]), Reply::Events(text("scheduled answer"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "human".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TurnStart { .. })).await;
    r.send(loop_cmd("60s scheduled work"));
    notice_containing(&mut r, "loop #1 scheduled").await;
    r.send(AgentCommand::Resume { id: "nonexistent-session".into() });
    notice_containing(&mut r, "finish or interrupt the turn before resuming").await;
    r.send(AgentCommand::Fork { index: None });
    notice_containing(&mut r, "finish or interrupt the turn before forking").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(client.requests().len(), 1, "no overlap while the human turn hangs");
    r.send(AgentCommand::Interrupt);
    r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    r.send(AgentCommand::Resume { id: "nonexistent-session".into() });
    // FIFO list is a barrier after the failed resume, independently of its error text.
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s: scheduled work").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(client.requests().len(), 1, "Esc prevents the due loop starting after interruption");
    r.send(loop_cmd("cancel 1"));
    notice_containing(&mut r, "cancelled").await;
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn scheduled_prompt_runs_the_blocking_submit_hook() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("user_prompt_submit", None, "echo 'loop denied by policy' >&2; exit 2"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s forbidden work"));
    notice_containing(&mut r, "loop #1: running scheduled prompt").await;
    notice_containing(&mut r, "loop denied by policy").await;
    assert!(client.requests().is_empty(), "a denied scheduled prompt must not call the model");
    r.stop().await;
}

// A marker makes sure the hook is actually running before the command is sent.
// The release file is deliberately absent until after the command's response.
#[cfg(unix)]
async fn slow_hook_command_is_responsive(interrupt: bool) {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("hook-started");
    let release = d.path().join("hook-release");
    let client = Scripted::new(vec![]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!("touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done", marker.display(), release.display()),
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s slow hook"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("hook did not start");
    if interrupt {
        r.send(AgentCommand::Interrupt);
        r.send(loop_cmd("list"));
    } else {
        r.send(loop_cmd("cancel 1"));
    }
    let expected = if interrupt { "loop #1 every 60s [paused]" } else { "loop #1 cancelled" };
    let response = tokio::time::timeout(Duration::from_secs(2), notice_containing(&mut r, expected)).await;
    // Release even on failure so the child cannot outlive the test.
    std::fs::write(&release, "").unwrap();
    assert!(response.is_ok(), "command blocked behind a slow submit hook");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(client.requests().is_empty(), "cancelled preparation must not call the model");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn cancel_during_slow_scheduled_hook() {
    slow_hook_command_is_responsive(false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn interrupt_during_slow_scheduled_hook() {
    slow_hook_command_is_responsive(true).await;
}

#[tokio::test]
async fn due_loop_runs_after_busy_turn_finishes_without_overlap() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("human answer")), Reply::Events(text("loop answer"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    // A queued command is processed before the scheduler tick.
    r.send(AgentCommand::Submit { text: "human".into(), attachments: vec![] });
    r.send(loop_cmd("60s scheduled work"));
    r.turn().await;
    r.turn().await;
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].messages.iter().any(|m| m.text().contains("human")));
    assert!(requests[1].messages.iter().any(|m| m.text().contains("scheduled work")));
    r.stop().await;
}

#[tokio::test]
async fn failed_scheduled_turn_pauses_only_its_loop_and_person_can_resume() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new((0..3).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect());
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 2;
    cfg.agent.on_limit = nexo_core::config::OnLimit::Ask;
    let mut r = start(client, cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled work"));
    let events = r.turn().await;
    assert_eq!(end_of(&events), TurnEnd::MaxTurns);
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("loop #1 paused"))));
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s [paused]").await;
    r.send(loop_cmd("resume 1"));
    notice_containing(&mut r, "loop #1 resumed").await;
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s: scheduled work").await;
    r.stop().await;
}

#[tokio::test]
async fn esc_on_scheduled_turn_stays_paused_after_human_message() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Hang(vec![StreamEvent::TextDelta("partial".into())]),
        Reply::Events(text("human answer")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(loop_cmd("60s scheduled work"));
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Interrupt);
    assert_eq!(end_of(&r.turn().await), TurnEnd::Interrupted);
    r.send(AgentCommand::Submit { text: "human follow-up".into(), attachments: vec![] });
    assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s [paused]").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(client.requests().len(), 2, "human follow-up must not restart the loop");
    r.send(loop_cmd("resume 1"));
    notice_containing(&mut r, "loop #1 resumed").await;
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s: scheduled work").await;
    r.stop().await;
}

#[tokio::test]
async fn time_limit_pauses_scheduled_loop_until_person_resumes() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Hang(vec![StreamEvent::TextDelta("partial".into())])]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(loop_cmd("60s scheduled work"));
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::StopBy(nexo_core::agent::Stopper::TimeLimit));
    assert_eq!(end_of(&r.turn().await), TurnEnd::Interrupted);
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s [paused]").await;
    assert_eq!(client.requests().len(), 1);
    r.send(loop_cmd("resume 1"));
    notice_containing(&mut r, "loop #1 resumed").await;
    r.stop().await;
}

#[tokio::test]
async fn human_created_loop_retains_person_provenance() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("done"))]);
    let mut r = start(client, config(PermissionMode::Auto), None, d.path()).await;
    r.send(loop_cmd("60s human scheduled work"));
    let events = r.turn().await;
    let msg = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::History { message, .. } if message.text().contains("human scheduled work") => {
                Some(message.text())
            }
            _ => None,
        })
        .expect("scheduled history");
    assert!(msg.contains(nexo_core::agent::PERSON_MARK), "{msg}");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn innocent_list_does_not_restart_a_running_hook() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("starts");
    let release = d.path().join("release");
    let client = Scripted::new(vec![Reply::Events(text("done"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!("echo x >> '{}'; while [ ! -e '{}' ]; do sleep 0.1; done", marker.display(), release.display()),
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.send(loop_cmd("list"));
    tokio::time::timeout(Duration::from_secs(2), notice_containing(&mut r, "loop #1 every 60s")).await.unwrap();
    std::fs::write(&release, "").unwrap();
    tokio::time::timeout(Duration::from_secs(10), r.turn()).await.unwrap();
    assert_eq!(std::fs::read_to_string(marker).unwrap().lines().count(), 1);
    assert_eq!(client.requests().len(), 1);
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn clear_during_hook_prevents_old_session_turn() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("starts");
    let release = d.path().join("release");
    let client = Scripted::new(vec![]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!("touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done", marker.display(), release.display()),
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.send(AgentCommand::Clear);
    tokio::time::timeout(Duration::from_secs(2), notice_containing(&mut r, "session change: all loops stopped"))
        .await
        .unwrap();
    std::fs::write(&release, "").unwrap();
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "no active loops").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(client.requests().is_empty());
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn human_turn_wins_when_scheduled_hook_finishes_while_busy() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("starts");
    let release = d.path().join("release");
    let client = Scripted::new(vec![Reply::Hang(vec![])]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!(
            "if grep -q scheduled; then touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done; fi",
            marker.display(),
            release.display()
        ),
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.send(AgentCommand::Submit { text: "human".into(), attachments: vec![] });
    tokio::time::timeout(Duration::from_secs(5), r.until(|e| matches!(e, AgentEvent::TurnStart { .. }))).await.unwrap();
    std::fs::write(&release, "").unwrap();
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s").await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(client.requests().len(), 1);
    assert!(client.requests()[0].messages.iter().any(|m| m.text().contains("human")));
    r.send(AgentCommand::Interrupt);
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn denial_during_human_turn_still_pauses_loop() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("started");
    let release = d.path().join("release");
    let client = Scripted::new(vec![Reply::Hang(vec![])]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!(
        "if grep -q scheduled; then touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done; echo denied >&2; exit 2; fi",
        marker.display(), release.display()),
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.send(AgentCommand::Submit { text: "human".into(), attachments: vec![] });
    tokio::time::timeout(Duration::from_secs(5), r.until(|e| matches!(e, AgentEvent::TurnStart { .. }))).await.unwrap();
    std::fs::write(&release, "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), notice_containing(&mut r, "loop #1 paused: prompt was blocked"))
        .await
        .unwrap();
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s [paused]").await;
    assert_eq!(client.requests().len(), 1);
    r.send(AgentCommand::Interrupt);
    r.stop().await;
}

// A human turn and queued message invalidate even an otherwise approving hook.
#[cfg(unix)]
#[tokio::test]
async fn approved_hook_is_invalidated_by_human_and_queued_turns() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("started");
    let release = d.path().join("release");
    let client = Scripted::new(vec![
        Reply::Events(call("q1", "ask_user", json!({"question":"continue?", "options":["yes"]}))),
        Reply::Events(text("human done")),
        Reply::Events(text("queued done")),
        Reply::Events(text("scheduled done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    let count = d.path().join("hook-count");
    cfg.hooks.push(hook_cfg("user_prompt_submit", None, &format!(
        "if grep -q scheduled; then echo x >> '{}'; touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done; echo hook-context-unique; fi", count.display(), marker.display(), release.display())));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.send(AgentCommand::Submit { text: "human".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::Question { .. })).await;
    r.send(AgentCommand::Submit { text: "queued human".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::Queued { pending: 1 })).await;
    std::fs::write(&release, "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), notice_containing(&mut r, "loop #1 paused: preparation invalidated"))
        .await
        .unwrap();
    assert_eq!(client.requests().len(), 1);
    r.send(AgentCommand::Answer { id: "q1".into(), answer: Some("yes".into()) });
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client.requests().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|m| m.text().contains("queued human")));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(client.requests().len(), 2, "stale approval must not start scheduled turn");
    assert_eq!(std::fs::read_to_string(count).unwrap().lines().count(), 1, "hook must not be repeated");
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s [paused]").await;
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn hook_longer_than_interval_does_not_immediately_repeat() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("starts");
    let client = Scripted::new(vec![Reply::Events(text("done"))]);
    let mut cfg = config(PermissionMode::Auto);
    let mut hook = hook_cfg("user_prompt_submit", None, &format!("echo x >> '{}'; sleep 61", marker.display()));
    hook.timeout = Some(75);
    cfg.hooks.push(hook);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    loop {
        if matches!(r.raw_within(Duration::from_secs(75)).await, AgentEvent::TurnEnd { .. }) {
            break;
        }
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(client.requests().len(), 1);
    assert_eq!(std::fs::read_to_string(marker).unwrap().lines().count(), 1);
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn changed_config_while_hook_waits_pauses_before_admission() {
    let d = test_tmpdir().unwrap();
    let extra = d.path().join("extra");
    std::fs::create_dir(&extra).unwrap();
    let marker = d.path().join("started");
    let release = d.path().join("release");
    let client = Scripted::new(vec![]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!("touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done", marker.display(), release.display()),
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.send(AgentCommand::AddDir(extra));
    tokio::time::timeout(Duration::from_secs(5), notice_containing(&mut r, "added ")).await.unwrap();
    std::fs::write(&release, "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), notice_containing(&mut r, "preparation interrupted")).await.unwrap();
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s [paused]").await;
    assert!(client.requests().is_empty());
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn queued_cancel_wins_when_hook_is_released() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("started");
    let release = d.path().join("release");
    let client = Scripted::new(vec![]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!("touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done", marker.display(), release.display()),
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    r.send(loop_cmd("cancel 1"));
    std::fs::write(&release, "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), notice_containing(&mut r, "loop #1 cancelled")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(client.requests().is_empty());
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn monitor_output_survives_pending_hook_and_is_delivered_after_cancel() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("hook-started");
    let release = d.path().join("hook-release");
    let client = Scripted::new(vec![
        Reply::Events(call(
            "bash",
            "bash",
            json!({"command":"while [ ! -f emit ]; do sleep 0.05; done; echo monitor-pending-marker; while [ ! -f finish ]; do sleep 0.05; done", "background":true}),
        )),
        Reply::Events(call("monitor", "monitor", json!({"action":"create", "id":1}))),
        Reply::Events(text("monitoring")),
        Reply::Events(text("received")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "user_prompt_submit",
        None,
        &format!(
            "if grep -q scheduled; then touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done; fi",
            marker.display(),
            release.display()
        ),
    ));
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "start monitor".into(), attachments: vec![] });
    r.turn().await;
    r.send(loop_cmd("60s scheduled"));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(d.path().join("emit"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(client.requests().len(), 3, "monitor must not overlap preparation");
    r.send(loop_cmd("cancel 1"));
    notice_containing(&mut r, "loop #1 cancelled").await;
    std::fs::write(&release, "").unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !client.requests().iter().any(|q| q.messages.iter().any(|m| m.text().contains("monitor-pending-marker")))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(d.path().join("finish"), "").unwrap();
    r.stop().await;
}
