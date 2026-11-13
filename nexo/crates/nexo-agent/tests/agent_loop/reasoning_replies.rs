use super::*;

/// A reply that only thinks.
fn thought(s: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ReasoningDelta(s.into()),
        StreamEvent::Usage(Usage { input: 1000, output: 100, cache_read: 2000, ..Default::default() }),
        StreamEvent::Finish(FinishReason::Length),
        StreamEvent::Done,
    ]
}

#[tokio::test]
async fn a_reply_of_only_reasoning_is_asked_to_go_on() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(thought("now write it: first")), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("only reasoning")), "{:?}", notices(&evs));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    let sent = &reqs[1].messages;
    assert!(nexo_core::message::is_well_formed(sent), "{sent:?}");
    assert!(sent.last().unwrap().text().contains("only reasoning"), "{sent:?}");
    // cut at max_tokens while thinking: more room only bought more reasoning, so the limit stays
    assert_eq!(reqs[1].max_tokens, reqs[0].max_tokens);
    r.stop().await;
}

#[tokio::test]
async fn replies_of_only_reasoning_stop_after_two_asks() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(thought("a")), Reply::Events(thought("b")), Reply::Events(thought("c"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    // the room stays at agent.max_tokens; the second ask says to build first
    let base = reqs[0].max_tokens;
    assert_eq!((reqs[1].max_tokens, reqs[2].max_tokens), (base, base));
    assert!(reqs[2].messages.last().unwrap().text().contains("Stop planning"));
    // the next request of the person starts from agent.max_tokens again
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    assert_eq!(client.requests()[3].max_tokens, base);
    r.stop().await;
}

/// A stream the network cut after `said`, as DeepSeek's breaks: billed `input` tokens all the same.
fn broken(said: Vec<StreamEvent>, input: u64) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }];
    evs.extend(said);
    evs.push(StreamEvent::Usage(Usage { input, output: 7, ..Default::default() }));
    evs.push(StreamEvent::Error(ProviderError::new(ErrorKind::Network, "stream cut: error decoding response body")));
    evs.push(StreamEvent::Done);
    evs
}

fn reasoning(s: &str) -> StreamEvent {
    StreamEvent::ReasoningDelta(s.into())
}

/// The session's tokens after the last call of `evs`.
fn spent(evs: &[AgentEvent]) -> Usage {
    evs.iter()
        .rev()
        .find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None })
        .unwrap()
}

const SENT_AGAIN: &str = "cut while the model reasoned";

#[tokio::test]
async fn a_stream_cut_inside_the_reasoning_is_sent_again_without_it() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(broken(vec![reasoning("PLAN-A: the eigenvalue "), reasoning("comes from")], 400)),
        Reply::Events(text("done")),
        Reply::Events(text("and again")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "{:?}", notices(&evs));
    assert_eq!(text_of(&evs), "done");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "the request went out a second time");
    assert_eq!(reqs[1].messages, reqs[0].messages, "the same request: nothing of the cut reply in it");
    assert!(!serde_json::to_string(&reqs[1]).unwrap().contains("PLAN-A"));
    assert_eq!(*client.waits.lock().unwrap(), [1], "the client's own wait, once");
    // both attempts are spent: 400 + 7 for the cut one, 1000 + 2000 cached + 100 for the answer
    let session = spent(&evs);
    assert_eq!((session.input, session.cache_read, session.output), (1400, 2000, 107), "{session:?}");
    // on the screen the cut reasoning is not one reply with what follows: the notice and a new start part them
    let at = |f: &dyn Fn(&AgentEvent) -> bool| evs.iter().position(f).unwrap();
    let said = at(&|e| matches!(e, AgentEvent::Notice { level: Level::Warn, text, .. } if text.contains(SENT_AGAIN)));
    let starts: Vec<usize> =
        evs.iter().enumerate().filter(|(_, e)| matches!(e, AgentEvent::AssistantStart)).map(|(i, _)| i).collect();
    assert_eq!(starts.len(), 2);
    assert!(at(&|e| matches!(e, AgentEvent::ReasoningDelta { .. })) < said && said < starts[1], "{evs:?}");
    // and it was never a message: not told as one, not in what the next request of the person carries
    let told: Vec<&Message> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::AssistantDone { message } = e { Some(message) } else { None })
        .collect();
    assert_eq!(told.len(), 1);
    assert_eq!((told[0].text().as_str(), told[0].reasoning().as_str()), ("done", ""));
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    r.turn().await;
    let next = &client.requests()[2].messages;
    assert_eq!(next.len(), 3, "{next:?}");
    assert!(!serde_json::to_string(next).unwrap().contains("PLAN-A"));
    r.stop().await;
}

