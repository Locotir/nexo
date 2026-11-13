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

#[tokio::test]
async fn the_requests_of_a_run_nobody_writes_into_are_the_bytes_they_were_before_sub_agents_could_be_watched() {
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
    assert_eq!(out.content, "the report");
    let evs = drain(&mut rx);
    let want_items = vec![
        SubItem::Task { text: "look around".into() },
        SubItem::Text { text: "let me look".into() },
        SubItem::Call { id: "s1".into(), name: "read".into(), summary: "read".into() },
        SubItem::Result { id: "s1".into(), ok: true, bytes: "contents".len() },
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
    assert_eq!(items_of(&evs, "t1#1"), want_items);
    assert_eq!(subs.transcript("t1#1").unwrap().items, want_items);
    assert_eq!(subs.transcript("t1#1").unwrap().dropped, 0);
    let info = &subs.list()[0];
    assert_eq!((info.id.as_str(), info.profile.as_str(), info.title.as_str()), ("t1#1", "explore", "look around"));
    assert_eq!((info.state, info.tokens_in, info.tokens_out), (SubState::Ok, 250, 50));
    assert!(info.ended_ms.is_some_and(|e| e >= info.started_ms));
}
