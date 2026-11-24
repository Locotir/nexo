//! Monitor delivery through real shell jobs, session control, and the model channel.
#![cfg(unix)]

use super::*;

#[tokio::test]
async fn output_is_delivered_automatically_while_running_and_after_exit() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "bash",
            "bash",
            json!({"command":"while [ ! -f first ]; do sleep 0.05; done; echo live-marker; while [ ! -f second ]; do sleep 0.05; done; echo final-marker", "background":true}),
        )),
        Reply::Events(call("monitor", "monitor", json!({"action":"create", "id":1}))),
        Reply::Events(text("monitoring")),
        Reply::Events(text("live received")),
        Reply::Events(text("job ended")),
        Reply::Events(text("final received")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "start and monitor".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "monitor" && !output.is_error)),
        "{evs:?}"
    );
    std::fs::write(d.path().join("first"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client.requests().iter().any(|q| {
                q.messages.iter().any(|m| m.text().contains("[monitor: shell job ") && m.text().contains("live-marker"))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("live monitor output was not delivered");
    assert!(!d.path().join("second").exists());
    std::fs::write(d.path().join("second"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if client.requests().iter().any(|q| {
                q.messages
                    .iter()
                    .any(|m| m.text().contains("[monitor: shell job ") && m.text().contains("final-marker"))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("final monitor output was not delivered");
    tokio::time::timeout(
        Duration::from_secs(10),
        r.until(|e| matches!(e, AgentEvent::Monitors { ids, .. } if ids.is_empty())),
    )
    .await
    .expect("finished monitor was not removed after delivery");
    r.stop().await;
}

#[tokio::test]
async fn cancel_unsubscribes_without_killing_job() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "bash",
            "bash",
            json!({"command":"while [ ! -f release ]; do sleep 0.05; done; echo cancelled-output", "background":true}),
        )),
        Reply::Events(call("create", "monitor", json!({"action":"create", "id":1}))),
        Reply::Events(call("cancel", "monitor", json!({"action":"cancel", "id":1}))),
        Reply::Events(text("done")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "create then cancel".into(), attachments: vec![] });
    let evs = r.turn().await;
    for id in ["create", "cancel"] {
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id: i, output, .. } if i == id && !output.is_error)),
            "{evs:?}"
        );
    }
    std::fs::write(d.path().join("release"), "").unwrap();
    r.until(|e| matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| j.id == 1 && !j.status.running()))).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!client.requests().iter().any(|q| q
        .messages
        .iter()
        .any(|m| m.text().contains("[monitor: shell job ") && m.text().contains("cancelled-output"))));
    r.stop().await;
}