#[tokio::test]
async fn three_cuts_inside_the_reasoning_stop_with_the_error_after_two_more_tries() {
    let d = test_tmpdir().unwrap();
    let cut = || Reply::Events(broken(vec![reasoning("thinking")], 400));
    let client = Scripted::new(vec![cut(), cut(), cut(), Reply::Events(text("never"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert_eq!(client.requests().len(), 3, "the request and two more tries");
    assert_eq!(*client.waits.lock().unwrap(), [1, 2]);
    assert_eq!(notices(&evs).iter().filter(|n| n.contains(SENT_AGAIN)).count(), 2, "{:?}", notices(&evs));
    // the error of the last cut is the one the person reads, as a failed call always was
    let p = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::Notice { level: Level::Error, problem: Some(p), .. } => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("an error notice with a problem: {evs:?}"));
    assert!(p.plain().contains("stream cut"), "{p:?}");
    assert_eq!(spent(&evs).input, 1200, "the three attempts are spent");
    r.stop().await;
}

#[tokio::test]
async fn a_stream_cut_after_text_or_a_call_began_is_not_sent_again() {
    let begun = StreamEvent::ToolCallStart { id: "c1".into(), name: "read".into() };
    for said in [vec![reasoning("hm"), StreamEvent::TextDelta("half".into())], vec![reasoning("hm"), begun]] {
        let d = test_tmpdir().unwrap();
        let client = Scripted::new(vec![Reply::Events(broken(said.clone(), 400)), Reply::Events(text("never"))]);
        let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Error, "{said:?}");
        assert_eq!(client.requests().len(), 1, "{said:?}");
        assert!(!notices(&evs).iter().any(|n| n.contains(SENT_AGAIN)), "{:?}", notices(&evs));
        r.stop().await;
    }
}

#[tokio::test]
async fn with_retries_off_a_cut_inside_the_reasoning_is_not_sent_again() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(broken(vec![reasoning("thinking")], 400)), Reply::Events(text("never"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.retries = 0;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    assert_eq!(end_of(&r.turn().await), TurnEnd::Error);
    assert_eq!(client.requests().len(), 1);
    r.stop().await;
}

#[tokio::test]
async fn the_ask_to_go_on_carries_the_end_of_the_reasoning_the_provider_does_not_send_back() {
    let d = test_tmpdir().unwrap();
    let long = format!("{}PLAN: write ars.R with a log-concave hull, then test it", "x".repeat(20_000));
    let client = Scripted::new(vec![Reply::Events(thought(&long)), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let note = reqs[1].messages.last().unwrap().text();
    assert!(note.contains("PLAN: write ars.R with a log-concave hull, then test it"), "{note}");
    // only the end of it: a note as long as the thinking would cost what the cut already cost
    assert!(note.len() < 6_000, "{}", note.len());
    r.stop().await;
}

/// A reply that thinks `chars` characters in pieces, then says `after`, as the provider would stream it.
fn long_thought(chars: usize, after: Vec<StreamEvent>) -> Vec<StreamEvent> {
    let mut ev = vec![StreamEvent::Start { response_id: None, model: None }];
    for _ in 0..chars / 1000 {
        ev.push(StreamEvent::ReasoningDelta("x".repeat(1000)));
    }
    ev.extend(after);
    ev
}

fn then_text(s: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(s.into()),
        StreamEvent::Usage(Usage { input: 1000, output: 100, ..Default::default() }),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ]
}

#[tokio::test]
async fn reasoning_past_the_budget_with_nothing_said_is_cut_and_asked_again() {
    let d = test_tmpdir().unwrap();
    // 40,000 characters are 10,000 tokens, past the 8,000 default; the text after them is never read
    let client =
        Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("never read"))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")), "{:?}", notices(&evs));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    // the ask names the budget and repeats none of the cut thinking, which is not in the history either
    let note = reqs[1].messages.last().unwrap().text();
    assert!(note.contains("8000-token limit") && !note.contains("xxxx"), "{note}");
    assert_eq!(thinking_sent(&reqs[1]), (0, 0));
    assert!(!reqs[1].messages.iter().any(|m| m.text().contains("never read")));
    // no more room: it would only buy more reasoning
    assert_eq!(reqs[1].max_tokens, reqs[0].max_tokens);
    // the model lists no reasoning level: none is sent, as a provider that takes none may refuse one
    assert_eq!(reqs[1].reasoning, None);
    r.stop().await;
}

#[tokio::test]
async fn three_cuts_at_the_budget_stop_the_turn_with_a_short_message() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(long_thought(40_000, then_text("never read"))),
        Reply::Events(long_thought(40_000, then_text("never read"))),
        Reply::Events(long_thought(40_000, then_text("never read either"))),
        Reply::Events(text("never asked")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    // the budget stays on: the third reply is cut too, and no fourth request goes out
    assert_eq!(client.requests().len(), 3);
    assert!(!text_of(&evs).contains("never"), "{}", text_of(&evs));
    let said = notices(&evs);
    assert_eq!(said.iter().filter(|n| n.contains("cut and asked")).count(), 2, "{said:?}");
    let last = said.last().unwrap();
    assert!(last.contains("reasoning_budget (8000 tokens) 3 times") && last.contains("stopped"), "{said:?}");
    assert!(last.contains("Send \"continue\""), "{last}");
    r.stop().await;
}

#[tokio::test]
async fn a_run_with_nobody_there_stops_after_five_cuts_at_the_budget() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new((0..7).map(|_| Reply::Events(long_thought(40_000, then_text("never read")))).collect());
    let mut r = start_alone(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "{:?}", notices(&evs));
    // the asks do not pile up the cut thinking: the last request carries none of it, and four short notes
    assert_eq!(thinking_sent(&reqs[4]).0, 0);
    let notes: usize = reqs[4].messages.iter().skip(1).map(|m| m.text().len()).sum();
    assert!(notes < 2_000, "{notes}");
    let said = notices(&evs);
    let last = said.last().unwrap();
    assert!(last.contains("5 times") && last.contains("stopped") && !last.contains("continue"), "{said:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_reply_that_only_reasons_ends_with_a_notice() {
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.reasoning_budget = 0;
    let only = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ReasoningDelta("thinking".into()),
        StreamEvent::Usage(Usage { input: 1000, output: 10, ..Default::default() }),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![Reply::Events(only.clone()), Reply::Events(only.clone()), Reply::Events(only)]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("reasoned without answering")), "{:?}", notices(&evs));
    // a person is there to send "continue": two asks to go on and no more
    assert_eq!(client.requests().len(), 3);
    r.stop().await;
}

/// Answers for a person who is not there, as `nexo -p` does.
struct Nobody;

#[async_trait]
impl nexo_core::permission::PermissionGate for Nobody {
    async fn ask(&self, _req: nexo_core::permission::PermissionRequest) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

/// As `start`, for a run no person watches: a gate answers in their place.
async fn start_alone(client: Arc<Scripted>, cfg: Config, cwd: &std::path::Path) -> Running {
    data_dir();
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: cfg,
        cwd: cwd.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: tools(),
        client,
        resume: None,
        gate: Some(Arc::new(Nobody)),
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    Running { handle, task }
}

#[tokio::test]
async fn a_run_with_nobody_there_goes_on_after_three_budget_cuts() {
    let d = test_tmpdir().unwrap();
    // three cuts at the budget: the third reply is not let run free past it
    let third = format!("{}NEXT: write gen.py with three multipliers", "y".repeat(60_000));
    let client = Scripted::new(vec![
        Reply::Events(long_thought(40_000, then_text("never read"))),
        Reply::Events(long_thought(40_000, then_text("never read"))),
        Reply::Events(thought(&third)),
        Reply::Events(call("c1", "read", json!({"path": "sim.c"}))),
        Reply::Events(text("built it")),
    ]);
    let mut r = start_alone(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "{:?}", notices(&evs));
    assert!(text_of(&evs).contains("built it"), "{}", text_of(&evs));
    // nobody can send "continue", and the request was not left there
    assert!(!notices(&evs).iter().any(|n| n.contains("reasoned without answering")), "{:?}", notices(&evs));
    assert_eq!(notices(&evs).iter().filter(|n| n.contains("agent.reasoning_budget (8000")).count(), 3);
    // the ask after the third cut names the budget and carries none of the thinking, with no more room
    let note = reqs[3].messages.last().unwrap().text();
    assert!(note.contains("limit again") && !note.contains("NEXT: write gen.py"), "{note}");
    assert_eq!(thinking_sent(&reqs[3]), (0, 0));
    assert_eq!(reqs[3].max_tokens, reqs[0].max_tokens);
    r.stop().await;
}

#[tokio::test]
async fn a_run_with_nobody_there_stops_after_five_replies_in_a_row_that_only_reasoned() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new((0..8).map(|_| Reply::Events(thought("thinking it over"))).collect());
    let mut r = start_alone(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 5, "{:?}", notices(&evs));
    let said = notices(&evs);
    assert!(said.iter().any(|n| n.contains("reasoned without answering 5 times in a row")), "{said:?}");
    assert!(!said.iter().any(|n| n.contains("Send \"continue\"")), "{said:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_budget_of_zero_lets_the_reasoning_run() {
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.reasoning_budget = 0;
    let client = Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("answer")))]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 1);
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")));
    r.stop().await;
}

