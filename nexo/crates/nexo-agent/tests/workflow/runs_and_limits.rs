use super::*;

#[tokio::test]
async fn three_items_with_two_votes_start_six_sub_agents_each_with_its_item_and_the_count_line() {
    let d = test_tmpdir().unwrap();
    let checked = answering(CHECKED);
    let client = Mock::new(vec![
        ("FIND", says("Three.\n[\"alpha\", \"beta\", \"gamma\"]")),
        ("CHECK alpha", says("REAL: it is there")),
        ("CHECK beta", says("NOT REAL: it is fine")),
        ("CHECK gamma", says("Hmm, who knows")),
    ]);
    let (out, c) = go(&client, d.path(), &checked, "src").await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.tasks().len(), 7);
    for item in ["alpha", "beta", "gamma"] {
        assert_eq!(client.count(&format!("CHECK {item}")), 2, "{item}");
    }
    // the closing line for a list is added to the step that returns one, not to the others
    assert!(client.tasks()[0].contains("JSON array of strings"));
    assert!(!client.tasks().iter().any(|t| t.starts_with("CHECK") && t.contains("JSON array")));
    assert!(client.tasks()[0].contains("FIND in src"));
    // the count line only where every copy began with one of the answers
    assert!(out.content.contains("=> item 1: REAL 2 of 2"), "{}", out.content);
    assert!(out.content.contains("=> item 2: NOT REAL 2 of 2"), "{}", out.content);
    assert!(!out.content.contains("item 3: HMM") && !out.content.contains("=> item 3"), "{}", out.content);
    assert!(out.content.contains("--- check #1 (item 1: alpha) (copy 1) ---"), "{}", out.content);
    let titles: Vec<String> = c.jobs.subs().list().into_iter().map(|i| i.title).collect();
    assert!(titles.contains(&"inline · Check · check #6".to_string()), "{titles:?}");
    assert!(titles.contains(&"inline · Find · find".to_string()), "{titles:?}");
}

#[tokio::test]
async fn the_count_line_needs_every_copy_to_begin_with_one_of_the_answers() {
    let d = test_tmpdir().unwrap();
    let one = answering(CHECKED).replace("votes = 2", "votes = 3");
    // two answers among three copies: the count is said for both
    let client = Mock::new(vec![
        ("FIND", says("[\"a\"]")),
        ("CHECK", Say::Cycle(vec!["REAL yes".into(), "not real, sorry".into(), "Real again".into()])),
    ]);
    let (out, _) = go(&client, d.path(), &one, "").await;
    assert!(out.content.contains("=> item 1: REAL 2 of 3, NOT REAL 1 of 3"), "{}", out.content);
    // one copy gave none of the answers: just the reports
    let client = Mock::new(vec![
        ("FIND", says("[\"a\"]")),
        ("CHECK", Say::Cycle(vec!["REAL".into(), "FAKE".into(), "REAL".into()])),
    ]);
    let (out, _) = go(&client, d.path(), &one, "").await;
    assert!(!out.content.contains("=>"), "{}", out.content);
    assert!(out.content.contains("--- check #3 (item 1: a) (copy 3) ---"), "{}", out.content);
    // without `answers` a first word that is the same is no reason to count
    let client = Mock::new(vec![("FIND", says("[\"a\"]")), ("CHECK", says("REAL, I think"))]);
    let (out, _) = go(&client, d.path(), &CHECKED.replace("votes = 2", "votes = 3"), "").await;
    assert!(!out.content.contains("=>"), "{}", out.content);
}

#[tokio::test]
async fn a_report_with_no_array_fails_the_step_and_skips_what_needs_it_but_not_the_rest_of_the_phase() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("FIND", says("I found nothing to list")), ("SIDE", says("side report"))]);
    let toml = r#"
[[phase]]
title = "Find"
  [[phase.step]]
  id = "find"
  prompt = "FIND"
  returns = "list"
  [[phase.step]]
  id = "side"
  prompt = "SIDE"