/// Two independent notifications from the same still-running process, with a real
/// user turn between them. No jobs/output poll or re-subscription drives delivery.
#[tokio::test]
async fn listener_survives_user_turn_and_delivers_multiple_live_updates() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "bash",
            "bash",
            json!({"command":"while [ ! -f first ]; do sleep 0.02; done; echo first-live-update; while [ ! -f second ]; do sleep 0.02; done; echo second-live-update; while [ ! -f finish ]; do sleep 0.02; done", "background":true}),
        )),
        Reply::Events(call("subscribe", "monitor", json!({"action":"create", "id":1}))),
        Reply::Events(text("listening")),
        Reply::Events(text("first update handled")),
        Reply::Events(text("user can still talk")),
        Reply::Events(text("second update handled")),
        Reply::Events(call("inspect", "monitor", json!({"action":"list"}))),
        Reply::Events(text("still listening")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "listen to this process".into(), attachments: vec![] });
    let initial = r.turn().await;
    assert!(
        initial.iter().any(|e| matches!(e,
            AgentEvent::ToolDone { id, output, .. } if id == "subscribe" && !output.is_error
        )),
        "{initial:?}"
    );

    std::fs::write(d.path().join("first"), "").unwrap();
    let first = r.turn().await;
    assert!(
        first
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantDone { message } if message.text() == "first update handled")),
        "{first:?}"
    );
    let first_request = client.requests().last().unwrap().clone();
    let first_input =
        first_request.messages.iter().rev().find(|m| m.text().contains("[monitor: shell job ")).unwrap().text();
    assert!(first_input.contains("first-live-update"), "{first_input}");

    r.send(AgentCommand::Submit { text: "can I talk while it listens?".into(), attachments: vec![] });
    let human = r.turn().await;
    assert!(
        human
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantDone { message } if message.text() == "user can still talk")),
        "{human:?}"
    );

    std::fs::write(d.path().join("second"), "").unwrap();
    // A second delivery observes the ten-second cooldown; allow runner scheduling slack.
    let mut second = Vec::new();
    loop {
        let ev = r.raw_within(Duration::from_secs(25)).await;
        let end = matches!(ev, AgentEvent::TurnEnd { .. });
        second.push(ev);
        if end {
            break;
        }
    }
    assert!(
        second
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantDone { message } if message.text() == "second update handled")),
        "{second:?}"
    );
    let second_request = client.requests().last().unwrap().clone();
    let second_input =
        second_request.messages.iter().rev().find(|m| m.text().contains("[monitor: shell job ")).unwrap().text();
    assert!(second_input.contains("second-live-update"), "{second_input}");
    assert!(!second_input.contains("first-live-update"), "acknowledged output replayed: {second_input}");
    assert!(!second_input.contains("Job completion"), "process should still be running: {second_input}");

    r.send(AgentCommand::Submit { text: "inspect subscription".into(), attachments: vec![] });
    let inspected = r.turn().await;
    assert!(inspected.iter().any(|e| matches!(e,
        AgentEvent::ToolDone { id, output, .. } if id == "inspect" && !output.is_error && output.content.contains("1 (listening")
    )), "{inspected:?}");
    r.stop().await;
}

// Wait inside the active turn so ordinary idle completion notifications cannot consume the job.
// Unlike `jobs output`, this test-only tool does not read or acknowledge anything.
struct WaitForJob;