#[tokio::test]
async fn reasoning_after_text_is_not_cut_by_the_budget() {
    let d = test_tmpdir().unwrap();
    let mut ev = vec![StreamEvent::Start { response_id: None, model: None }, StreamEvent::TextDelta("first ".into())];
    ev.extend(long_thought(40_000, then_text("last")).into_iter().skip(1));
    let client = Scripted::new(vec![Reply::Events(ev)]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 1);
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")));
    r.stop().await;
}

#[tokio::test]
async fn reasoning_after_a_call_started_is_not_cut_by_the_budget() {
    let d = test_tmpdir().unwrap();
    let mut ev = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ToolCallStart { id: "c1".into(), name: "read".into() },
    ];
    ev.extend(long_thought(40_000, then_text("last")).into_iter().skip(1));
    let client = Scripted::new(vec![Reply::Events(ev), Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")), "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn a_reasoning_level_set_by_the_person_skips_the_budget() {
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.reasoning = Some("high".into());
    let client = Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("answer")))]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 1);
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")));
    r.stop().await;
}

#[tokio::test]
async fn a_cut_stream_is_billed_from_what_was_streamed() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("never read"))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    let billed: Vec<u64> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Usage { call, .. } = e { Some(call.output) } else { None })
        .collect();
    assert!(billed.iter().any(|o| *o >= 8_000), "{billed:?}");
    r.stop().await;
}

