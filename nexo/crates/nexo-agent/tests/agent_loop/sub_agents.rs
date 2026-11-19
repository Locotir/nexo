use super::*;

#[tokio::test]
async fn a_task_runs_a_read_only_sub_agent_whose_report_is_the_result() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        // the main model hands a question over
        Reply::Events(call(
            "t1",
            "task",
            json!({"agent": "explore", "description": "find the parser", "prompt": "where is parsing done?"}),
        )),
        // the sub-agent reads one file, reads the notes beside the project, tries to write, then reports
        Reply::Events(vec![
            StreamEvent::ToolCallStart { id: "s1".into(), name: "read".into() },
            StreamEvent::ToolCallEnd { id: "s1".into(), name: "read".into(), args: json!({"path": "src/parse.rs"}) },
            StreamEvent::ToolCallStart { id: "s2".into(), name: "read".into() },
            StreamEvent::ToolCallEnd { id: "s2".into(), name: "read".into(), args: json!({"path": "../notes.md"}) },
            StreamEvent::ToolCallStart { id: "s3".into(), name: "write".into() },
            StreamEvent::ToolCallEnd { id: "s3".into(), name: "write".into(), args: json!({"path": "x"}) },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]),
        Reply::Events(text("parsing is in src/parse.rs:12")),
        Reply::Events(text("it is in src/parse.rs")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "where do we parse?".into(), attachments: vec![] });
    let evs = r.turn().await;
    let done = evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.clone()),
        _ => None,
    });
    let said = done.map(|o| o.content).unwrap();
    // the report, then what its tool log shows: three calls, no file written, none run
    assert_eq!(said, "parsing is in src/parse.rs:12\n\n[tool log] 3 tool calls");
    let reqs = client.requests();
    // the sub-agent's own call has only the read-only tools and its own system prompt
    assert!(reqs[1].system.starts_with("You are a sub-agent"));
    let names: Vec<&str> = reqs[1].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        names.contains(&"read") && names.contains(&"grep") && !names.contains(&"write") && !names.contains(&"task"),
        "{names:?}"
    );
    let back = reqs[2].messages.last().unwrap();
    let said: Vec<String> = back
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => {
                content.first().map(|c| if let Part::Text { text } = c { text.clone() } else { String::new() })
            }
            _ => None,
        })
        .collect();
    assert!(said[1] == "read:../notes.md" && said[2].contains("only read"), "{said:?}");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolProgress { line, .. } if line.contains("read"))));
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_starts_with_a_briefing_of_what_the_session_read() {
    let d = test_tmpdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    let body: String = (1..=40)
        .map(|i| {
            if i == 21 {
                "fn parse_line(s: &str) -> Token { todo!() }\n".to_string()
            } else {
                format!("// filler {i}\n")
            }
        })
        .collect();
    std::fs::write(d.path().join("src/parser.rs"), body).unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("r1", "read", json!({"path": "src/parser.rs"}))),
        Reply::Events(call(
            "t1",
            "task",
            json!({"description": "fix the parser", "prompt": "make parse_line in src/parser.rs return a Token"}),
        )),
        Reply::Events(text("parse_line is at src/parser.rs:21")),
        Reply::Events(text("done")),
    ]);
    let cfg = config(PermissionMode::Ask);
    let set = tools().with(Arc::new(nexo_tools::read::Read));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look at the parser".into(), attachments: vec![] });
    let evs = r.turn().await;
    let reqs = client.requests();
    // the sub-agent's first request: the briefing, then the task
    let first = &reqs[2].messages[0].content;
    let texts: Vec<&str> =
        first.iter().filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None }).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts[0].starts_with("<briefing") && texts[0].contains("fn parse_line"), "{}", texts[0]);
    assert!(texts[0].contains("src/parser.rs") && !texts[0].contains("filler 1\n"), "{}", texts[0]);
    assert!(texts[1].contains("make parse_line"));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolProgress { line, .. } if line.starts_with("briefed:"))));
    r.stop().await;
}

