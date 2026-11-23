use super::*;

#[tokio::test]
async fn a_max_output_error_caps_the_reply_and_retries_once() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Err(ProviderError::new(
            ErrorKind::BadRequest,
            "max_tokens: 65536 > 8192, which is the maximum allowed number of output tokens for m1",
        )),
        Reply::Events(text("fits now")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "fits now");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelLimits { max_output: Some(8192), source, .. } if source == "error")));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!((reqs[0].max_tokens, reqs[1].max_tokens), (16_384, 8_192));
    assert_eq!(user_config().providers["mock"].max_output["m1"], 8_192);
    r.stop().await;
}

#[tokio::test]
async fn a_context_error_with_a_number_records_the_window() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("first")),
        Reply::Err(ProviderError::new(
            ErrorKind::ContextLength,
            "This model's maximum context length is 131072 tokens. However, you requested 140000 tokens.",
        )),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("after")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ModelLimits { context_window: Some(131_072), source, .. } if source == "error")
    ));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("context window of m1"))));
    assert_eq!(user_config().providers["mock"].context_window["m1"], 131_072);
    r.stop().await;
}

#[tokio::test]
async fn a_window_learned_when_only_the_reply_went_over_shortens_the_reply_instead_of_compacting() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Err(ProviderError::new(
            ErrorKind::BadRequest,
            "This model's maximum context length is 8192 tokens. However, you requested 16500 tokens (130 in the \
             messages, 16370 in the completion). Please reduce the length of the messages or completion.",
        )),
        Reply::Events(text("short reply")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "short reply");
    assert!(!notices(&evs).iter().any(|n| n.contains("compact")), "{evs:?}");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "no summariser call between the two requests");
    assert!(!is_summary(&reqs[1]) && reqs[1].messages == reqs[0].messages);
    assert!(reqs[1].max_tokens < reqs[0].max_tokens && reqs[1].max_tokens <= 8192, "{}", reqs[1].max_tokens);
    r.stop().await;
}

