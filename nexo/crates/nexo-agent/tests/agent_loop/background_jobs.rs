use super::*;

#[tokio::test]
async fn a_background_command_lets_the_turn_end_and_its_end_reaches_the_model() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo hi", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("saw it")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    let mut evs = Vec::new();
    loop {
        let ev = r.raw_within(Duration::from_secs(30)).await;
        let idle = matches!(ev, AgentEvent::Idle { running_jobs: 0 });
        evs.push(ev);
        if idle {
            break;
        }
    }
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "b1" && output.content.starts_with("Started background job 1"))));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| j.id == 1 && !j.status.running()))));
    // the model hears that it ended: with the tool results, or in a turn of its own
    let reqs = client.requests();
    let told = reqs.iter().skip(1).any(|q| {
        q.messages
            .iter()
            .any(|m| m.role == nexo_core::message::Role::User && m.text().contains("[background jobs ended]"))
    });
    assert!(told, "the ended job is reported to the model");
    assert!(text_of(&evs).ends_with("saw it") || reqs.len() == 2);
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn moving_to_another_session_stops_the_jobs_of_the_one_left() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 30", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("in the fork")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Fork { index: None });
    r.until(|e| {
        matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| j.id == 1 && j.status == nexo_core::jobs::JobStatus::Killed))
    })
    .await;
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "in the fork");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3, "no turn of its own for the old session's job");
    assert!(
        !reqs[2].messages.iter().any(|m| m.text().contains("[background jobs ended]")),
        "the old session's job is not reported in the new one"
    );
    r.stop().await;
}

#[tokio::test]
async fn asking_for_the_job_tails_answers_with_the_end_of_each_output() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo hi", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("saw it")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client, config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    loop {
        if matches!(r.raw().await, AgentEvent::Idle { running_jobs: 0 }) {
            break;
        }
    }
    r.send(AgentCommand::JobTails);
    loop {
        if let AgentEvent::JobTails { tails } = r.raw().await {
            assert_eq!(tails.len(), 1);
            assert_eq!(tails[0].id, 1);
            assert_eq!(tails[0].text, "hi");
            assert!(tails[0].ended_ago_ms.is_some());
            break;
        }
    }
    r.stop().await;
}

// ------------------------------------------------------------ checkpoints at agent.max_turns

#[cfg(unix)]
#[tokio::test]
async fn a_background_command_past_its_time_is_weighed_apart_and_stopped_when_the_review_says_stop() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo waiting; sleep 30", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("STOP")),
        Reply::Events(text("it hung, so it was stopped")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.job_review = 1;
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    r.turn().await;
    // the job's end, told to the idle agent, starts a turn of its own
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "it hung, so it was stopped");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    let review = &reqs[2];
    assert!(review.tools.is_empty() && review.system.contains("CONTINUE or STOP"), "a tool-less request of its own");
    assert!(review.messages.len() == 1 && review.messages[0].text().contains("echo waiting; sleep 30"));
    assert!(review.messages[0].text().contains("run it in the background"));
    let told = reqs[3].messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n");
    assert!(told.contains("[background jobs ended]") && told.contains("stopped: a review judged it"), "{told}");
    assert!(
        !reqs[3].messages.iter().any(|m| m.text().contains("CONTINUE or STOP")),
        "the review stays out of the conversation"
    );
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_review_that_says_continue_leaves_the_command_running() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 3", "background": true}))),
        Reply::Events(text("started")),
        // every request from here on, the looks and the turn the job's end starts, hears CONTINUE
        Reply::Events(text("CONTINUE")),
        Reply::Events(text("CONTINUE")),
        Reply::Events(text("CONTINUE")),
        Reply::Events(text("CONTINUE")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.job_review = 1;
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    r.turn().await;
    r.turn().await;
    let reqs = client.requests();
    assert!(reqs.iter().any(|q| q.system.contains("CONTINUE or STOP")), "it was looked at");
    let told = reqs.last().unwrap().messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n");
    assert!(told.contains("done (exit 0)"), "{told}");
    r.stop().await;
}