#[tokio::test]
async fn a_general_sub_agent_asks_for_its_writes_and_its_spending_is_counted() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(text("wrote x.txt")),
        Reply::Events(text("all done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    let req = match evs.last().unwrap() {
        AgentEvent::PermissionAsk { request } => request.clone(),
        _ => unreachable!(),
    };
    assert_eq!((req.id.as_str(), req.tool.as_str()), ("t1#1.s1", "write"));
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    let evs = r.turn().await;
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "t1" && output.content.starts_with("wrote x.txt\n\n[tool log] 1 tool call:\n- wrote x.txt"))
    ));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolProgress { id, line } if id == "t1" && line == "→ write x.txt")));
    assert_eq!(text_of(&evs), "all done");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    assert!(reqs[1].system.contains("sub-agent") && reqs[1].tools.iter().any(|t| t.name == "write"));
    assert!(!reqs[1].tools.iter().any(|t| t.name == "task"), "a sub-agent does not start its own");
    // both of the sub-agent's calls are on the bill: 10+1000 of theirs, 10+1000 of the main agent's
    let session =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None });
    assert_eq!(session.unwrap().input, 10 + 1000 + 10 + 1000);
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_out_of_rounds_still_reports_what_it_found() {
    let d = test_tmpdir().unwrap();
    let mut replies = vec![Reply::Events(call(
        "t1",
        "task",
        json!({"agent": "explore", "description": "map it", "prompt": "where is x kept?"}),
    ))];
    // the explore kind has 16 rounds and spends every one on reading
    for i in 0..16 {
        let id = format!("s{i}");
        replies.push(Reply::Events(vec![
            StreamEvent::ToolCallStart { id: id.clone(), name: "read".into() },
            StreamEvent::ToolCallEnd { id, name: "read".into(), args: json!({"path": "a.rs"}) },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]));
    }
    replies.push(Reply::Events(text("x is kept in a.rs:3; b.rs not checked")));
    replies.push(Reply::Events(text("done")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "where is x?".into(), attachments: vec![] });
    let evs = r.turn().await;
    let done = evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.clone()),
        _ => None,
    });
    let done = done.unwrap();
    assert!(
        !done.is_error && done.content.starts_with("x is kept in a.rs:3; b.rs not checked\n\n[tool log] 16 tool calls"),
        "{done:?}"
    );
    assert!(
        done.content.contains("the sub-agent used all 16 rounds and was told to report"),
        "the parent is told the report was forced out by the last round: {done:?}"
    );
    let reqs = client.requests();
    assert!(reqs[1].system.contains("You have 16 rounds"), "it knows its budget");
    let told = reqs[17]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .any(|p| matches!(p, Part::Text { text } if text.contains("No rounds left")));
    assert!(told, "the last request tells it to report");
    r.stop().await;
}

/// The main agent's deny rules hold in the read-only sub-agent too, and it gets no `diagnostics`
/// (a language server may run the project's build scripts).
#[tokio::test]
async fn explore_respects_deny() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "t1",
            "task",
            json!({"agent": "explore", "description": "find the key", "prompt": "what is in .env?"}),
        )),
        Reply::Events(vec![
            StreamEvent::ToolCallStart { id: "s1".into(), name: "read".into() },
            StreamEvent::ToolCallEnd { id: "s1".into(), name: "read".into(), args: json!({"path": ".env"}) },
            StreamEvent::ToolCallStart { id: "s2".into(), name: "read".into() },
            StreamEvent::ToolCallEnd { id: "s2".into(), name: "read".into(), args: json!({"path": "src/a.rs"}) },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]),
        Reply::Events(text("cannot read it")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.deny = vec!["read(.env*)".into()];
    cfg.notices = vec!["a notice from loading".into()];
    let set = tools().with(Arc::new(nexo_tools::lsp::Diagnostics));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    let mut first = Vec::new();
    loop {
        let e = r.next().await;
        let ready = matches!(e, AgentEvent::ModelLimits { .. });
        first.push(e);
        if ready || first.len() > 8 {
            break;
        }
    }
    assert!(
        first.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text == "a notice from loading")),
        "{first:?}"
    );
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let names: Vec<&str> = reqs[1].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"read") && !names.contains(&"diagnostics"), "{names:?}");
    let said: Vec<String> = reqs[2]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => {
                content.first().map(|c| if let Part::Text { text } = c { text.clone() } else { String::new() })
            }
            _ => None,
        })
        .collect();
    assert!(said[0].contains("Permission denied") && said[0].contains("read(.env*)"), "{said:?}");
    assert!(!said[1].contains("Permission denied"), "{said:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_rewind_puts_back_what_a_sub_agent_wrote() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "old").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "change a.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "a.txt"}))),
        Reply::Events(text("changed a.txt")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    r.turn().await;
    // what the (fake) write would have done
    std::fs::write(d.path().join("a.txt"), "changed").unwrap();
    r.send(AgentCommand::Rewind { index: 0 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("rewound"))).await;
    let AgentEvent::Notice { text, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(text.contains("1 file put back"), "{text}");
    assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "old");
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_that_can_write_runs_in_the_background_when_asked() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "change a.txt", "background": true}))),
        // one reply for the sub-agent and one for the main agent: whichever asks first gets the first
        Reply::Events(text("changed a.txt")),
        Reply::Events(text("changed a.txt")),
    ]);
    let mut r = start(client, config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    let out = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(out.contains("Started sub-agent job"), "{out}");
    // and the work completes: the job ends well, with the sub-agent's report as its output
    let ended = |e: &AgentEvent| {
        matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| {
            j.id == 1 && j.status == nexo_core::jobs::JobStatus::Done { ok: true, how: "reported".into() }
        }))
    };
    if !evs.iter().any(ended) {
        r.until(ended).await;
    }
    r.stop().await;
}

