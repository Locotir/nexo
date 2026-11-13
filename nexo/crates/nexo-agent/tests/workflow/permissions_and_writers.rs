use super::*;

#[tokio::test]
async fn sub_agents_that_can_change_files_run_one_at_a_time_as_with_the_task_tool() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("GATE", Say::Gated("done".into()))]);
    let wf = writer_tool(&client, d.path(), false);
    let c = ctx(d.path());
    let driver = async {
        for n in 1..=4 {
            client.until_started(n).await;
            grace().await;
            assert_eq!(client.st.started.load(Ordering::SeqCst), n, "a writer began while another was at work");
            client.st.gate.add_permits(1);
            client.until_ended(n).await;
        }
    };
    let (out, ()) = tokio::join!(wf.run(json!({"inline": four("general")}), &c), driver);
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.st.peak.load(Ordering::SeqCst), 1, "{}", out.content);
    assert_eq!(client.tasks().len(), 4);
}

#[tokio::test]
async fn read_only_kinds_run_together_and_so_do_writers_when_the_setting_lets_task_sub_agents_write_side_by_side() {
    for (agent, parallel) in [("explore", false), ("general", true)] {
        let d = test_tmpdir().unwrap();
        let client = Mock::new(vec![("GATE", Say::Gated("done".into()))]);
        let wf = writer_tool(&client, d.path(), parallel);
        let c = ctx(d.path());
        let driver = async {
            // all four at work at once, with the gate shut
            client.until_started(4).await;
            assert_eq!(client.st.active.load(Ordering::SeqCst), 4, "{agent} {parallel}");
            client.st.gate.add_permits(4);
            client.until_ended(4).await;
        };
        let (out, ()) = tokio::join!(wf.run(json!({"inline": four(agent)}), &c), driver);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(client.st.peak.load(Ordering::SeqCst), 4, "{agent} {parallel}");
    }
}

#[tokio::test]
async fn an_error_a_cut_and_a_refused_start_each_fail_their_own_step_and_skip_what_needs_it() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![
        ("BOOM", Say::Fail),
        ("CUT", Say::Hang),
        ("FINE", says("all good")),
        ("FREE", says("free report")),
        ("NEEDSFINE", says("used the good one")),
    ]);
    // a kind that works in a worktree of its own, in a folder that is no repository: it cannot be started
    let mut profiles = nexo_agent::subagent::builtin();
    profiles.push(nexo_agent::subagent::Profile {
        name: "iso".into(),
        description: "works in a worktree".into(),
        tools: None,
        read_only: false,
        model: None,
        prompt: "Work.".into(),
        rounds: 4,
        source: None,
        soft_model: false,
        isolation: true,
    });
    let set = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("edit")));
    let wf = Workflow::new(Arc::new(sub_agent_with(&client, set, profiles)), d.path().to_path_buf());
    let toml = r#"
[[phase]]
title = "Work"
  [[phase.step]]
  id = "boom"
  prompt = "BOOM"
  [[phase.step]]
  id = "cutoff"
  prompt = "CUT"
  [[phase.step]]
  id = "refused"
  agent = "iso"
  prompt = "ISO"
  [[phase.step]]
  id = "fine"
  prompt = "FINE"
[[phase]]
title = "After"
  [[phase.step]]
  id = "needs_boom"
  prompt = "NEEDS {{steps.boom.report}}"
  [[phase.step]]
  id = "needs_cut"
  prompt = "NEEDS {{steps.cutoff.report}}"
  [[phase.step]]
  id = "needs_refused"
  prompt = "NEEDS {{steps.refused.report}}"
  [[phase.step]]
  id = "needs_fine"
  prompt = "NEEDSFINE {{steps.fine.report}}"
  [[phase.step]]
  id = "free"
  prompt = "FREE"
