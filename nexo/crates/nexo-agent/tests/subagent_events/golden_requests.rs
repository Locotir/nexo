use super::*;

#[test]
fn a_windows_temp_path_is_blanked_in_every_form_it_takes_in_a_request() {
    let named = [r"C:\Users\x\Temp\.tmpA".to_string(), r"\\?\C:\Users\x\Temp\.tmpA".to_string()];
    let forms = path_forms(&named);
    let mut v = json!({
        "a": r"cwd is C:\Users\x\Temp\.tmpA\sub and more",
        "b": [r"\\?\C:\Users\x\Temp\.tmpA", "C:/Users/x/Temp/.tmpA/file"],
        "c": {"d": r"C:\Users\x\Temp\.tmpB stays"},
    });
    blank_paths(&mut v, &forms);
    assert_eq!(
        v,
        json!({
            "a": r"cwd is <cwd>\sub and more",
            "b": ["<cwd>", "<cwd>/file"],
            "c": {"d": r"C:\Users\x\Temp\.tmpB stays"},
        })
    );
    // the JSON text of the same value holds the path with doubled backslashes: the text is not where to look
    let text = json!({"a": r"C:\Users\x\Temp\.tmpA"}).to_string();
    assert!(!text.contains(r"C:\Users\x\Temp\.tmpA"));
}

// The historical transcript stays byte-for-byte unchanged. The fixture now also pins the hard
// output ceiling: adapters must not enlarge a worker's reserved reply behind the window guard.
#[tokio::test]
async fn the_requests_of_a_run_nobody_writes_into_keep_the_transcript_and_a_hard_output_ceiling() {
    let (main, subs) = the_scripted_run().await;
    assert!(main.as_array().unwrap().len() >= 4 && subs.as_array().unwrap().len() >= 4);
    matches_golden(&main, "main_requests.json");
    matches_golden(&subs, "sub_requests.json");
}

#[tokio::test]
async fn a_sub_agent_that_writes_calls_a_tool_and_reports_leaves_its_conversation_in_order() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![],
        vec![
            Reply::Events(says_then_calls(&["let me ", "look"], "s1", "read", spends(100, 20))),
            Reply::Events(text_spending("the report", spends(150, 30))),
        ],
    );
    let (agent, ctx) = (task_tool(&client), call_ctx(d.path(), "t1"));
    let subs = ctx.jobs.subs();
    let mut rx = subs.changes();
    let out = agent.run(task("explore", "look around"), &ctx).await;
    assert_eq!(out.content, "the report\n\n[tool log] 1 tool call");
    let evs = drain(&mut rx);
    let want_items = vec![
        SubItem::Task { text: "look around".into() },
        SubItem::Text { text: "let me look".into() },
        SubItem::Call { id: "s1".into(), name: "read".into(), summary: "read".into(), args: "{}".into() },
        SubItem::Result { id: "s1".into(), ok: true, bytes: "contents".len(), output: "contents".into() },
        SubItem::Text { text: "the report".into() },
    ];
    // the events, in the order they were made: the state, the items, a delta for each piece of text
    let shape: Vec<String> = evs
        .iter()
        .map(|c| match c {
            SubChange::State(i) => format!("state {:?}", i.state),
            SubChange::Item { item, .. } => {
                format!("item {}", serde_json::to_value(item).unwrap()["item"].as_str().unwrap())
            }
            SubChange::Delta { text, .. } => format!("delta {text}"),
            SubChange::NotDelivered { .. } => "not delivered".into(),
            SubChange::Flow(_) => "flow".into(),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "state Attached",
            "item task",
            "state Attached", // The resolved provider/model is published to live readers.
            "delta let me ",
            "delta look",
            "item text",
            "item call",
            "item result",
            "delta the report",
            "item text",
            "state Ok"
        ]
    );
    let SubChange::State(resolved) = &evs[2] else { panic!("missing resolved target") };
    assert!(resolved.provider.is_some() && resolved.model.is_some());
    assert_eq!(items_of(&evs, "t1#1"), want_items);
    assert_eq!(subs.transcript("t1#1").unwrap().items, want_items);
    assert_eq!(subs.transcript("t1#1").unwrap().dropped, 0);
    let info = &subs.list()[0];
    assert_eq!((info.id.as_str(), info.profile.as_str(), info.title.as_str()), ("t1#1", "explore", "look around"));
    assert_eq!((info.state, info.tokens_in, info.tokens_out), (SubState::Ok, 250, 50));
    assert!(info.ended_ms.is_some_and(|e| e >= info.started_ms));
}