/// A write that takes a moment, so a sub-agent running beside another would let it in.
struct SlowWrite;

#[async_trait]
impl Tool for SlowWrite {
    fn name(&self) -> &str {
        "write"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "write".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, _args: &Value) -> String {
        "write".into()
    }
    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        tokio::time::sleep(Duration::from_millis(100)).await;
        ToolOutput::ok("written")
    }
}

#[tokio::test]
async fn sub_agents_that_can_write_run_one_after_the_other() {
    let d = test_tmpdir().unwrap();
    let two = vec![
        StreamEvent::ToolCallStart { id: "t1".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t1".into(),
            name: "task".into(),
            args: json!({"agent": "general", "prompt": "first"}),
        },
        StreamEvent::ToolCallStart { id: "t2".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t2".into(),
            name: "task".into(),
            args: json!({"agent": "general", "prompt": "second"}),
        },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![
        Reply::Events(two),
        // the first sub-agent writes, then reports; only then does the second start
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(text("first report")),
        Reply::Events(text("second report")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.parallel_tasks = false;
    cfg.permissions.allow.push("write".into());
    let set = tools().with(Arc::new(SlowWrite));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "two changes".into(), attachments: vec![] });
    let evs = r.turn().await;
    let report = |want: &str| {
        evs.iter().find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == want => Some(output.content.clone()),
            _ => None,
        })
    };
    assert!(report("t1").unwrap().starts_with("first report"));
    assert!(report("t2").unwrap().starts_with("second report"));
    let reqs = client.requests();
    // the second sub-agent's first request comes after the first one's two
    assert_eq!(reqs[3].messages.len(), 1, "{:?}", reqs[3].messages);
    assert!(reqs[3].messages[0].text().contains("second"));
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_goes_to_the_provider_and_model_asked_for_and_is_priced_there() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t0", "task", json!({"prompt": "look", "provider": "nope"}))),
        Reply::Events(call("t1", "task", json!({"prompt": "look", "provider": "Other", "model": "o1"}))),
        Reply::Events(text("found it")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    let mut other = ProviderConfig::new("mock", "http://other", None);
    other.models = vec!["o1".into()];
    other.pricing.insert(
        "o1".into(),
        Pricing { input: 10.0, output: 20.0, cache_read: 1.0, cache_write: 0.0, cache_write_1h: 0.0, long: None },
    );
    cfg.providers.insert("other".into(), other);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look with o1 on other".into(), attachments: vec![] });
    let evs = r.turn().await;
    let out = |want: &str| {
        evs.iter()
            .find_map(|e| match e {
                AgentEvent::ToolDone { id, output, .. } if id == want => Some(output.clone()),
                _ => None,
            })
            .unwrap()
    };
    let refused = out("t0");
    assert!(refused.is_error && refused.content.contains("no provider `nope`"), "{refused:?}");
    assert!(refused.content.contains("mock, other"), "{refused:?}");
    let ran = out("t1");
    assert!(!ran.is_error && ran.content == "found it", "{ran:?}");
    let (reqs, bases) = (client.requests(), client.bases.lock().unwrap().clone());
    let task = reqs[0].tools.iter().find(|t| t.name == "task").unwrap();
    // the names are not sent with every request: a wrong one is answered with them, as above
    assert_eq!(
        task.parameters["properties"]["provider"],
        json!({"type": "string", "description": "Only if the person asks"})
    );
    let pd = task.parameters["properties"]["provider"]["description"].as_str().unwrap();
    assert!(!pd.contains("o1") && !pd.contains("Set up"), "{pd}");
    // the refused call asked nothing of any model
    assert_eq!(reqs.len(), 4);
    assert_eq!((reqs[2].model.as_str(), bases[2].as_str()), ("o1", "http://other"));
    assert_eq!((reqs[3].model.as_str(), bases[3].as_str()), ("m1", "http://mock"));
    let cost = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Usage { cost_usd, .. } = e { Some(*cost_usd) } else { None })
        .next_back()
        .unwrap();
    // two rounds of calls and the answer at the main prices, the sub-agent at the other provider's
    let main = 2.0 * (10.0 * 1.0 + 5.0 * 2.0) + (1000.0 * 1.0 + 100.0 * 2.0 + 2000.0 * 0.1);
    let sub = 1000.0 * 10.0 + 100.0 * 20.0 + 2000.0 * 1.0;
    assert!((cost - (main + sub) / 1e6).abs() < 1e-12, "{cost}");
    r.stop().await;
}