"#;
    let c = ctx(d.path());
    let cutter = async {
        client.until_started(2).await;
        // the person stops that one sub-agent from the job list: the run goes on
        let job = loop {
            if let Some(j) = c.jobs.list().into_iter().find(|j| j.label.contains("cutoff")) {
                break j;
            }
            tokio::task::yield_now().await;
        };
        assert!(c.jobs.kill(job.id));
    };
    let (out, ()) = tokio::join!(wf.run(json!({"inline": toml}), &c), cutter);
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "boom").contains("failed: the sub-agent's model call failed"), "{}", out.content);
    assert!(steps_line(&out, "cutoff").contains("failed: the sub-agent was stopped"), "{}", out.content);
    assert!(steps_line(&out, "refused").contains("failed: could not make the sub-agent a worktree"), "{}", out.content);
    assert!(steps_line(&out, "fine").contains(": ok"), "{}", out.content);
    for (id, by) in [("needs_boom", "boom"), ("needs_cut", "cutoff"), ("needs_refused", "refused")] {
        assert!(steps_line(&out, id).contains(&format!("skipped: step `{by}` failed")), "{}", out.content);
    }
    assert!(steps_line(&out, "needs_fine").contains(": ok") && steps_line(&out, "free").contains(": ok"));
    assert_eq!(client.count("NEEDS "), 0);
    assert_eq!(client.count("NEEDSFINE"), 1);
    assert_eq!(client.count("FREE"), 1);
    assert_eq!(client.count("ISO"), 0, "the refused one never ran");
}

#[tokio::test]
async fn a_tool_a_sub_agent_is_refused_goes_back_to_it_and_does_not_by_itself_fail_its_step() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("DENY", Say::CallThen("edit", "the edit was refused, nothing changed".into()))]);
    let wf = writer_tool(&client, d.path(), false);
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"deny\"\nagent = \"general\"\nprompt = \"DENY\"\n";
    let out = wf.run(json!({"inline": toml}), &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "deny").contains(": ok"), "{}", out.content);
    assert!(out.content.contains("the edit was refused, nothing changed"), "{}", out.content);
    let log = client.st.log.lock().unwrap().clone();
    let result = log.iter().find(|(_, t)| t.starts_with("RESULT")).expect("the sub-agent got a result back");
    assert!(result.1.contains("Permission denied"), "{}", result.1);
}

#[tokio::test]
async fn read_only_and_plan_mode_ask_about_a_workflow_as_ever_and_the_mode_refuses_what_a_sub_agent_changes() {
    for mode in [PermissionMode::ReadOnly, PermissionMode::Plan] {
        let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"look\"\nprompt = \"LOOK\"\n[[phase.step]]\nid = \"change\"\nagent = \"general\"\nprompt = \"CHANGE\"\n";
        let d = project_with("saved", text);
        let client = Mock::with_main(
            vec![("LOOK", says("looked")), ("CHANGE", Say::CallThen("edit", "the mode said no".into()))],
            vec![call("w1", "workflow", json!({"name": "saved"})), text_events("done", 1, 1)],
        );
        let tools = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("edit")));
        let mut r = start(&client, d.path(), mode, tools);
        r.next().await;
        r.submit("run it");
        // asked, not denied by the mode (a workflow is judged call by call inside)
        let req = r.ask().await;
        assert_eq!(req.tool, "workflow", "{mode:?}");
        r.send(AgentCommand::Permission { id: "w1".into(), decision: PermissionDecision::Allow });
        let evs = r.turn().await;
        assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "{mode:?}: asked twice");
        // the read ran; the sub-agent that tried to change something was refused by the mode
        assert_eq!(client.count("LOOK"), 1, "{mode:?}");
        assert_eq!(client.count("CHANGE"), 1, "{mode:?}");
        let log = client.st.log.lock().unwrap().clone();
        let result = log.iter().find(|(_, t)| t.starts_with("RESULT")).expect("a result came back");
        assert!(result.1.contains("Permission denied") && result.1.contains(mode.as_str()), "{mode:?}: {}", result.1);
        r.send(AgentCommand::Shutdown);
        r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
        r.task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn a_run_that_reports_no_usage_counts_as_zero_tokens_and_the_result_says_so() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("BARE", Say::Bare("no meter".into())), ("METERED", says("metered"))]);
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"bare\"\nprompt = \"BARE\"\n[[phase.step]]\nid = \"metered\"\nprompt = \"METERED\"\n";
    let (out, _) = go(&client, d.path(), toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "bare").contains("1 sub-agent, 0 tokens"), "{}", out.content);
    assert!(steps_line(&out, "metered").contains("1 sub-agent, 15 tokens"), "{}", out.content);
    assert!(
        out.content.contains("note: token use was not reported for 1 sub-agent; it is counted as 0 tokens"),
        "{}",
        out.content
    );
    assert_eq!(out.content.matches("token use was not reported").count(), 1, "only the one that did not report");
}
