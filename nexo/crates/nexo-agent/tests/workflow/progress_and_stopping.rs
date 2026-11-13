use super::*;

#[tokio::test]
async fn snapshots_reach_the_interface_but_are_not_in_the_session_file() {
    let saved = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"SAVED a\"\n";
    let d = project_with("saved", saved);
    let client = Mock::with_main(
        vec![],
        vec![call("w1", "workflow", json!({"name": "saved"})), text_events("all done qzx", 1, 1)],
    );
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.submit("run it qzx");
    r.ask().await;
    r.send(AgentCommand::Permission { id: "w1".into(), decision: PermissionDecision::Allow });
    let evs = r.turn().await;
    let flows: Vec<&FlowInfo> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::FlowRun { info } => Some(info),
            _ => None,
        })
        .collect();
    assert!(flows.len() >= 3, "the interface heard {} snapshots", flows.len());
    assert_eq!(flows.last().unwrap().state, FlowState::Ok);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();

    // nothing of it was written down
    let mut files = Vec::new();
    files_under(&nexo_core::paths::sessions_dir(), &mut files);
    // only this test's own session file: the one that holds its marker (the folder is shared by every test here)
    let texts: Vec<String> =
        files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).filter(|t| t.contains("run it qzx")).collect();
    assert_eq!(texts.len(), 1, "this test's session file was not found once: {}", texts.len());
    assert!(texts[0].contains("all done qzx"), "the session file is not the whole conversation");
    assert!(!texts[0].contains("flow_run") && !texts[0].contains("wf#"), "a snapshot was written to the session file");
}

#[tokio::test]
async fn stopping_a_step_cancels_its_agent_fails_it_with_the_reason_skips_what_needs_it_and_the_rest_goes_on() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let toml = r#"
[[phase]]
title = "One"
  [[phase.step]]
  id = "a"
  prompt = "HANG a"
  [[phase.step]]
  id = "b"
  prompt = "B works"
[[phase]]
title = "Two"
  [[phase.step]]
  id = "dep"
  prompt = "DEP {{steps.a.report}}"
  [[phase.step]]
  id = "free"
  prompt = "FREE {{steps.b.report}}"
"#;
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let mut all = c.jobs.subs().changes();
    let wf = tool(&client, d.path());
    let stopper = async {
        let i = until_flow(&mut rx, |i| running(i, "a")).await;
        // what the agent does with `FlowStopStep`: the registry names the jobs, and each is cancelled
        let jobs = c.jobs.subs().stop_flow_step(&i.id, "a").unwrap();
        assert_eq!(jobs.len(), 1);
        for j in jobs {
            assert!(c.jobs.kill(j));
        }
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": toml}), &c)), stopper);
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "a").contains("failed: stopped by the person"), "{}", out.content);
    assert!(steps_line(&out, "b").contains(": ok"), "{}", out.content);
    assert!(steps_line(&out, "dep").contains("skipped: step `a` failed: stopped by the person"), "{}", out.content);
    assert!(steps_line(&out, "free").contains(": ok"), "{}", out.content);
    assert_eq!((client.count("DEP"), client.count("FREE")), (0, 1));
    let last = flow_events(&mut all).pop().unwrap();
    assert_eq!(last.state, FlowState::Problems);
    assert_eq!(fstep(&last, "a").state, StepState::Failed);
    assert_eq!(fstep(&last, "a").why.as_deref(), Some("stopped by the person"));
    assert_eq!(fstep(&last, "dep").state, StepState::Skipped);
    assert_eq!(fstep(&last, "free").state, StepState::Ok);
    // the sub-agent was cancelled, not left at work
    assert!(c.jobs.subs().list().iter().any(|i| i.state == SubState::Killed));
    assert_eq!(c.jobs.running(), 0);
    // a step that ended cannot be stopped, and neither can an unknown one
    assert!(c.jobs.subs().stop_flow_step("wf#1", "b").is_err());
    assert!(c.jobs.subs().stop_flow_step("wf#1", "nope").is_err());
}