#[tokio::test]
async fn reasoning_that_ended_on_its_own_keeps_the_room() {
    let d = test_tmpdir().unwrap();
    let mut done = thought("all thought out");
    done[3] = StreamEvent::Finish(FinishReason::Stop);
    let client = Scripted::new(vec![Reply::Events(done), Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs[1].max_tokens, reqs[0].max_tokens);
    r.stop().await;
}

// ---------------------------------------------------------------- mail between sessions

#[tokio::test]
async fn an_empty_reply_is_not_called_reasoning() {
    let d = test_tmpdir().unwrap();
    let empty = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::Usage(Usage { input: 1000, output: 0, ..Default::default() }),
        StreamEvent::Finish(FinishReason::ContentFilter),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![Reply::Events(empty)]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!notices(&evs).iter().any(|n| n.contains("reasoned without answering")), "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn a_cut_reasoning_call_is_billed_its_input_too() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("never read"))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    let first = evs
        .iter()
        .find_map(|e| if let AgentEvent::Usage { call, .. } = e { Some(*call) } else { None })
        .expect("a usage event for the cut call");
    assert!(first.total_input() > 0, "the cut call sent a whole request and was billed none of it: {first:?}");
    // an estimate: not far above what was sent (4 characters a token, the nexo estimate)
    let sent = client.requests()[0].clone();
    let size = serde_json::to_string(&sent).unwrap().len() as u64 / 4;
    assert!(first.total_input() <= size * 2, "estimate {} for a request of about {size} tokens", first.total_input());
    r.stop().await;
}

/// A reply that thinks `chars` characters of `pad`, ending in `tail`, and says nothing: the shape of a
/// reasoning model that never gets to the answer.
fn long_thought_ending(chars: usize, pad: &str, tail: &str) -> Vec<StreamEvent> {
    let mut ev = vec![StreamEvent::Start { response_id: None, model: None }];
    for _ in 0..chars / 1000 {
        ev.push(StreamEvent::ReasoningDelta(pad.repeat(1000)));
    }
    ev.push(StreamEvent::ReasoningDelta(tail.into()));
    ev
}

/// What a request carries of the cut thinking: characters of reasoning parts in assistant messages, and of
/// the notes that repeat the end of it.
fn thinking_sent(req: &ModelRequest) -> (usize, usize) {
    let parts: usize = req.messages.iter().filter(|m| m.role == Role::Assistant).map(|m| m.reasoning().len()).sum();
    let notes: usize = req
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.text())
        .filter(|t| t.contains("The end of that reasoning"))
        .map(|t| t.len())
        .sum();
    (parts, notes)
}