#[tokio::test]
async fn a_window_learned_when_the_estimate_has_the_whole_reply_fitting_compacts_the_history() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    // the estimate puts the request and the 16384-token reply well under 131072, so a shorter reply would send the
    // same request again; the error said the history is what went over
    // (a history of one message has nothing to summarise, so a first turn is answered before the error)
    let client = Scripted::new(vec![
        Reply::Events(bare(text("first"))),
        Reply::Err(ProviderError::new(
            ErrorKind::ContextLength,
            "This model's maximum context length is 131072 tokens. However, you requested 140000 tokens.",
        )),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("after")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    assert!(is_summary(&reqs[2]), "the third call is the summariser's");
    r.stop().await;
}

// ------------------------------------------------------------ context errors under the curator

/// The window of the sessions below, in tokens (over 32k, where the prompt stays whole).
const WINDOW: u32 = 40_000;
/// Messages of about 3500 estimated tokens that fill the view of a session at `WINDOW`.
const FAT_TURNS: usize = 10;

/// A scripted reply with no usage: nexo goes by its own estimate alone, so no count teaches it a ratio.
fn bare(events: Vec<StreamEvent>) -> Vec<StreamEvent> {
    events.into_iter().filter(|e| !matches!(e, StreamEvent::Usage(_))).collect()
}

fn answers(n: usize) -> Vec<Reply> {
    (0..n).map(|_| Reply::Events(bare(text("ok")))).collect()
}

fn fat_message(i: usize) -> String {
    (0..1_600).map(|j| format!("w{i}x{j} ")).collect()
}

/// A curator session, at `window` when given, with `turns` long messages answered.
async fn fat_session(client: &Arc<Scripted>, window: Option<u32>, turns: usize, cwd: &std::path::Path) -> Running {
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    if let Some(w) = window {
        cfg.providers.get_mut("mock").unwrap().context_window.insert("m1".into(), w);
    }
    let mut r = start(client.clone(), cfg, None, cwd).await;
    r.next().await;
    for i in 0..turns {
        r.send(AgentCommand::Submit { text: fat_message(i), attachments: vec![] });
        r.turn().await;
    }
    r
}

/// What a request takes by nexo's own estimate, the system prompt and the tools included.
fn estimate(r: &ModelRequest) -> u64 {
    let fixed = (r.system.len() + serde_json::to_string(&r.tools).unwrap().len()) as u64 / 4;
    fixed + nexo_core::message::estimate_tokens(&r.messages)
}

/// Tokens by which `estimate` (core's count of the request as sent) may pass the curator's own count of the same
/// request, which the view's ceiling bounds: each message's rounding and the per-call notes counted apart.
const COUNT_GAP: u64 = 64;

/// Whether `after` is under three quarters of `before`, both by `estimate`, but for [`COUNT_GAP`].
fn under_three_quarters(after: u64, before: u64) -> bool {
    after.saturating_sub(COUNT_GAP) * 4 <= before * 3
}

fn no_summary(client: &Scripted) -> bool {
    !client.requests().iter().any(is_summary)
}

fn too_long(message: &str) -> Reply {
    Reply::Err(ProviderError::new(ErrorKind::ContextLength, message))
}

// The request fits comfortably by *every* local estimate. Learning a different ratio from this error does
// not change the short view or require a shorter reply, even though it changes the preflight ceiling.
const KNOWN_WINDOW_ERROR: &str = "This model's maximum context length is 131072 tokens.";

fn context_retry_config(protocol: &str) -> Config {
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    let p = cfg.providers.get_mut("mock").unwrap();
    p.protocol = protocol.into();
    p.context_window.insert("m1".into(), 131_072);
    cfg
}

#[tokio::test]
async fn context_retry_does_not_resend_the_same_load_or_block_the_next_turn() {
    let _g = config_lock().lock().await;
    for in_stream in [false, true] {
        let d = test_tmpdir().unwrap();
        let error = ProviderError::new(ErrorKind::ContextLength, KNOWN_WINDOW_ERROR);
        let refusal = if in_stream { Reply::Events(vec![StreamEvent::Error(error)]) } else { Reply::Err(error) };
        let client = Scripted::new(vec![refusal, Reply::Events(bare(text("next turn")))]);
        let mut r = start(client.clone(), context_retry_config("openai-chat"), None, d.path()).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Error);
        assert!(notices(&evs).iter().any(|s| s.contains("not sent again")), "{evs:?}");
        assert_eq!(evs.iter().filter(|e| matches!(e, AgentEvent::AssistantStart)).count(), 1);
        assert_eq!(client.requests().len(), 1, "the numbered window was already known");
        r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("next turn", TurnEnd::Done));
        assert_eq!(client.requests().len(), 2);
        r.stop().await;
    }
}

#[tokio::test]
async fn context_retry_after_billing_checks_the_effective_output_limit() {
    let _g = config_lock().lock().await;
    for omits_limit in [false, true] {
        let d = test_tmpdir().unwrap();
        let mut cfg = context_retry_config("openai-responses");
        cfg.agent.max_cost = Some(0.007);
        let p = cfg.providers.get_mut("mock").unwrap();
        p.extra.insert("omit_max_tokens".into(), omits_limit.into());
        // Only the reply has a price: the failed call consumes $0.0025, so the next spend admission lowers
        // max_tokens even though the window refit does not change the prompt or preferred reply size.
        p.pricing.insert("m1".into(), Pricing { output: 1.0, ..Pricing::default() });
        let client = Scripted::new(vec![
            Reply::Events(vec![
                StreamEvent::Usage(Usage { output: 2500, ..Default::default() }),
                StreamEvent::Error(ProviderError::new(ErrorKind::ContextLength, KNOWN_WINDOW_ERROR)),
            ]),
            Reply::Events(bare(text("fits now"))),
        ]);
        let mut r = start(client.clone(), cfg, None, d.path()).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
        let evs = r.turn().await;
        let reqs = client.requests();
        if omits_limit {
            assert_eq!(end_of(&evs), TurnEnd::Error);
            assert_eq!(reqs.len(), 1, "a smaller unsent output limit changes nothing for the provider");
        } else {
            assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("fits now", TurnEnd::Done));
            assert_eq!(reqs.len(), 2);
            assert_eq!(reqs[0].messages, reqs[1].messages);
            assert!(reqs[1].max_tokens < reqs[0].max_tokens, "the provider can use a genuinely smaller reply");
        }
        r.stop().await;
    }
}

