use super::*;

#[cfg(unix)]
#[tokio::test]
async fn a_hook_decision_reaches_the_permission_of_a_start_by_name() {
    let d = project_with("saved", &quick());
    let client = Mock::new(vec![]);
    let cfg = with_hook(
        "pre_tool",
        "workflow",
        r#"case "$(cat)" in *allowme*) echo '{"decision":"allow"}';; *denyme*) echo '{"decision":"deny","reason":"the hook says no"}';; esac"#,
    );
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    // allow: it runs with no question, though the mode asks
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "allowme".into() });
    let ended = |e: &AgentEvent| matches!(e, AgentEvent::FlowRun { info } if info.state == FlowState::Ok);
    let evs = r.until(|e| ended(e) || matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(ended(evs.last().unwrap()), "it asked though the hook allowed it: {:?}", evs.last());
    r.turn().await;
    // deny: refused with the hook's reason and no question
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "denyme".into() });
    let evs = r.until(|e| notice_has(e, "was not started") || matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(notice_has(evs.last().unwrap(), "the hook says no"), "{:?}", evs.last());
    assert_eq!(client.count("DONE"), 1);
    // no decision: the person is asked, as before
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "other".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Deny { reason: None } });
    r.until(|e| notice_has(e, "was not started")).await;
    stop_agent(r).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_hook_that_replaces_the_input_of_a_start_by_name_gets_the_new_file_pinned_and_asked() {
    let other = "[[phase]]\ntitle = \"B\"\n[[phase.step]]\nid = \"b\"\nprompt = \"GATE swapped {{args}}\"\n";
    let d = project_with("saved", BYNAME);
    std::fs::write(d.path().join(".nexo/workflows/other.toml"), other).unwrap();
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let cfg = with_hook(
        "pre_tool",
        "workflow",
        r#"case "$(cat)" in *first*) echo '{"updated_input":{"name":"other","args":"now"}}';; esac"#,
    );
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "first".into() });
    let req = r.ask().await;
    // the question is about what will run: the other file, pinned as it is now
    assert_eq!(req.suggested_rules, vec![format!("workflow({})", subject("other", other))]);
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    assert_eq!(client.tasks(), vec!["GATE swapped now".to_string()]);
    client.st.gate.add_permits(1);
    r.turn().await;
    stop_agent(r).await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_post_tool_hook_of_a_start_by_name_fires_and_its_words_reach_the_main_agent_in_the_note() {
    let d = project_with("saved", &quick());
    let client = Mock::new(vec![]);
    let cfg = with_hook("post_tool", "workflow", "cat > post.json; echo 'formatted it' >&2; exit 2");
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    let ended = |e: &AgentEvent| matches!(e, AgentEvent::FlowRun { info } if info.state == FlowState::Ok);
    r.until_both(ended, |e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    let note = client.main_msgs.lock().unwrap().last().cloned().unwrap();
    assert!(note.contains("[hook] formatted it"), "{note}");
    let seen: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("post.json")).unwrap()).unwrap();
    assert_eq!(seen["tool_name"], "workflow");
    stop_agent(r).await;
}

#[tokio::test]
async fn a_second_start_while_the_first_is_asking_is_refused_in_one_sentence_and_a_denial_frees_the_slot() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "one".into() });
    let first = r.ask().await;
    // the question is open and no run exists yet: the slot is what refuses
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "two".into() });
    let evs = r.until(|e| notice_has(e, "already starting") || matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(notice_has(evs.last().unwrap(), "already starting"), "a second question was asked: {:?}", evs.last());
    r.send(AgentCommand::Permission { id: first.id, decision: PermissionDecision::Deny { reason: None } });
    r.until(|e| notice_has(e, "was not started")).await;
    // a denial frees it: the next start is asked
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "three".into() });
    let again = r.ask().await;
    r.send(AgentCommand::Permission { id: again.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    assert_eq!(client.tasks(), vec!["GATE three".to_string()]);
    // and the slot is held until the run ends: a start while it runs is refused before any question
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "four".into() });
    let evs = r.until(|e| notice_has(e, "already")).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    client.st.gate.add_permits(1);
    r.turn().await;
    // the end frees it too
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "five".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Deny { reason: None } });
    r.until(|e| notice_has(e, "was not started")).await;
    stop_agent(r).await;
}

#[tokio::test]
async fn a_question_open_when_the_conversation_is_replaced_is_denied_silently_and_no_rule_is_kept() {
    for how in [Replace::Clear, Replace::Fork, Replace::Resume] {
        let d = project_with("saved", BYNAME);
        let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
        let (mut r, sid1) = recorded(&client, d.path()).await;
        if matches!(how, Replace::Resume) {
            // a second session, so that the first can be resumed
            replace(&mut r, Replace::Clear, &sid1).await;
        }
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: "q".into() });
        let req = r.ask().await;
        replace(&mut r, how, &sid1).await;
        // the answer of the person comes late and belongs to the old conversation: it keeps no rule
        r.send(AgentCommand::Permission {
            id: req.id.clone(),
            decision: PermissionDecision::AllowAlways { rule: req.suggested_rules[0].clone(), persist: false },
        });
        let evs = drain(&mut r).await;
        assert!(!evs.iter().any(|e| notice_has(e, "was not started")), "{how:?}: the new session was told: {evs:?}");
        assert!(flows_in(&evs).is_empty(), "{how:?}");
        // the slot is free and the same start is asked again
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: "q".into() });
        let evs = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. } | AgentEvent::FlowRun { .. })).await;
        assert!(
            matches!(evs.last(), Some(AgentEvent::PermissionAsk { .. })),
            "{how:?}: the rule of an old conversation allowed it: {:?}",
            evs.last()
        );
        assert!(client.tasks().is_empty(), "{how:?}");
        stop_agent(r).await;
    }
}
