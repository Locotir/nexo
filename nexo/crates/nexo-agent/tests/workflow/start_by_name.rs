use super::*;

#[tokio::test]
async fn an_allowed_start_by_name_runs_in_the_background_the_person_keeps_talking_and_nexo_writes_the_result() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "hello".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    assert_eq!(client.count("GATE hello"), 1, "the argument text reached the step");

    // the main agent is not busy with it: a turn starts and ends while the sub-agent is held
    r.submit("meanwhile qzx");
    let evs = r.turn().await;
    assert!(evs.iter().all(|e| !matches!(e, AgentEvent::FlowRun { info } if info.state != FlowState::Running)));
    assert_eq!(client.st.ended.load(Ordering::SeqCst), 0, "the run ended before it was let through");
    assert_eq!(client.main_msgs.lock().unwrap().len(), 1);

    // let it end: the result arrives as a turn the note starts
    client.st.gate.add_permits(1);
    let ended = |e: &AgentEvent| matches!(e, AgentEvent::FlowRun { info } if info.state == FlowState::Ok);
    r.until_both(ended, |e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    let note = client.main_msgs.lock().unwrap().last().cloned().unwrap();
    assert!(note.contains("nexo ran the saved workflow `saved`"), "{note}");
    assert!(note.contains("not from the person"), "{note}");
    assert!(note.contains("- a: ok"), "the report is in the note: {note}");
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_second_start_by_name_while_one_runs_is_refused_before_any_question() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "one".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "two".into() });
    let evs = r.until(|e| notice_has(e, "already running")).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "asked about the second");
    client.st.gate.add_permits(1);
    r.turn().await;
    assert_eq!(client.tasks().len(), 1);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_file_swapped_between_the_question_and_the_answer_is_refused_and_nothing_starts() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let req = r.ask().await;
    std::fs::write(d.path().join(".nexo/workflows/saved.toml"), BYNAME.replace("GATE", "GATE swapped")).unwrap();
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| notice_has(e, "changed after you were asked about it, so nothing was started")).await;
    assert!(client.tasks().is_empty(), "{:?}", client.tasks());
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn read_only_and_plan_mode_treat_a_start_by_name_as_the_tool_does() {
    for mode in [PermissionMode::ReadOnly, PermissionMode::Plan] {
        let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"look\"\nprompt = \"LOOK\"\n[[phase.step]]\nid = \"change\"\nagent = \"general\"\nprompt = \"CHANGE\"\n";
        let d = project_with("saved", text);
        let client =
            Mock::new(vec![("LOOK", says("looked")), ("CHANGE", Say::CallThen("edit", "the mode said no".into()))]);
        let tools = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("edit")));
        let mut r = start(&client, d.path(), mode, tools);
        r.next().await;
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: String::new() });
        let req = r.ask().await;
        assert_eq!(req.tool, "workflow", "{mode:?}: asked as the tool is");
        r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
        r.until(|e| matches!(e, AgentEvent::FlowRun { info } if info.state != FlowState::Running)).await;
        assert_eq!((client.count("LOOK"), client.count("CHANGE")), (1, 1), "{mode:?}");
        let log = client.st.log.lock().unwrap().clone();
        let result = log.iter().find(|(_, t)| t.starts_with("RESULT")).expect("a result came back");
        assert!(result.1.contains("Permission denied") && result.1.contains(mode.as_str()), "{mode:?}: {}", result.1);
        r.send(AgentCommand::Shutdown);
        r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
        r.task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn the_commands_stop_a_step_and_the_run_through_the_agent_and_the_interrupt_key_is_unchanged() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::with_main(
        vec![("HANG", Say::Hang)],
        vec![
            text_events("noted", 1, 1),
            call("w1", "workflow", json!({"name": "saved", "args": "t"})),
            text_events("after", 1, 1),
        ],
    );
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;

    // 1: by name, then stop the step: the run ends with the step failed
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "s".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if info.phases[0].steps[0].state == StepState::Running)).await;
    r.send(AgentCommand::FlowStopStep { run: "wf#1".into(), step: "a".into() });
    let evs = r
        .until_both(
            |e| matches!(e, AgentEvent::FlowRun { info } if info.state != FlowState::Running),
            |e| matches!(e, AgentEvent::TurnEnd { .. }),
        )
        .await;
    let end = evs.iter().rev().find_map(|e| match e {
        AgentEvent::FlowRun { info } => Some(info.clone()),
        _ => None,
    });
    let end = end.unwrap();
    assert_eq!((end.state, end.phases[0].steps[0].state), (FlowState::Problems, StepState::Failed));
    assert_eq!(end.phases[0].steps[0].why.as_deref(), Some("stopped by the person"));

    // 2: an unknown run is said to the person in one sentence
    r.send(AgentCommand::FlowStop { run: "wf#9".into() });
    r.until(|e| notice_has(e, "there is no workflow run wf#9")).await;

    // 3: Esc (the interrupt) in a turn that waits for a workflow ends the turn and the run, as before
    r.submit("run it");
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if info.id == "wf#2" && running(info, "a"))).await;
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    let end = evs.iter().rev().find_map(|e| match e {
        AgentEvent::FlowRun { info } => Some(info.clone()),
        _ => None,
    });
    assert_eq!(end.unwrap().state, FlowState::Stopped);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn clear_during_a_run_stops_it_and_none_of_its_events_reach_the_new_conversation() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "s".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await;
    r.send(AgentCommand::Clear);
    let mut evs = r.until(|e| matches!(e, AgentEvent::HistoryCleared)).await;
    // the new conversation goes on with the count (an old id is never given twice), and the old run says nothing
    // more: the first snapshot it hears is the new run beginning
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "again".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    evs.extend(r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await);
    // the old run has had its time to end: nothing it did is told
    evs.extend(drain(&mut r).await);
    let flows = flows_in(&evs);
    let first = flows[0];
    assert_eq!((first.id.as_str(), first.state), ("wf#2", FlowState::Running), "{flows:?}");
    assert!(flows.iter().all(|i| i.id != "wf#1"), "the old run spoke into the new conversation: {flows:?}");
    assert!(flows.iter().all(|i| i.state == FlowState::Running), "an old end reached the new one: {flows:?}");
    assert!(first.phases[0].steps.iter().all(|s| s.state == StepState::Waiting), "{first:?}");
    let named: Vec<&AgentEvent> = evs.iter().filter(|e| notice_has(e, "saved")).collect();
    assert!(named.is_empty(), "the new conversation was told of the old run: {named:?}");
    assert!(client.main_msgs.lock().unwrap().is_empty(), "the main agent got a note of the old run");
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}