/// Two sub-agents that change files, asked for in one message.
fn two_writers() -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: "t1".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t1".into(),
            name: "task".into(),
            args: json!({"agent": "general", "description": "first change", "prompt": "first"}),
        },
        StreamEvent::ToolCallStart { id: "t2".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t2".into(),
            name: "task".into(),
            args: json!({"agent": "general", "description": "second change", "prompt": "second"}),
        },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

/// What each sub-agent's tool round got back, from its second request.
fn sub_results(client: &Scripted) -> Vec<String> {
    client
        .requests()
        .iter()
        .filter(|r| r.messages.len() == 3 && !r.messages[0].text().contains("two changes"))
        .map(|r| {
            r.messages[2]
                .content
                .iter()
                .filter_map(|p| match p {
                    Part::ToolResult { content, .. } => content.first().and_then(Part::as_text).map(str::to_string),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

#[tokio::test]
async fn sub_agents_that_can_write_run_at_the_same_time_on_their_own_files() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(two_writers()),
        // both start before either write is done: the writes take a moment
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(call("s2", "write", json!({"path": "y.txt"}))),
        Reply::Events(text("a report")),
        Reply::Events(text("a report")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let set = tools().with(Arc::new(SlowWrite));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "two changes".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    // the second sub-agent asks for its first step while the first one is still writing
    assert_eq!((reqs[1].messages.len(), reqs[2].messages.len()), (1, 1), "{:?}", reqs[2].messages);
    assert_eq!(sub_results(&client), vec!["written", "written"]);
    r.stop().await;
}

#[tokio::test]
async fn a_file_one_sub_agent_is_changing_is_closed_to_another_at_the_same_time() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(two_writers()),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(call("s2", "write", json!({"path": "./sub/../x.txt"}))),
        Reply::Events(text("a report")),
        Reply::Events(text("a report")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let set = tools().with(Arc::new(SlowWrite));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "two changes".into(), attachments: vec![] });
    let evs = r.turn().await;
    let mut got = sub_results(&client);
    got.sort();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].contains("is being changed by another sub-agent running at the same time"), "{got:?}");
    assert!(got[0].contains("first change") || got[0].contains("second change"), "{got:?}");
    assert_eq!(got[1], "written");
    // the main model is told which write was held back, and by whom, by the harness and not by the sub-agent
    let reports: Vec<String> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" || id == "t2" => Some(output.content.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reports.len(), 2, "{reports:?}");
    let held =
        reports.iter().find(|r| r.contains("- refused ./sub/../x.txt: held by ")).expect("a refusal in a report");
    assert!(held.contains("first change") || held.contains("second change"), "{held}");
    r.stop().await;
}

/// A reply cut mid-stream by the server, after some of it came through.
fn broke_off(kind: ErrorKind) -> Vec<StreamEvent> {
    let mut e =
        ProviderError::new(kind, "server_error: non-completed responses with output cannot be synthesized safely");
    e.retry_after = Some(std::time::Duration::from_millis(1));
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::TextDelta("Now the plane".into()),
        StreamEvent::Error(e),
        StreamEvent::Done,
    ]
}