/// The user's session: a reply reasons past the budget with no answer and no call, and is cut. The next request
/// asks for the next step at the least reasoning the model lists, names the budget, and carries none of the cut
/// thinking (before, it was the same request again at the same level, with the thinking in the history and its
/// end repeated in the note, growing at each ask); and a third cut is not followed by a free round but stops
/// with a short message.
#[tokio::test]
async fn a_budget_cut_retry_asks_for_the_next_step_with_less_thinking() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(long_thought_ending(40_000, "a", "END-OF-FIRST")),
        Reply::Events(long_thought_ending(40_000, "b", "END-OF-SECOND")),
        Reply::Events(long_thought_ending(40_000, "c", "END-OF-THIRD")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    let levels = ["low", "medium", "high"].map(String::from).to_vec();
    cfg.providers.get_mut("mock").unwrap().efforts.insert("m1".into(), levels);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "make a snake game in python".into(), attachments: vec![] });
    let evs = r.turn().await;
    let reqs = client.requests();
    for (i, q) in reqs.iter().enumerate() {
        let (parts, notes) = thinking_sent(q);
        let last = q.messages.last().unwrap().text();
        eprintln!(
            "request {i}: reasoning={:?} max_tokens={} msgs={} thinking_parts_chars={parts} tail_note_chars={notes} last={last}",
            q.reasoning,
            q.max_tokens,
            q.messages.len()
        );
    }
    eprintln!("notices: {:?}", notices(&evs));
    // the first request is the person's level (none set: the provider's own); the retries think less
    assert_eq!(reqs[0].reasoning, None);
    let low = Some(nexo_core::ReasoningConfig::Effort(nexo_core::Effort::Low));
    assert_eq!((reqs[1].reasoning, reqs[2].reasoning), (low, low));
    // the cut thinking is not carried, in the history or in the notes
    for q in &reqs[1..] {
        assert_eq!(thinking_sent(q), (0, 0));
        assert!(!serde_json::to_string(q).unwrap().contains("END-OF-"));
    }
    // the ask names the budget and says what to do; the second says it again, shorter
    let note = reqs[1].messages.last().unwrap().text();
    assert!(note.contains("8000-token limit") && note.contains("tool call now"), "{note}");
    assert!(reqs[2].messages.last().unwrap().text().contains("limit again"));
    // a third cut ends the turn with a message, not with a fourth request
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(reqs.len(), 3, "{:?}", notices(&evs));
    assert!(notices(&evs).iter().any(|n| n.contains("stopped")), "{:?}", notices(&evs));
    r.stop().await;
}