#[tokio::test]
async fn the_same_window_named_again_gets_a_smaller_curated_view_and_no_summary() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let said = "This model's maximum context length is 40000 tokens. However, you requested 45000 tokens.";
    let mut script = answers(6);
    script.extend([
        too_long(said),
        Reply::Events(bare(text("after"))),
        too_long(said),
        Reply::Events(bare(text("third"))),
    ]);
    let client = Scripted::new(script);
    let mut r = fat_session(&client, None, 6, d.path()).await;
    r.send(AgentCommand::Submit { text: fat_message(6), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    assert_eq!(client.requests().len(), 8);
    // the window is the same on the second error: the view still gets smaller
    r.send(AgentCommand::Submit { text: fat_message(7), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "third");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(!notices(&evs).iter().any(|n| n.starts_with("history compacted")), "{evs:?}");
    assert!(no_summary(&client), "no summary request");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 10);
    assert!(estimate(&reqs[9]) < estimate(&reqs[8]), "{} then {}", estimate(&reqs[8]), estimate(&reqs[9]));
    let shape: Vec<String> = reqs[9]
        .messages
        .iter()
        .map(|m| {
            let t = m.text();
            let at = |i: usize| t.contains(&format!("w{i}x0 "));
            format!(
                "{:?} {} chars, starts {}, has the start of 6: {}, of 7: {}",
                m.role,
                t.len(),
                t.chars().take(50).collect::<String>(),
                at(6),
                at(7)
            ) + &format!(
                "\n  lines in [..]: {:?}\n  last 120: {:?}",
                t.lines()
                    .filter(|l| l.starts_with('['))
                    .map(|l| l.chars().take(90).collect::<String>())
                    .collect::<Vec<_>>(),
                t.chars().skip(t.chars().count().saturating_sub(120)).collect::<String>()
            )
        })
        .collect();
    // The second error names a count three times the estimate of the smaller view, so the second view is smaller than
    // the person's newest message (about 3500 tokens). Their message stays the last one, with its time note, and what
    // does not fit is named as left out with the way to get it (the curator's cut of a text that does not fit the window,
    // never a summary): it is whole or it says what was left.
    let last = reqs[9].messages.last().expect("a message").text();
    assert!(
        last.contains("after your previous message]")
            && (last.contains(&fat_message(7)) || last.contains("left out here")),
        "the person's newest message is the last of the view, whole or with its cut named; the last request had:\n{}",
        shape.join("\n")
    );
    r.stop().await;
}

#[tokio::test]
async fn an_error_that_names_the_requests_tokens_makes_the_next_view_fit_them() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let said = 60_000u64;
    let mut script = answers(FAT_TURNS);
    script.extend([
        too_long(
            "This model's maximum context length is 40000 tokens. However, you requested 70000 tokens \
             (60000 in the messages, 10000 in the completion). Please reduce the length of the messages or completion.",
        ),
        Reply::Events(bare(text("after"))),
    ]);
    let client = Scripted::new(script);
    let mut r = fat_session(&client, Some(WINDOW), FAT_TURNS, d.path()).await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("after", TurnEnd::Done));
    assert!(no_summary(&client), "no summary request");
    let reqs = client.requests();
    assert_eq!(reqs.len(), FAT_TURNS + 2);
    let (before, after) = (estimate(&reqs[FAT_TURNS]), estimate(&reqs[FAT_TURNS + 1]));
    // the provider's count of the retried request is under the window
    assert!(after < before && (after * said) / before < u64::from(WINDOW), "{before} then {after}");
    // an error that gives numbers sets no bound of its own
    assert!(!notices(&evs).iter().any(|n| n.contains("did not say its window")), "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn an_error_with_no_numbers_under_a_known_window_retries_under_three_quarters_of_the_refused_request() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut script = answers(FAT_TURNS);
    script.extend([too_long("context length exceeded"), Reply::Events(bare(text("after")))]);
    let client = Scripted::new(script);
    let mut r = fat_session(&client, Some(WINDOW), FAT_TURNS, d.path()).await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("after", TurnEnd::Done));
    assert!(no_summary(&client), "no summary request");
    let reqs = client.requests();
    assert_eq!(reqs.len(), FAT_TURNS + 2);
    let (before, after) = (estimate(&reqs[FAT_TURNS]), estimate(&reqs[FAT_TURNS + 1]));
    // an error with no number teaches no ratio: the retry stays under three quarters of the request refused
    assert!(under_three_quarters(after, before), "{before} then {after}");
    let told = notices(&evs).into_iter().filter(|n| n.contains("did not say its window")).count();
    assert_eq!(told, 1, "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn the_bound_from_a_refusal_with_no_number_keeps_the_next_turn_under_three_quarters() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut script = answers(FAT_TURNS);
    script.extend([
        too_long("context length exceeded"),
        // the answers after the refusal carry usage (3000 tokens for a request of over 10 000 estimated): the ratio
        // learned from them must not lift the bound
        Reply::Events(text("after")),
        Reply::Events(text("next")),
    ]);
    let client = Scripted::new(script);
    let mut r = fat_session(&client, Some(WINDOW), FAT_TURNS, d.path()).await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("after", TurnEnd::Done));
    r.send(AgentCommand::Submit { text: fat_message(FAT_TURNS), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("next", TurnEnd::Done));
    assert!(no_summary(&client), "no summary request");
    let reqs = client.requests();
    assert_eq!(reqs.len(), FAT_TURNS + 3);
    let (refused, next) = (estimate(&reqs[FAT_TURNS]), estimate(&reqs[FAT_TURNS + 2]));
    assert!(under_three_quarters(next, refused), "{refused} then {next}");
    r.stop().await;
}