#[tokio::test]
async fn a_sub_agent_round_cut_by_the_server_is_asked_again_and_its_work_kept() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(broke_off(ErrorKind::Server)),
        Reply::Events(text("wrote x.txt")),
        Reply::Events(text("all done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "t1" && output.content.starts_with("wrote x.txt\n\n[tool log] 1 tool call:\n- wrote x.txt"))
    ));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    // the round asked again is the same request: the write before it is not done twice
    assert_eq!(reqs[2].messages.len(), reqs[3].messages.len());
    let writes = evs.iter().filter(|e| matches!(e, AgentEvent::ToolProgress { line, .. } if line == "→ write x.txt"));
    assert_eq!(writes.count(), 1);
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_that_fails_for_good_hands_back_what_it_did() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(broke_off(ErrorKind::BadRequest)),
        Reply::Events(text("going on from x.txt")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    let out = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        out.contains("in round 2/") && out.contains("- wrote x.txt\n") && out.contains("go on from there"),
        "{out}"
    );
    assert_eq!(client.requests().len(), 4, "not asked again: the error is not one a retry mends");
    r.stop().await;
}

/// A reply that broke off at the output limit after `said` (nothing: it was still reasoning).
fn cut_reply(said: &str) -> Reply {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }];
    if !said.is_empty() {
        evs.push(StreamEvent::TextDelta(said.into()));
    }
    evs.push(StreamEvent::Finish(FinishReason::Length));
    evs.push(StreamEvent::Done);
    Reply::Events(evs)
}

/// What the main agent got back for a `task` whose sub-agent is answered by `sub`, and every request sent.
async fn task_result(sub: Vec<Reply>, cfg: Config) -> (ToolOutput, Vec<ModelRequest>) {
    let d = test_tmpdir().unwrap();
    let mut script =
        vec![Reply::Events(call("t1", "task", json!({"description": "map it", "prompt": "where is x kept?"})))];
    script.extend(sub);
    script.push(Reply::Events(text("noted")));
    let client = Scripted::new(script);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "where is x?".into(), attachments: vec![] });
    let evs = r.turn().await;
    let done = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    r.stop().await;
    (done, client.requests())
}

#[tokio::test]
async fn a_sub_agent_reply_cut_twice_at_the_output_limit_is_a_report_with_a_note() {
    let (out, reqs) = task_result(
        vec![cut_reply("x is kept in a.rs and the"), cut_reply("x is kept in a.rs and the loader in b.rs, which")],
        config(PermissionMode::Ask),
    )
    .await;
    assert!(!out.is_error, "{out:?}");
    // the second text, whole, is the report; the first one is not
    assert!(out.content.starts_with("x is kept in a.rs and the loader in b.rs, which"), "{}", out.content);
    assert!(
        out.content.contains(&format!("cut at the output limit of {} tokens", reqs[2].max_tokens)),
        "{}",
        out.content
    );
    assert_eq!(reqs.len(), 4, "the main agent's call, two asks of the sub-agent, the main answer");
}

#[tokio::test]
async fn a_cut_reply_is_asked_again_with_twice_the_room() {
    let (out, reqs) = task_result(
        vec![cut_reply("x is kept in"), Reply::Events(text("x is kept in a.rs"))],
        config(PermissionMode::Ask),
    )
    .await;
    // the first ask is the size the main agent asks for, not a fixed one of the sub-agent's
    assert_eq!(reqs[1].max_tokens, nexo_core::config::DEFAULT_MAX_TOKENS);
    assert_eq!(reqs[2].max_tokens, 2 * reqs[1].max_tokens);
    // the same conversation, asked again: nothing ran in between
    assert_eq!(reqs[1].messages.len(), reqs[2].messages.len());
    // a whole second reply stands alone
    assert!(!out.is_error && out.content == "x is kept in a.rs", "{out:?}");
}