[[phase]]
title = "Check"
  [[phase.step]]
  id = "check"
  each = "find"
  prompt = "CHECK {{item}}"
  [[phase.step]]
  id = "uses"
  prompt = "USES {{steps.find.report}}"
  [[phase.step]]
  id = "free"
  prompt = "FREE"
"#;
    let (out, _) = go(&client, d.path(), toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "find").contains("failed: the report has no JSON array of strings"), "{}", out.content);
    assert!(steps_line(&out, "side").contains(": ok"));
    assert!(steps_line(&out, "check").contains("skipped: step `find` failed"), "{}", out.content);
    assert!(steps_line(&out, "uses").contains("skipped: step `find` failed"));
    assert!(steps_line(&out, "free").contains(": ok"));
    assert_eq!(client.count("CHECK"), 0);
    assert_eq!(client.count("USES"), 0);
    assert_eq!(client.count("FREE"), 1);
}

#[tokio::test]
async fn a_phase_where_every_step_failed_ends_the_run() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[1, 2, 3]"))]);
    let toml = format!("{CHECKED}\n[[phase]]\ntitle = \"Never\"\n[[phase.step]]\nid = \"never\"\nprompt = \"NEVER\"\n");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    // the only usable thing would be a report: there is none
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("Ended early: every step of phase \"Find\" failed"), "{}", out.content);
    assert!(steps_line(&out, "find").contains("not one of strings"));
    assert!(steps_line(&out, "never").contains("not run"));
    assert_eq!(client.tasks().len(), 1);
}

#[tokio::test]
async fn text_that_arrives_through_a_placeholder_is_never_expanded_again() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[\"{{args}} and {{steps.find.report}}\"]")), ("CHECK", says("fine"))]);
    let (out, _) = go(&client, d.path(), CHECKED, "THE-ARGS").await;
    assert!(!out.is_error, "{}", out.content);
    let check = client.tasks().into_iter().find(|t| t.starts_with("CHECK")).unwrap();
    assert!(check.contains("{{args}} and {{steps.find.report}}"), "{check}");
    assert!(!check.contains("THE-ARGS"), "{check}");
}

#[tokio::test]
async fn max_agents_stops_new_starts_and_is_named() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[\"a\", \"b\", \"c\", \"d\", \"e\"]"))]);
    let toml = format!("max_agents = 3\nmax_parallel = 1\n{CHECKED}\n[[phase]]\ntitle = \"After\"\n[[phase.step]]\nid = \"after\"\nprompt = \"AFTER\"\n");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert_eq!(client.tasks().len(), 3, "{}", out.content);
    assert!(out.content.contains("Stopped: max_agents (3) was reached"), "{}", out.content);
    assert!(steps_line(&out, "check").contains("partial, 8 without a report"), "{}", out.content);
    assert!(steps_line(&out, "after").contains("not run: max_agents (3) was reached"), "{}", out.content);
    assert_eq!(client.count("AFTER"), 0);
}

#[tokio::test]
async fn max_tokens_stops_new_starts_and_is_named() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![
        ("FIND", Say::Text("[\"a\", \"b\", \"c\"]".into(), 500, 100)),
        ("CHECK", Say::Text("fine".into(), 500, 100)),
    ]);
    let toml = format!("max_tokens = 1000\nmax_parallel = 1\n{CHECKED}").replace("votes = 2", "votes = 1");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    // 600 spent by `find`, 1200 after the first check: the second check does not start
    assert_eq!(client.tasks().len(), 2, "{}", out.content);
    assert!(out.content.contains("Stopped: max_tokens (1000) was reached"), "{}", out.content);
    assert!(out.content.contains("1200 tokens"), "{}", out.content);
    assert!(steps_line(&out, "check").contains("partial, 2 without a report"), "{}", out.content);
}

