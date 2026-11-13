use super::*;

#[tokio::test]
async fn the_run_count_goes_on_after_clear_fork_and_resume_and_each_replaces_the_run_before() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let (mut r, sid1) = recorded(&client, d.path()).await;
    let mut n = 0;
    for how in [None, Some(Replace::Clear), Some(Replace::Fork), Some(Replace::Resume)] {
        if let Some(how) = how {
            replace(&mut r, how, &sid1).await;
        }
        n += 1;
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: n.to_string() });
        let req = r.ask().await;
        r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
        let evs = r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await;
        let id = format!("wf#{n}");
        assert!(flows_in(&evs).iter().all(|i| i.id == id), "{how:?}: {:?}", flows_in(&evs));
    }
    stop_agent(r).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_that_ends_after_a_clear_fires_no_hook_and_tells_nothing_to_the_new_conversation() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let cfg = with_hook("post_tool", "workflow", "echo ran > post-ran.txt");
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "s".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await;
    r.send(AgentCommand::Clear);
    let mut evs = r.until(|e| matches!(e, AgentEvent::HistoryCleared)).await;
    // the old run ends in the cleared session; the hooks of the new one are not run for it
    evs.extend(drain(&mut r).await);
    evs.extend(drain(&mut r).await);
    assert!(!d.path().join("post-ran.txt").exists(), "a hook ran for a run of a session that is gone");
    assert!(!evs.iter().any(|e| notice_has(e, "saved")), "{evs:?}");
    assert!(client.main_msgs.lock().unwrap().is_empty());
    stop_agent(r).await;
}

#[tokio::test]
async fn a_sub_agent_that_ends_while_the_main_agent_is_idle_starts_no_turn_and_the_run_makes_one_note() {
    let two = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"DONE a\"\n[[phase.step]]\nid = \"b\"\nprompt = \"GATE b\"\n";
    let d = project_with("saved", two);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: String::new() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    // one step ended and the other is held: the run goes on, and the main agent has been told nothing
    let mut evs = r
        .until(|e| matches!(e, AgentEvent::FlowRun { info } if fstep(info, "a").state == StepState::Ok && running(info, "b")))
        .await;
    evs.extend(drain(&mut r).await);
    assert!(client.main_msgs.lock().unwrap().is_empty(), "a turn started: {:?}", client.main_msgs.lock().unwrap());
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "{evs:?}");
    // the end of the run is one note, once
    client.st.gate.add_permits(1);
    r.turn().await;
    evs = drain(&mut r).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "a second turn: {evs:?}");
    assert_eq!(client.main_msgs.lock().unwrap().len(), 1);
    assert!(client.main_msgs.lock().unwrap()[0].contains("nexo ran the saved workflow `saved`"));
    stop_agent(r).await;
}

#[tokio::test]
async fn a_stopped_step_does_not_start_the_items_that_waited_for_a_place_and_ends_failed_with_the_reason() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[\"DONE one\", \"HANG two\", \"HANG three\"]")), ("HANG", Say::Hang)]);
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let wf = tool(&client, d.path());
    let stopper = async {
        // find, the first item (ends at once) and the second (held): the third waits for a place
        client.until_started(3).await;
        let i = until_flow(&mut rx, |i| running(i, "check")).await;
        let jobs = c.jobs.subs().stop_flow_step(&i.id, "check").unwrap();
        assert_eq!(jobs.len(), 1, "only the second item is at work");
        for j in jobs {
            assert!(c.jobs.kill(j));
        }
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": EACH_STOP}), &c)), stopper);
    assert!(steps_line(&out, "check").contains("failed: stopped by the person"), "{}", out.content);
    assert_eq!(client.count("HANG three"), 0, "an item of a stopped step started: {:?}", client.tasks());
    assert_eq!(c.jobs.running(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_stop_that_comes_while_a_sub_agent_is_starting_reaches_it_as_soon_as_it_has_started() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    // the hook of the sub-agent's start holds the start until the test lets it go
    let hold = nexo_core::config::HookConfig {
        event: "subagent_start".into(),
        command: "touch starting; while [ ! -f go ]; do sleep 0.02; done".into(),
        ..Default::default()
    };
    let sub = sub_agent(&client, ToolSet::new().with(Arc::new(Read))).with_hooks(vec![hold]);
    let wf = Workflow::new(Arc::new(sub), d.path().to_path_buf());
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"HANG a\"\n";
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let stopper = async {
        let i = until_flow(&mut rx, |_| true).await;
        let end = tokio::time::Instant::now() + PATIENCE;
        while !d.path().join("starting").exists() {
            assert!(tokio::time::Instant::now() < end, "the sub-agent never began to start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // no sub-agent is at work yet, so the stop has nobody to cancel
        assert!(c.jobs.subs().stop_flow_step(&i.id, "a").unwrap().is_empty());
        std::fs::write(d.path().join("go"), "").unwrap();
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": toml}), &c)), stopper);
    assert!(steps_line(&out, "a").contains("failed: stopped by the person"), "{}", out.content);
    assert_eq!(c.jobs.running(), 0);
}