#[tokio::test]
async fn a_request_not_smaller_than_one_refused_is_not_sent() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![too_long("context length exceeded"), Reply::Events(bare(text("never asked")))]);
    // a known window over 32k: a guessed one under it would shorten the prompt and the tools and let a smaller
    // retry go
    let mut r = fat_session(&client, Some(WINDOW), 0, d.path()).await;
    r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    // the system prompt and the tools alone pass three quarters of the request refused: the retry is not sent
    assert_eq!(client.requests().len(), 1);
    let told: Vec<String> =
        notices(&evs).into_iter().filter(|n| n.contains("/model") && n.contains("was not sent")).collect();
    assert_eq!(told.len(), 1, "{:?}", notices(&evs));
    assert!(!told[0].contains("/compact") && !told[0].contains("/clear"), "{}", told[0]);
    assert!(told[0].contains("was not sent") && told[0].contains("smallest refused"), "{}", told[0]);
    assert_eq!(notices(&evs).iter().filter(|n| n.contains("did not say its window")).count(), 1);
    assert!(no_summary(&client), "no summary request");
    r.stop().await;
}

#[tokio::test]
async fn a_bedrock_refusal_with_no_number_gets_a_smaller_view() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut script = answers(6);
    script.extend([
        // AWS Bedrock's whole message: no window, no size
        Reply::Err(ProviderError::from_http(400, r#"{"message":"Input is too long for requested model."}"#)),
        Reply::Events(bare(text("after"))),
    ]);
    let client = Scripted::new(script);
    let mut r = fat_session(&client, None, 6, d.path()).await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("after", TurnEnd::Done));
    assert!(no_summary(&client), "no summary request");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 8);
    let (refused, retry) = (estimate(&reqs[6]), estimate(&reqs[7]));
    assert!(under_three_quarters(retry, refused), "{refused} then {retry}");
    r.stop().await;
}

#[tokio::test]
async fn with_no_window_known_and_no_numbers_the_next_view_is_three_quarters_and_no_file_is_written() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut script = answers(6);
    script.extend([too_long("context length exceeded"), Reply::Events(bare(text("after")))]);
    let client = Scripted::new(script);
    let mut r = fat_session(&client, None, 6, d.path()).await;
    let file = data_dir().join("config.toml");
    let saved = std::fs::read_to_string(&file).ok();
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!((text_of(&evs).as_str(), end_of(&evs)), ("after", TurnEnd::Done));
    assert!(no_summary(&client), "no summary request");
    let reqs = client.requests();
    let (before, after) = (estimate(&reqs[6]), estimate(&reqs[7]));
    assert!(after * 4 <= before * 3, "{before} then {after}");
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::ModelLimits { source, .. } if source == "estimate")),
        "the window guessed is told to the interface"
    );
    assert_eq!(std::fs::read_to_string(&file).ok(), saved, "the person's config file is not touched");
    r.stop().await;
}