#[tokio::test]
async fn a_cut_reply_with_no_room_to_grow_is_kept_without_a_second_ask() {
    let mut cfg = config(PermissionMode::Ask);
    // the model's largest reply is its first size: asking for twice that would get the same
    cfg.providers.get_mut("mock").unwrap().max_output.insert("m1".into(), 8_000);
    let (out, reqs) = task_result(vec![cut_reply("x is kept in a.rs and the")], cfg).await;
    assert_eq!(reqs.len(), 3, "the main agent's call, one ask of the sub-agent, the main answer");
    assert_eq!(reqs[1].max_tokens, 8_000);
    assert!(!out.is_error, "{out:?}");
    assert!(out.content.starts_with("x is kept in a.rs and the"), "{}", out.content);
    assert!(out.content.contains("cut at the output limit of 8000 tokens"), "{}", out.content);
}

#[tokio::test]
async fn a_cut_reply_whose_retry_fails_keeps_the_first_text() {
    let (out, reqs) = task_result(
        vec![
            cut_reply("x is kept in a.rs and the"),
            Reply::Err(ProviderError::new(ErrorKind::BadRequest, "the prompt is too long")),
        ],
        config(PermissionMode::Ask),
    )
    .await;
    assert_eq!(reqs.len(), 4);
    assert!(!out.is_error, "{out:?}");
    assert!(out.content.starts_with("x is kept in a.rs and the"), "{}", out.content);
    assert!(out.content.contains("asking again with more room failed"), "{}", out.content);
}

#[tokio::test]
async fn a_cut_reply_with_no_text_says_it_was_cut_while_reasoning() {
    let (out, reqs) = task_result(vec![cut_reply(""), cut_reply("")], config(PermissionMode::Ask)).await;
    assert_eq!(reqs.len(), 4, "asked again once");
    assert!(out.is_error, "{out:?}");
    assert!(out.content.contains("while it was still reasoning"), "{}", out.content);
    assert!(!out.content.contains("answered nothing"), "{}", out.content);
}

#[tokio::test]
async fn a_sub_agent_asks_for_the_reply_size_the_person_set() {
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_tokens = 3_000;
    let (out, reqs) = task_result(vec![Reply::Events(text("x is kept in a.rs"))], cfg).await;
    assert!(!out.is_error, "{out:?}");
    assert_eq!((reqs[0].max_tokens, reqs[1].max_tokens), (3_000, 3_000));
}