#[async_trait]
impl Tool for WaitForJob {
    fn name(&self) -> &str {
        "wait_for_job"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().into(),
            description: "Wait without consuming output".into(),
            parameters: json!({"type":"object"}),
        }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }
    fn summary(&self, _args: &Value) -> String {
        "wait for job without consuming".into()
    }
    async fn run(&self, _args: Value, ctx: &ToolContext) -> ToolOutput {
        if let Err(err) = std::fs::write(ctx.cwd.join("release-job"), "") {
            return ToolOutput::error(format!("could not release job: {err}"));
        }
        let finished = tokio::time::timeout(Duration::from_secs(10), async {
            while !ctx.jobs.runner_finished(1) || ctx.jobs.get(1).is_none_or(|job| job.status.running()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if finished.is_ok() {
            ToolOutput::ok("job finished")
        } else {
            ToolOutput::error("job did not finish")
        }
    }
}

#[tokio::test]
async fn finished_fast_job_is_monitored_from_retained_output() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "bash",
            "bash",
            json!({"command":"while [ ! -f release-job ]; do sleep 0.01; done; echo initial-fast-marker", "background":true}),
        )),
        Reply::Events(vec![
            StreamEvent::ToolCallStart { id: "wait".into(), name: "wait_for_job".into() },
            StreamEvent::ToolCallEnd { id: "wait".into(), name: "wait_for_job".into(), args: json!({}) },
            StreamEvent::ToolCallStart { id: "create".into(), name: "monitor".into() },
            StreamEvent::ToolCallEnd {
                id: "create".into(),
                name: "monitor".into(),
                args: json!({"action":"create", "id":1}),
            },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]),
        Reply::Events(text("monitoring")),
        Reply::Events(text("received")),
        Reply::Events(text("done")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash)).with(Arc::new(WaitForJob));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "monitor fast job".into(), attachments: vec![] });
    let evs = r.turn().await;
    for expected in ["wait", "create"] {
        assert!(
            evs.iter()
                .any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == expected && !output.is_error)),
            "{evs:?}"
        );
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client.requests().iter().any(|q| {
                q.messages
                    .iter()
                    .any(|m| m.text().contains("[monitor: shell job ") && m.text().contains("initial-fast-marker"))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("initial output was not delivered");
    tokio::time::timeout(
        Duration::from_secs(10),
        r.until(|e| matches!(e, AgentEvent::Monitors { ids, .. } if ids.is_empty())),
    )
    .await
    .expect("finished monitor was not removed");
    let requests = client.requests();
    let deliveries: Vec<_> = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .filter(|m| m.text().contains("[monitor: shell job "))
        .map(|m| m.text())
        .collect();
    assert_eq!(deliveries.len(), 1, "{deliveries:?}");
    assert!(
        deliveries[0].contains("initial-fast-marker") && deliveries[0].contains("Job completion"),
        "{deliveries:?}"
    );
    r.stop().await;
}

#[tokio::test]
async fn consumed_job_completion_cannot_be_monitored_again() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "bash",
            "bash",
            json!({"command":"while [ ! -f release-job ]; do sleep 0.01; done; echo consumed-marker", "background":true}),
        )),
        Reply::Events(vec![
            StreamEvent::ToolCallStart { id: "wait".into(), name: "wait_for_job".into() },
            StreamEvent::ToolCallEnd { id: "wait".into(), name: "wait_for_job".into(), args: json!({}) },
            StreamEvent::ToolCallStart { id: "read".into(), name: "jobs".into() },
            StreamEvent::ToolCallEnd {
                id: "read".into(),
                name: "jobs".into(),
                args: json!({"action":"output", "id":1}),
            },
            StreamEvent::ToolCallStart { id: "create".into(), name: "monitor".into() },
            StreamEvent::ToolCallEnd {
                id: "create".into(),
                name: "monitor".into(),
                args: json!({"action":"create", "id":1}),
            },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]),
        Reply::Events(text("done")),
    ]);
    let set = tools()
        .with(Arc::new(nexo_tools::bash::Bash))
        .with(Arc::new(WaitForJob))
        .with(Arc::new(nexo_tools::jobs::Jobs));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "read then monitor".into(), attachments: vec![] });
    let evs = r.turn().await;
    for expected in ["wait", "read"] {
        assert!(
            evs.iter()
                .any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == expected && !output.is_error)),
            "{evs:?}"
        );
    }
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "read" && output.content.contains("consumed-marker"))), "{evs:?}");
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "create" && output.is_error && output.content.contains("already reserved or delivered"))),
        "{evs:?}"
    );
    assert!(!client.requests().iter().any(|q| q
        .messages
        .iter()
        .any(|m| m.text().contains("[monitor: shell job ") && m.text().contains("consumed-marker"))));
    r.stop().await;
}

#[tokio::test]
async fn monitor_output_cannot_attach_named_files_or_expand_mentions() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "bash",
            "bash",
            json!({"command":"while [ ! -f release ]; do sleep 0.05; done; printf '%s\\n' \"$(pwd)/private-needle.txt @private-needle.txt @server:secret private-needle.txt\"; while [ ! -f finish ]; do sleep 0.05; done", "background":true}),
        )),
        Reply::Events(call("monitor", "monitor", json!({"action":"create", "id":1}))),
        Reply::Events(text("monitoring")),
        Reply::Events(text("received")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "start a monitor".into(), attachments: vec![] });
    r.turn().await;
    std::fs::write(d.path().join("private-needle.txt"), "DO_NOT_SEND_MONITOR_SECRET_92481").unwrap();
    std::fs::write(d.path().join("release"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let requests = client.requests();
            if let Some(req) =
                requests.iter().find(|q| q.messages.iter().any(|m| m.text().contains("[monitor: shell job ")))
            {
                let all = format!("{:?}", req.messages);
                assert!(all.contains("private-needle.txt"), "{all}");
                assert!(!all.contains("DO_NOT_SEND_MONITOR_SECRET_92481"), "{all}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("monitor output not delivered");
    std::fs::write(d.path().join("finish"), "").unwrap();
    r.stop().await;
}