#[tokio::test]
async fn four_context_errors_in_a_row_end_the_turn_with_a_notice_and_no_summary() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut script = answers(FAT_TURNS);
    script.extend([
        too_long("context length exceeded"),
        // the error inside the stream, before any word, is read the same way
        Reply::Events(vec![StreamEvent::Error(ProviderError::new(
            ErrorKind::ContextLength,
            "context length exceeded",
        ))]),
        too_long("context length exceeded"),
        Reply::Events(vec![StreamEvent::Error(ProviderError::new(
            ErrorKind::ContextLength,
            "context length exceeded",
        ))]),
        Reply::Events(bare(text("never asked"))),
    ]);
    let client = Scripted::new(script);
    let mut r = fat_session(&client, Some(WINDOW), FAT_TURNS, d.path()).await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    let told: Vec<String> =
        notices(&evs).into_iter().filter(|n| n.contains("/compact") && n.contains("/clear")).collect();
    assert_eq!(told.len(), 1, "{:?}", notices(&evs));
    assert!(told[0].contains("3 smaller views"), "{}", told[0]);
    assert!(no_summary(&client), "no summary request");
    assert!(!notices(&evs).iter().any(|n| n.starts_with("history compacted")), "{evs:?}");
    // one request and three smaller views, none after
    assert_eq!(client.requests().len(), FAT_TURNS + 4);
    assert!(told[0].contains("smallest refused"), "{}", told[0]);
    // once a turn
    assert_eq!(notices(&evs).iter().filter(|n| n.contains("did not say its window")).count(), 1);
    let reqs = client.requests();
    // every view sent after a refusal is smaller than the request refused before it
    for pair in reqs[FAT_TURNS..].windows(2) {
        let (refused, next) = (estimate(&pair[0]), estimate(&pair[1]));
        assert!(next < refused, "{refused} then {next}");
    }
    r.stop().await;
}

#[tokio::test]
async fn a_throttle_that_reads_as_a_context_error_changes_nothing_and_asks_for_no_summary() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut script = answers(FAT_TURNS);
    script.push(too_long(
        "Request too large for model `m1` in organization `org` on tokens per minute (TPM): Limit 6000, Requested 9000, \
         please reduce your message size",
    ));
    script.extend(answers(1));
    let client = Scripted::new(script);
    let mut r = fat_session(&client, Some(WINDOW), FAT_TURNS, d.path()).await;
    r.send(AgentCommand::Submit { text: fat_message(FAT_TURNS), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(no_summary(&client), "no summary request");
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ModelLimits { source, .. } if source == "error")));
    r.send(AgentCommand::Submit { text: fat_message(FAT_TURNS + 1), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), FAT_TURNS + 2);
    // Curator selections can change between turns; request size alone does not prove that the ratio changed.
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ModelLimits { source, .. } if source == "error")));
    r.stop().await;
}

#[tokio::test]
async fn a_throttle_misclassified_as_context_does_not_compact_plain_history() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(bare(text("first"))),
        too_long("Request too large on tokens per minute (TPM): Limit 6000, Requested 9000"),
        Reply::Events(bare(text("next"))),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "first".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "second".into(), attachments: vec![] });
    assert_eq!(end_of(&r.turn().await), TurnEnd::Error);
    assert_eq!(client.requests().len(), 2);
    assert!(no_summary(&client));
    r.send(AgentCommand::Submit { text: "third".into(), attachments: vec![] });
    assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    r.stop().await;
}

#[tokio::test]
async fn each_call_reports_its_inventory_after_the_map_and_plain_clears_it() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    let evs = r.turn().await;
    let at = evs.iter().position(|e| matches!(e, AgentEvent::ContextMap { .. })).expect("a map");
    // a short conversation adds nothing, and the event still says so
    assert!(matches!(&evs[at + 1], AgentEvent::Curated { items, .. } if items.is_empty()), "{evs:?}");
    r.stop().await;

    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    let evs = r.turn().await;
    let at = evs.iter().position(|e| matches!(e, AgentEvent::ContextMap { .. })).expect("a map");
    // Plain must clear any inventory left by a previously selected curator policy.
    assert!(
        matches!(&evs[at + 1], AgentEvent::Curated { items, relation_quotes: None, quote_heat: None } if items.is_empty()),
        "{evs:?}"
    );
    r.stop().await;
}