#[tokio::test]
async fn the_interrupt_stops_the_whole_run_and_nothing_starts_after_it() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let toml = r#"
max_parallel = 2
[[phase]]
title = "One"
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
    let wf = tool(&client, d.path());
    let stopper = async {
        client.until_started(2).await;
        let at_cancel = client.tasks().len();
        c.cancel.cancel();
        at_cancel
    };
    let (out, at_cancel) = tokio::join!(wf.run(json!({"inline": toml}), &c), stopper);
    assert_eq!(at_cancel, 2);
    assert_eq!(client.tasks().len(), 2, "a call was made after the interrupt");
    assert!(out.content.contains("Interrupted by the person"), "{}", out.content);
    assert!(steps_line(&out, "c").contains("not run"), "{}", out.content);
    assert!(steps_line(&out, "z").contains("not run"), "{}", out.content);
    let list = c.jobs.subs().list();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|i| i.state == SubState::Killed), "{list:?}");
    assert_eq!(c.jobs.running(), 0);
}

#[tokio::test]
async fn a_file_that_fails_a_check_or_names_an_unknown_kind_starts_nothing() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![]);
    let bad = "[[phase]]\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\nvotes = 9\nagent = \"nobody\"\n";
    let (out, _) = go(&client, d.path(), bad, "").await;
    assert!(
        out.is_error && out.content.contains("nothing was started") && out.content.contains("line"),
        "{}",
        out.content
    );
    assert!(out.content.contains("`votes` is 9"), "{}", out.content);
    let kind = "[[phase]]\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\nagent = \"nobody\"\n";
    let (out, _) = go(&client, d.path(), kind, "").await;
    assert!(out.is_error && out.content.contains("no sub-agent kind `nobody`"), "{}", out.content);
    assert!(out.content.contains("explore"), "the kinds there are are listed: {}", out.content);
    let wf = tool(&client, d.path());
    let c = ctx(d.path());
    for args in [json!({}), json!({"name": "ghost"}), json!({"name": "a", "inline": "b"})] {
        assert!(wf.run(args, &c).await.is_error);
    }
    assert!(client.tasks().is_empty());
}

#[tokio::test]
async fn the_answer_is_bounded_per_report_and_every_report_keeps_its_share_of_the_total() {
    let d = test_tmpdir().unwrap();
    let big = "x".repeat(7000);
    let client = Mock::new(vec![("BIG", says(&big))]);
    // one report: 6000 characters, and the line says how much was cut
    let one = "[[phase]]\ntitle = \"Only\"\n[[phase.step]]\nid = \"s1\"\nprompt = \"BIG 1\"\n";
    let (out, _) = go(&client, d.path(), one, "").await;
    assert_eq!(out.content.matches('x').count(), 6000);
    assert!(out.content.contains("[cut: 1000 more characters]"), "{}", out.content);
    // five of them: each has a fifth of 24000, so none comes first and none vanishes
    let steps: String = (1..=5).map(|n| format!("[[phase.step]]\nid = \"s{n}\"\nprompt = \"BIG {n}\"\n")).collect();
    let toml = format!("[[phase]]\ntitle = \"Only\"\n{steps}");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert_eq!(
        out.content.matches('x').count(),
        24000,
        "{}",
        out.content.lines().filter(|l| !l.starts_with("xxx")).collect::<Vec<_>>().join("|")
    );
    assert_eq!(out.content.matches("[cut: 2200 more characters]").count(), 5);
    for n in 1..=5 {
        assert!(out.content.contains(&format!("== s{n} ==")), "s{n} vanished");
    }
    assert!(
        !out.content.contains("left out"),
        "{}",
        out.content.lines().filter(|l| !l.starts_with("xxx")).collect::<Vec<_>>().join("|")
    );
}