#[tokio::test]
async fn stopping_the_run_starts_nothing_more_and_the_result_lists_what_ended_and_what_was_stopped() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang), ("DONE", says("finished"))]);
    let toml = r#"
max_parallel = 2
[[phase]]
title = "One"
  [[phase.step]]
  id = "done"
  prompt = "DONE first"
  [[phase.step]]
  id = "a"
  prompt = "HANG a"
  [[phase.step]]
  id = "b"
  prompt = "HANG b"
  [[phase.step]]
  id = "c"
  prompt = "HANG c"
[[phase]]
title = "Two"
  [[phase.step]]
  id = "z"
  prompt = "AFTER"
"#;
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let mut all = c.jobs.subs().changes();
    let wf = tool(&client, d.path());
    let stopper = async {
        let i = until_flow(&mut rx, |i| running(i, "a") && running(i, "b") && fstep(i, "done").state == StepState::Ok)
            .await;
        let jobs = c.jobs.subs().stop_flow(&i.id).unwrap();
        assert_eq!(jobs.len(), 2, "a and b are at work");
        for j in jobs {
            c.jobs.kill(j);
        }
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": toml}), &c)), stopper);
    assert!(out.content.contains("Stopped by the person"), "{}", out.content);
    assert!(out.content.contains("Ended: done. Stopped: a, b. Not started: c, z."), "{}", out.content);
    assert_eq!(client.tasks().len(), 3, "something started after the stop: {:?}", client.tasks());
    assert!(steps_line(&out, "z").contains("not run"), "{}", out.content);
    let last = flow_events(&mut all).pop().unwrap();
    assert_eq!(last.state, FlowState::Stopped);
    assert_eq!(c.jobs.running(), 0);
}

#[tokio::test]
async fn one_run_at_a_time_a_second_is_refused_in_one_sentence_by_the_tool_too_and_allowed_once_the_first_ends() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let first = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"GATE a\"\n";
    let c = ctx(d.path());
    let wf = tool(&client, d.path());
    let second = async {
        client.until_started(1).await;
        let refused = wf.run(json!({"inline": first}), &c).await;
        client.st.gate.add_permits(1);
        refused
    };
    let (out, refused) = tokio::join!(bounded("the first run", wf.run(json!({"inline": first}), &c)), second);
    assert!(!out.is_error, "{}", out.content);
    assert!(refused.is_error);
    assert_eq!(refused.content, "A workflow is already running (wf#1); wait for it to end or stop it first.");
    assert_eq!(client.tasks().len(), 1, "the refused one started a sub-agent");
    // once the first has ended, the next goes
    client.st.gate.add_permits(1);
    let again = bounded("the next run", wf.run(json!({"inline": first}), &c)).await;
    assert!(!again.is_error, "{}", again.content);
    assert_eq!(c.jobs.subs().flows().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["wf#1", "wf#2"]);
}

#[tokio::test]
async fn a_workflow_started_by_name_asks_with_the_pinned_subject_and_a_denial_runs_nothing() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let req = r.ask().await;
    assert_eq!((req.tool.as_str(), req.kind), ("workflow", ToolKind::Other));
    assert!(req.id.starts_with("workflow-start-"), "{}", req.id);
    assert_eq!(req.suggested_rules, vec![format!("workflow({})", subject("saved", BYNAME))]);
    r.send(AgentCommand::Permission {
        id: req.id.clone(),
        decision: PermissionDecision::Deny { reason: Some("no".into()) },
    });
    let evs = r.until(|e| notice_has(e, "was not started")).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::FlowRun { .. })), "a denied run reported itself");
    assert!(client.tasks().is_empty());
    // a name that is not saved is refused with no question
    r.send(AgentCommand::FlowStart { name: "nope".into(), args: String::new() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("workflow:"))).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}