/// A model can emit a tool that was not offered: the runtime, not the persona,
/// must enforce the planner's permissions and still let it finish its report.
#[tokio::test]
async fn plan_runtime_refuses_writes_and_project_escape_but_can_read_and_report() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![],
        vec![
            Reply::Events(call("w1", "write", json!({"path":"src/lib.rs", "hold":true}))),
            Reply::Events(call("r1", "read", json!({"path":"../secret"}))),
            Reply::Events(call("r2", "read", json!({"path":"src/lib.rs"}))),
            Reply::Events(text("Plan: inspect src/lib.rs, then add a regression test.")),
        ],
    );
    let (agent, ctx) = (task_tool(&client), call_ctx(d.path(), "plan1"));
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        agent.run(task("plan", "Plan the smallest fix; do not implement."), &ctx),
    )
    .await
    .expect("a forbidden write must not enter the waiting tool");
    assert!(!out.is_error, "{out:?}");
    assert!(out.content.starts_with("Plan: inspect src/lib.rs"), "{out:?}");
    assert_eq!(client.probe.tools.load(Ordering::SeqCst), 0);
    let requests = client.sub_requests();
    assert_eq!(requests.len(), 4);
    for req in &requests {
        assert_eq!(req.model, "m1");
        assert!(req.tools.iter().any(|t| t.name == "read"));
        assert!(!req.tools.iter().any(|t| t.name == "write"));
        assert!(req.system.contains("smallest viable implementation plan"));
    }
    let result = |req: &ModelRequest, id: &str| {
        req.messages
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|part| match part {
                nexo_core::message::Part::ToolResult { call_id, content, is_error, .. } if call_id == id => {
                    Some((*is_error, nexo_core::message::result_text(content)))
                }
                _ => None,
            })
            .expect("tool result in the next request")
    };
    let (is_error, after_write) = result(&requests[1], "w1");
    assert!(is_error && after_write.contains("has no tool `write`"), "{after_write}");
    let (is_error, after_escape) = result(&requests[2], "r1");
    assert!(is_error && after_escape.contains("outside the project"), "{after_escape}");
    assert_eq!(result(&requests[3], "r2"), (false, "contents".into()));
    let info = ctx.jobs.subs().list().pop().unwrap();
    assert_eq!((info.profile.as_str(), info.state), ("plan", SubState::Ok));
    assert_eq!((info.provider.as_deref(), info.model.as_deref()), (Some("mock"), Some("m1")));
}

#[tokio::test]
async fn resolved_effort_is_published_and_matches_every_subagent_request() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![],
        vec![
            Reply::Events(says_then_calls(&["looking"], "s1", "read", spends(10, 2))),
            Reply::Events(text_spending("done", spends(10, 2))),
        ],
    );
    let mut cfg = ProviderConfig::new("mock", "http://mock", None);
    cfg.models = vec!["m1".into()];
    cfg.effort.insert("m1".into(), "high".into());
    cfg.efforts.insert("m1".into(), vec!["low".into(), "medium".into()]);
    let agent = SubAgent::new(
        client.clone(),
        Target::new("mock", cfg, "m1"),
        &tools(&client.probe),
        vec![],
        Arc::new(OnceLock::new()),
    );
    let ctx = call_ctx(d.path(), "t1");
    let subs = ctx.jobs.subs();
    let mut rx = subs.changes();
    agent.run(task("explore", "look around"), &ctx).await;
    let info = subs.list().pop().unwrap();
    assert_eq!(
        (info.provider.as_deref(), info.model.as_deref(), info.reasoning_effort.as_deref()),
        (Some("mock"), Some("m1"), Some("medium"))
    );
    assert!(drain(&mut rx)
        .iter()
        .any(|e| matches!(e, SubChange::State(i) if i.reasoning_effort.as_deref() == Some("medium"))));
    let requests = client.sub_requests();
    assert_eq!(requests.len(), 2);
    for req in requests {
        assert_eq!(serde_json::to_value(req.reasoning).unwrap(), serde_json::json!({"effort": "medium"}));
    }
}

#[tokio::test]
async fn explicit_effort_overrides_default_and_is_clamped_in_every_request() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![],
        vec![
            Reply::Events(says_then_calls(&["looking"], "s1", "read", spends(10, 2))),
            Reply::Events(text_spending("done", spends(10, 2))),
        ],
    );
    let mut cfg = ProviderConfig::new("mock", "http://mock", None);
    cfg.models = vec!["m1".into()];
    cfg.effort.insert("m1".into(), "low".into());
    cfg.efforts.insert("m1".into(), vec!["low".into(), "medium".into()]);
    let agent = SubAgent::new(
        client.clone(),
        Target::new("mock", cfg, "m1"),
        &tools(&client.probe),
        vec![],
        Arc::new(OnceLock::new()),
    );
    let ctx = call_ctx(d.path(), "t1");
    let subs = ctx.jobs.subs();
    let mut rx = subs.changes();
    let mut args = task("explore", "look around");
    args["effort"] = serde_json::json!("max");
    let out = agent.run(args, &ctx).await;
    assert!(!out.is_error, "{out:?}");
    let info = subs.list().pop().unwrap();
    assert_eq!(
        (info.provider.as_deref(), info.model.as_deref(), info.reasoning_effort.as_deref()),
        (Some("mock"), Some("m1"), Some("medium"))
    );
    assert!(drain(&mut rx)
        .iter()
        .any(|e| matches!(e, SubChange::State(i) if i.reasoning_effort.as_deref() == Some("medium"))));
    let requests = client.sub_requests();
    assert_eq!(requests.len(), 2);
    for req in requests {
        assert_eq!(serde_json::to_value(req.reasoning).unwrap(), serde_json::json!({"effort": "medium"}));
    }
}