#[tokio::test]
async fn in_the_answer_every_vote_keeps_its_header_and_its_count_line_even_with_no_text_left() {
    let d = test_tmpdir().unwrap();
    // 5 items with 5 votes: 25 reports share 24000, 960 characters each
    let items = (1..=5).map(|n| format!("\"it{n}\"")).collect::<Vec<_>>().join(", ");
    let client =
        Mock::new(vec![("FIND", says(&format!("[{items}]"))), ("CHECK", says(&format!("REAL {}", "x".repeat(2000))))]);
    let toml = answering(CHECKED).replace("votes = 2", "votes = 5");
    let toml = toml.replace("[[phase]]\ntitle = \"Find\"", "max_agents = 50\n[[phase]]\ntitle = \"Find\"");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert_eq!(out.content.matches("--- check #").count(), 25, "{}", out.content);
    assert_eq!(out.content.matches("=> item ").count(), 5);
    assert!(out.content.contains("=> item 5: REAL 5 of 5"), "{}", out.content);
    assert!(out.content.contains("--- check #25 (item 5: it5) (copy 5) ---"), "{}", out.content);
    assert!(out.content.matches('x').count() <= 24000);
}

#[tokio::test]
async fn a_workflow_cannot_start_a_workflow_and_a_sub_agent_is_not_offered_the_tool() {
    let d = test_tmpdir().unwrap();
    let client = Mock::new(vec![]);
    let set = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("workflow")));
    let wf = Workflow::new(Arc::new(sub_agent(&client, set)), d.path().to_path_buf());
    let out = wf
        .run(json!({"inline": "[[phase]]\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"\nagent=\"general\""}), &ctx(d.path()))
        .await;
    assert!(!out.is_error, "{}", out.content);
    let seen = client.st.tools_seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains(&"read".to_string()), "{seen:?}");
    assert!(!seen[0].contains(&"workflow".to_string()) && !seen[0].contains(&"task".to_string()), "{seen:?}");
}

#[tokio::test]
async fn the_description_lists_the_saved_workflows_bounded_and_the_question_says_what_it_costs() {
    data_dir();
    let d = test_tmpdir().unwrap();
    nexo_core::config::trust_project(d.path(), &nexo_core::paths::trusted_projects_file()).unwrap();
    let dir = d.path().join(".nexo/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    let body = |desc: &str| {
        format!("description = \"{desc}\"\n[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\n")
    };
    for n in 0..25 {
        std::fs::write(dir.join(format!("w{n:02}.toml")), body(&"long ".repeat(40))).unwrap();
    }
    std::fs::write(dir.join("w00.toml"), body("the first one")).unwrap();
    let client = Mock::new(vec![]);
    let wf = tool(&client, d.path());
    let spec = wf.spec();
    assert!(spec.description.contains("- w00: the first one"), "{}", spec.description);
    assert!(spec.description.contains("- w19:"), "{}", spec.description);
    assert!(!spec.description.contains("- w20"), "{}", spec.description);
    assert!(spec.description.contains("and 5 more"));
    let line = spec.description.lines().find(|l| l.starts_with("- w01")).unwrap();
    assert!(line.chars().count() <= 80 + 6, "{line}");
    assert_eq!(wf.kind(), ToolKind::Other);
    // the question
    let summary = wf.summary(&json!({"name": "w00"}));
    assert_eq!(summary, "workflow `w00`: up to 1 sub-agents (at least 1), 400000 tokens at most");
    let (s, detail) = nexo_agent::workflow::tool::question(&json!({"inline": CHECKED}), d.path());
    assert_eq!(s, "inline workflow: up to 12 sub-agents (at least 1), 400000 tokens at most");
    assert_eq!(detail, vec!["Find: find".to_string(), "Check: check (each of find, 2 votes)".to_string()]);
    // more than 12 phases: 12 lines, then how many more
    let phases: String = std::iter::once("max_agents = 20\n".to_string())
        .chain(
            (1..=15).map(|n| format!("[[phase]]\ntitle = \"P{n}\"\n[[phase.step]]\nid = \"s{n}\"\nprompt = \"p\"\n")),
        )
        .collect();
    let (_, detail) = nexo_agent::workflow::tool::question(&json!({"inline": phases}), d.path());
    assert_eq!(detail.len(), 13, "{detail:?}");
    assert_eq!(detail[11], "P12: s12");
    assert_eq!(detail[12], "+3 more");
}