#[tokio::test]
async fn an_unknown_model_is_reported_with_the_closest_names() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Err(
        ProviderError::new(ErrorKind::BadRequest, "The model `m3` does not exist or you do not have access to it.")
            .with_status(404),
    )]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetModel { provider: None, model: "m3".into() });
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("has no model `m3`") && text.contains("Closest names") && text.contains("m1"))),
        "{evs:?}"
    );
    let notices = evs.iter().filter(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("m3"))).count();
    assert_eq!(notices, 1, "one block for one failure: {evs:?}");
    assert_eq!(client.requests().len(), 1, "no retry");
    r.stop().await;
}

#[tokio::test]
async fn a_failed_request_comes_as_a_notice_with_a_problem_that_says_what_to_do() {
    for (status, body, title, step) in [
        (
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-proj-a1B2c3D4e5F6g7H8i9J0k1L2m3N4"}}"#,
            "rejected the key",
            "nexo auth login",
        ),
        (402, r#"{"error":{"message":"Insufficient Balance"}}"#, "has no credit left", "billing page"),
    ] {
        let d = test_tmpdir().unwrap();
        let client = Scripted::new(vec![Reply::Err(ProviderError::from_http(status, body))]);
        let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Error);
        let (text, p) = evs
            .iter()
            .find_map(|e| match e {
                AgentEvent::Notice { level: Level::Error, text, problem: Some(p) } => Some((text.clone(), p.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{status}: an error notice with a problem: {evs:?}"));
        assert!(p.title.contains(title), "{p:?}");
        assert!(p.steps[0].contains(step), "{p:?}");
        assert!(p.detail.as_deref().is_some_and(|d| d.contains(&status.to_string())), "{p:?}");
        assert_eq!(text, p.plain(), "the text stays the full plain form for readers that know no card");
        assert!(!text.contains("a1B2c3D4"), "no key in the text: {text}");
        r.stop().await;
    }
}

// ------------------------------------------------------------ goals and todo

#[tokio::test]
async fn a_model_changed_during_a_request_does_not_change_who_is_billed() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let tx = r.handle.commands.clone();
    client.replies.lock().unwrap().push_back(Reply::Midway(
        tx,
        Box::new(AgentCommand::SetModel { provider: None, model: "m2".into() }),
        text("answer from m1"),
    ));
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let cost =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { cost_usd, .. } = e { Some(*cost_usd) } else { None });
    assert!(cost.is_some_and(|c| c > 0.0), "m1 has a price: {cost:?}");
    r.stop().await;
}

#[tokio::test]
async fn the_usage_of_a_failed_attempt_is_still_billed() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(vec![
            StreamEvent::Usage(Usage { input: 5000, ..Default::default() }),
            StreamEvent::Error(ProviderError::new(
                ErrorKind::BadRequest,
                "max_tokens: 65536 > 8192, which is the maximum allowed number of output tokens for m1",
            )),
        ]),
        Reply::Events(text("fits now")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let session =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None });
    assert_eq!(session.map(|u| u.input), Some(6000), "the failed attempt's 5000 count too");
    r.stop().await;
}

#[tokio::test]
async fn a_large_window_gets_the_largest_reply_from_the_first_request() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    let p = cfg.providers.get_mut("mock").unwrap();
    p.context_window.insert("m1".into(), 1_048_576);
    p.max_output.insert("m1".into(), 32_768);
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests()[0].max_tokens, 32_768);
    r.stop().await;
}

#[tokio::test]
async fn a_model_the_server_does_not_list_is_flagged_at_the_switch() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    *client.listed.lock().unwrap() = vec!["m1".into(), "m2".into()];
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetModel { provider: None, model: "m3".into() });
    let evs = r
        .until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("does not list the model `m3`")))
        .await;
    assert!(
        evs.iter()
            .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("closest") && text.contains("m1"))),
        "{evs:?}"
    );
    r.stop().await;
}