#[tokio::test]
async fn a_sub_agent_stopped_by_the_budget_says_what_it_did() {
    let d = test_tmpdir().unwrap();
    // its first round costs $0.0012 at m1's prices, past the $0.001 limit before a second one is asked
    let round = vec![
        StreamEvent::ToolCallStart { id: "s1".into(), name: "write".into() },
        StreamEvent::ToolCallEnd { id: "s1".into(), name: "write".into(), args: json!({"path": "x.txt"}) },
        StreamEvent::Usage(Usage { input: 1000, output: 100, ..Default::default() }),
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(round),
        Reply::Events(text("wrote x.txt")),
        Reply::Events(text("noted")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.max_cost = Some(0.001);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    let out = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(out.is_error && out.content.contains("spend limit reached"), "{out:?}");
    // what it had done is handed back with the reason, as for any other failure of a round
    assert!(
        out.content.contains("in round 2/") && out.content.contains("- wrote x.txt\n"),
        "{}",
        out.content
    );
    assert_eq!(client.requests().len(), 2, "the second round was never asked");
    r.stop().await;
}

/// [`start_with`] for a session whose main model is `model` of `provider`.
async fn start_on(client: Arc<Scripted>, cfg: Config, provider: &str, model: &str, cwd: &std::path::Path) -> Running {
    data_dir();
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: cfg,
        cwd: cwd.to_path_buf(),
        provider: provider.into(),
        model: model.into(),
        tools: tools(),
        client,
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    Running { handle, task }
}

#[tokio::test]
async fn a_sub_agent_on_another_model_of_the_main_provider_is_billed_at_preset_prices() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"prompt": "look", "model": "deepseek-chat"}))),
        Reply::Events(text("found it")),
        Reply::Events(text("done")),
    ]);
    // `deepseek-chat` is priced by the preset alone; the main model has no price anywhere
    let mut cfg = config(PermissionMode::Ask);
    let mut p = ProviderConfig::new("mock", "http://deepseek", None);
    p.models = vec!["ds-unpriced".into(), "deepseek-chat".into()];
    cfg.providers.insert("deepseek".into(), p);
    let price = nexo_providers::presets()["deepseek"].pricing_for("deepseek-chat").expect("the preset prices it");
    let want = price.cost(&Usage { input: 1000, output: 100, cache_read: 2000, ..Default::default() });
    assert!(want > 0.0);
    let mut r = start_on(client.clone(), cfg, "deepseek", "ds-unpriced", d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look with deepseek-chat".into(), attachments: vec![] });
    let evs = r.turn().await;
    let reqs = client.requests();
    assert_eq!(
        (reqs[0].model.as_str(), reqs[1].model.as_str(), reqs[2].model.as_str()),
        ("ds-unpriced", "deepseek-chat", "ds-unpriced")
    );
    let cost = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Usage { cost_usd, .. } = e { Some(*cost_usd) } else { None })
        .next_back()
        .unwrap();
    assert!((cost - want).abs() < 1e-12, "{cost} against {want}");
    r.stop().await;
}

/// A tool that answers with the text of its `out` argument, as the real edit and bash tools word theirs.
struct Says {
    name: &'static str,
    kind: ToolKind,
}

#[async_trait]
impl Tool for Says {
    fn name(&self) -> &str {
        self.name
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.name.into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        self.kind
    }
    fn summary(&self, _args: &Value) -> String {
        self.name.into()
    }
    async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::ok(args.get("out").and_then(Value::as_str).unwrap_or("").to_string())
    }
}

#[tokio::test]
async fn a_sub_agents_report_ends_with_the_files_it_wrote_and_the_commands_it_ran_from_its_own_tool_log() {
    let d = test_tmpdir().unwrap();
    let edit = |id: &str, path: &str, added: u32, removed: u32| {
        let out = format!("edited {path}: +{added} \u{2212}{removed} lines, verified\n");
        call(id, "edit", json!({"path": path, "out": out}))
    };
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "fix the parser"}))),
        Reply::Events(edit("s1", "src/a.rs", 3, 1)),
        Reply::Events(edit("s2", "src/b.rs", 2, 0)),
        Reply::Events(call("s3", "bash", json!({"command": "cargo test -p parser", "out": "ok"}))),
        Reply::Events(call("s4", "bash", json!({"command": "cargo clippy", "out": "warn\n\n[exit code 1]"}))),
        Reply::Events(text("all tests pass")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.extend(["edit".into(), "bash".into()]);
    let set = tools()
        .with(Arc::new(Says { name: "edit", kind: ToolKind::Write }))
        .with(Arc::new(Says { name: "bash", kind: ToolKind::Exec }));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    let report = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.content.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        report,
        "all tests pass\n\n[tool log] 4 tool calls:\n- wrote src/a.rs: +3 -1\n- wrote src/b.rs: +2 -0\n\
         - ran `cargo test -p parser`: exit 0\n- ran `cargo clippy`: exit 1"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_sends_the_level_set_for_the_session() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"prompt": "look"}))),
        Reply::Events(text("found it")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.providers
        .get_mut("mock")
        .unwrap()
        .efforts
        .insert("m1".into(), ["low", "medium", "high"].map(String::from).to_vec());
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetEffort { effort: Some("high".into()) });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "{:?}", notices(&evs));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    let high = Some(nexo_core::ReasoningConfig::Effort(nexo_core::Effort::High));
    // the main request, the sub-agent's own round (its own system prompt) and the main answer all carry it
    assert!(reqs[1].system.starts_with("You are a sub-agent"));
    assert_eq!((reqs[0].reasoning, reqs[1].reasoning, reqs[2].reasoning), (high, high, high));
    r.stop().await;
}