#[tokio::test]
async fn leave_is_asked_once_a_denial_starts_nothing_and_always_allow_holds_for_one_file_as_it_is() {
    let saved = "description = \"two reads\"\n[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"SAVED a\"\n[[phase.step]]\nid = \"b\"\nprompt = \"SAVED b\"\n";
    let d = project_with("saved", saved);
    let inline = "[[phase]]\ntitle = \"I\"\n[[phase.step]]\nid = \"i\"\nprompt = \"INLINE i\"\n";
    let client = Mock::with_main(
        vec![],
        vec![
            call("w1", "workflow", json!({"name": "saved"})),
            text_events("understood", 1, 1),
            call("w2", "workflow", json!({"name": "saved"})),
            text_events("ran", 1, 1),
            call("w3", "workflow", json!({"name": "saved"})),
            text_events("ran again", 1, 1),
            call("w5", "workflow", json!({"name": "saved"})),
            text_events("changed", 1, 1),
            call("w4", "workflow", json!({"inline": inline})),
            text_events("denied", 1, 1),
        ],
    );
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;

    // 1: asked, denied: nothing starts
    r.submit("run it");
    let req = r.ask().await;
    assert_eq!((req.tool.as_str(), req.kind), ("workflow", ToolKind::Other));
    assert_eq!(req.summary, "workflow `saved`: up to 2 sub-agents (at least 2), 400000 tokens at most");
    // the rule that is offered holds the name and the start of the hash of the file
    let rule = format!("workflow({})", subject("saved", saved));
    assert_eq!(req.suggested_rules, vec![rule.clone()]);
    assert!(rule.len() == "workflow(saved@".len() + 32 + 1, "the pin is 32 hex: {rule}");
    r.send(AgentCommand::Permission {
        id: "w1".into(),
        decision: PermissionDecision::Deny { reason: Some("no".into()) },
    });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { .. })), "a denied tool never starts");
    assert!(client.tasks().is_empty(), "{:?}", client.tasks());

    // 2: allowed always with the rule it offered: one question, two sub-agents
    r.submit("again");
    r.ask().await;
    r.send(AgentCommand::Permission {
        id: "w2".into(),
        decision: PermissionDecision::AllowAlways { rule: rule.clone(), persist: false },
    });
    let evs = r.turn().await;
    assert_eq!(evs.iter().filter(|e| matches!(e, AgentEvent::PermissionAsk { .. })).count(), 0);
    assert_eq!(client.tasks().len(), 2);

    // 3: the rule covers it: no question at all
    r.submit("and again");
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "{evs:?}");
    assert_eq!(client.tasks().len(), 4);

    // 3b: the file changed: the rule was for the other one, so it is asked again, with a rule of its own
    let changed = saved.replace("SAVED b", "SAVED b, and now more");
    std::fs::write(d.path().join(".nexo/workflows/saved.toml"), &changed).unwrap();
    r.submit("after the change");
    let req = r.ask().await;
    assert_eq!(req.suggested_rules, vec![format!("workflow({})", subject("saved", &changed))]);
    assert_ne!(req.suggested_rules[0], rule);
    r.send(AgentCommand::Permission { id: "w5".into(), decision: PermissionDecision::Deny { reason: None } });
    r.turn().await;
    assert_eq!(client.tasks().len(), 4, "a changed file started on the old leave");

    // 4: text the model wrote is asked every time, and says its phases and steps
    r.submit("write one");
    let req = r.ask().await;
    assert!(req.summary.starts_with("inline workflow: up to 1 sub-agents"), "{}", req.summary);
    assert!(req.suggested_rules.is_empty(), "{:?}", req.suggested_rules);
    assert!(
        matches!(&req.detail, nexo_core::tool::ToolDisplay::List { items } if items == &vec!["I: i".to_string()]),
        "{:?}",
        req.detail
    );
    r.send(AgentCommand::Permission { id: "w4".into(), decision: PermissionDecision::Deny { reason: None } });
    r.turn().await;
    assert_eq!(client.tasks().len(), 4);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}
