//! A small window: what nexo adds on its own (instruction files, a sub-agent's task and reply) is made to fit,
//! and the model is told what was cut.

use super::*;

/// The fixed part of the input estimate: UTF-8 bytes of system text and serialized tool schemas, divided by four.
fn fixed_tokens(r: &ModelRequest) -> u64 {
    (r.system.len() + serde_json::to_string(&r.tools).unwrap().len()) as u64 / 4
}

/// `messages` as they go out, as JSON, with the time of each person's message (`[16:13:05`, see `person_clock`)
/// replaced by `[<T>`: two runs of the same session type their messages in different seconds. Nothing else changes.
fn sent_without_clock<T: serde::Serialize + ?Sized>(messages: &T) -> String {
    let s = serde_json::to_string(messages).unwrap();
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let stamp = b.len() >= i + 9
            && b[i] == b'['
            && b[i + 3] == b':'
            && b[i + 6] == b':'
            && [1, 2, 4, 5, 7, 8].iter().all(|&k| b[i + k].is_ascii_digit());
        if stamp {
            out.push_str("[<T>");
            i += 9;
        } else {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// What the whole first request takes, and the reply it asks room for.
fn total_with_reply(r: &ModelRequest) -> u64 {
    fixed_tokens(r) + nexo_core::message::estimate_tokens(&r.messages) + u64::from(r.max_tokens)
}

fn cfg_window(w: u32) -> Config {
    let mut cfg = config(PermissionMode::Auto);
    cfg.providers.get_mut("mock").unwrap().context_window.insert("m1".into(), w);
    cfg.context.policy = "auto".into();
    cfg
}

fn project() -> tempfile::TempDir {
    let d = test_tmpdir().unwrap();
    // the walk for instruction files stops here
    std::fs::create_dir_all(d.path().join(".git")).unwrap();
    d
}

async fn first_request(cfg: Config, cwd: &std::path::Path) -> ModelRequest {
    let client = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut r = start_with(client.clone(), cfg, None, cwd, nexo_tools::builtin()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    client.requests()[0].clone()
}

fn agents_file(cwd: &std::path::Path, chars: usize) -> String {
    let body = "- Keep functions short and name tests as sentences; run the gate before landing.\n";
    let text = body.repeat(chars / body.len() + 1);
    std::fs::write(cwd.join("AGENTS.md"), &text).unwrap();
    text
}

#[tokio::test]
async fn a_40k_character_agents_file_at_8k_is_cut_to_fit_and_the_request_stays_in_the_window() {
    let _g = config_lock().lock().await;
    let d = project();
    agents_file(d.path(), 40_000);
    let req = first_request(cfg_window(8_000), d.path()).await;
    // the person's text takes at most a quarter of the window, in tokens
    let own = &req.system[req.system.find("\n\nInstructions from").unwrap()..];
    assert!(own.chars().count() / 4 <= 2_000, "the instructions take {} characters", own.chars().count());
    // the cut says what, how much and where the rest is
    let path = d.path().join("AGENTS.md");
    let line = own.lines().find(|l| l.starts_with("AGENTS.md cut to ")).unwrap_or_else(|| panic!("{own}"));
    assert!(line.contains("characters for a 8000-token window; read "), "{line}");
    assert!(line.ends_with(&format!("read {} for the rest", path.display())), "{line}");
    assert!(own.contains("- Keep functions short"), "the head stays");
    let total = total_with_reply(&req);
    assert!(total <= 8_000, "the first request and its reply take {total} tokens of 8000");
}

#[tokio::test]
async fn the_same_agents_file_at_200k_is_sent_whole_as_before() {
    let _g = config_lock().lock().await;
    let d = project();
    let text = agents_file(d.path(), 40_000);
    let req = first_request(cfg_window(200_000), d.path()).await;
    let path = d.path().join("AGENTS.md");
    assert!(
        req.system.contains(&format!("\n\nInstructions from {}:\n{}", path.display(), text.trim())),
        "the whole file is in the system prompt, framed as before"
    );
    assert!(!req.system.contains(" cut to "), "nothing is cut");
    // the same request with no window known (nothing is capped then) carries the same instruction text
    let mut none = cfg_window(200_000);
    none.providers.get_mut("mock").unwrap().context_window.clear();
    let unknown = first_request(none, d.path()).await;
    let from = |r: &ModelRequest| r.system[r.system.find("\n\nInstructions from").unwrap()..].to_string();
    assert_eq!(from(&req), from(&unknown));
}

/// The request a sub-agent on the model `s1` of another provider, whose window is `w`, sends first, and what
/// the main agent was told when it ended; the task is 24 300 characters.
async fn sub_agent_on_a_small_model(w: u32, max_output: Option<u32>) -> (ModelRequest, String, String) {
    let d = project();
    let task = "Find where the config is parsed and list every key it reads, with file and line. ".repeat(300);
    let client = Scripted::new(vec![
        Reply::Events(call(
            "t1",
            "task",
            json!({"prompt": task, "provider": "small", "model": "s1", "description": "look"}),
        )),
        Reply::Events(text("found it")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = cfg_window(200_000);
    let mut small = ProviderConfig::new("mock", "http://small", None);
    small.models = vec!["s1".into()];
    small.context_window.insert("s1".into(), w);
    if let Some(m) = max_output {
        small.max_output.insert("s1".into(), m);
    }
    cfg.providers.insert("small".into(), small);
    let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look with s1 on small".into(), attachments: vec![] });
    let evs = r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    let bases = client.bases.lock().unwrap().clone();
    let i = bases.iter().position(|b| b == "http://small").expect("the sub-agent asked its model");
    let result = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.content.clone()),
            _ => None,
        })
        .unwrap_or_default();
    (reqs[i].clone(), task, result)
}

#[tokio::test]
async fn a_sub_agent_on_a_4k_and_a_6k_window_asks_a_quarter_for_its_reply_and_keeps_a_cut_task_in_the_window() {
    let _g = config_lock().lock().await;
    for w in [4_096u32, 6_000] {
        let (req, task, result) = sub_agent_on_a_small_model(w, None).await;
        assert!(req.max_tokens <= w / 4, "{w}: the reply asked for {} tokens", req.max_tokens);
        let total = total_with_reply(&req);
        assert!(total <= u64::from(w), "{w}: the request and its reply take {total} tokens");
        let first = req.messages[0].text();
        let line = first
            .lines()
            .find_map(|l| l.strip_prefix("[nexo: task cut to ").and_then(|l| l.strip_suffix("]")))
            .unwrap_or_else(|| panic!("{w}: no cut line in {first:.300}"));
        let (kept, rest) = line.split_once(" of ").unwrap();
        let kept: usize = kept.parse().unwrap();
        assert_eq!(rest, format!("{} characters for a {w}-token window", task.len()), "{w}");
        // the head of the brief is what stays
        assert!(first.starts_with(&task[..200]) && first.contains(&task[..kept]), "{w}");
        assert!(!first.contains(&task[..kept + 100]), "{w}: only the head is kept");
        // the main agent sees the same fact in the job result
        assert!(
            result.contains(&format!("task cut to {kept} of {} characters for a {w}-token window", task.len())),
            "{w}: {result}"
        );
    }
}

#[tokio::test]
async fn a_sub_agent_on_a_large_window_gets_its_whole_task_and_the_usual_reply() {
    let _g = config_lock().lock().await;
    let (req, task, result) = sub_agent_on_a_small_model(200_000, None).await;
    // the size the main agent asks for, as the person set it (the default here)
    assert_eq!(req.max_tokens, nexo_core::config::DEFAULT_MAX_TOKENS);
    let first = req.messages[0].text();
    assert!(first.contains(&task) && !first.contains("task cut to"), "the task is whole");
    assert!(!result.contains("task cut to"), "{result}");
}

#[tokio::test]
async fn a_sub_agent_asks_no_more_than_its_models_largest_reply() {
    let _g = config_lock().lock().await;
    let (req, _, _) = sub_agent_on_a_small_model(32_000, Some(1_000)).await;
    assert_eq!(req.max_tokens, 1_000);
}

/// The tool text of a request as the wire counts it: characters of the tools' JSON.
fn tools_chars(r: &ModelRequest) -> usize {
    serde_json::to_string(&r.tools).unwrap().chars().count()
}

#[tokio::test]
async fn a_sub_agent_on_an_8k_window_gets_the_compact_tool_text_the_main_agent_gets_there() {
    let _g = config_lock().lock().await;
    let (req, _, _) = sub_agent_on_a_small_model(8_000, None).await;
    eprintln!("sub-agent tools at 8k: {} tokens", tools_chars(&req) / 4);
    // the budget the main agent's compact tool text is held to at 8k (`prompt::size_budget`)
    assert!(tools_chars(&req) <= 6_000, "the sub-agent's tools take {} characters", tools_chars(&req));
    let names: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(!names.contains(&"web_search"), "a tool the window leaves out: {names:?}");
    assert!(names.contains(&"read") && names.contains(&"grep"), "{names:?}");
    let all = nexo_tools::builtin();
    for t in &req.tools {
        let compact = all.compact_specs().into_iter().find(|s| s.name == t.name);
        // the session adds a few tools of its own (shared memory); the builtin ones are the compact text
        if let Some(c) = compact {
            assert_eq!(&c, t, "{} is not the compact text", t.name);
        }
    }
}

#[tokio::test]
async fn a_sub_agent_on_a_large_window_gets_the_full_tool_text_as_before() {
    let _g = config_lock().lock().await;
    let (req, _, _) = sub_agent_on_a_small_model(200_000, None).await;
    let names: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"web_search"), "{names:?}");
    let all = nexo_tools::builtin();
    for t in &req.tools {
        let full = all.specs_for(false).into_iter().find(|s| s.name == t.name);
        // the session adds a few tools of its own (shared memory); the builtin ones are the whole text
        if let Some(f) = full {
            // `recall` is whole in a sub-agent whatever the main agent is offered
            let f = if t.name == "recall" { nexo_core::tool::Tool::spec(&nexo_tools::recall::Recall) } else { f };
            assert_eq!(&f, t, "{} is not the full text", t.name);
        }
    }
}

/// A server with 30 tools, each with a long text and three parameters. A call answers `ran`; one whose arguments
/// carry `fail` is an error the tool reports.
#[cfg(unix)]
const BIG_SERVER: &str = r#"while IFS= read -r l; do
  id=$(printf '%s' "$l" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$l" in
    *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"big"}}}\n' "$id" ;;
    *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":' "$id"; cat "$TOOLS"; printf '}\n' ;;
    *'"method":"tools/call"'*'fail'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":true,"content":[{"type":"text","text":"bad arguments"}]}}\n' "$id" ;;
    *'"method":"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ran"}]}}\n' "$id" ;;
  esac
done
"#;

/// A session under `cfg` with the 30-tool server, its model answering `replies`: the requests (and where each went)
/// and what the turn said.
#[cfg(unix)]
async fn with_big_server(cfg: Config, replies: Vec<Reply>) -> (Vec<ModelRequest>, Vec<String>, Vec<AgentEvent>) {
    let d = test_tmpdir().unwrap();
    let tool = |i: usize| {
        json!({
            "name": format!("tool_{i:02}"),
            "description": format!("Tool number {i} of the big server. It looks things up in the records the server keeps, by name, by date or by owner, and returns the matching rows with their fields in full."),
            "inputSchema": {"type": "object", "required": ["q"], "properties": {
                "q": {"type": "string", "description": "What to look for: a name, a date range or an owner, as the records keep it."},
                "limit": {"type": "integer", "description": "The most rows to return, 1 to 500."},
                "fail": {"type": "boolean", "description": "Make the call fail."}
            }}
        })
    };
    let list = json!({"tools": (0..30).map(tool).collect::<Vec<_>>()});
    std::fs::write(d.path().join("tools.json"), list.to_string()).unwrap();
    let mut servers = std::collections::BTreeMap::new();
    servers.insert(
        "big".to_string(),
        nexo_core::config::McpServerConfig {
            command: "sh".into(),
            args: vec!["-c".into(), BIG_SERVER.into()],
            env: [("TOOLS".to_string(), d.path().join("tools.json").display().to_string())].into(),
            ..Default::default()
        },
    );
    let hub = nexo_tools::mcp_hub::McpHub::start_in_background(&servers, &d.path().join("auth.json"), None);
    // the server is up before the session starts, so the sub-agents it starts have its tools too
    hub.ready().await;
    let mut tools = nexo_tools::builtin();
    hub.sync_tools(&mut tools, &mut 0);
    data_dir();
    let (handle, ends) = channels();
    let client = Scripted::new(replies);
    let setup = AgentSetup {
        config: cfg,
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools,
        client: client.clone(),
        resume: None,
        gate: None,
        mail: None,
        mcp: Some(hub.clone()),
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut r = Running { handle, task };
    assert!(matches!(r.next().await, AgentEvent::Ready { .. }));
    r.send(AgentCommand::Submit { text: "look up the records".into(), attachments: vec![] });
    let evs = r.turn().await;
    r.stop().await;
    hub.close().await;
    let bases = client.bases.lock().unwrap().clone();
    (client.requests(), bases, evs)
}

/// What the main agent was told a call `id` of its own answered.
#[cfg(unix)]
fn done_output(evs: &[AgentEvent], id: &str) -> String {
    evs.iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id: i, output, .. } if i == id => Some(output.content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// What the 30-tool server's session sends on a window of `w` tokens: the first request, what its two calls to
/// the last tool (right arguments, then wrong ones) answered, and what the turn said.
#[cfg(unix)]
async fn big_mcp_session(w: u32) -> (ModelRequest, Vec<String>, Vec<AgentEvent>) {
    let replies = vec![
        Reply::Events(call("c1", "mcp__big__tool_29", json!({"q": "x"}))),
        Reply::Events(call("c2", "mcp__big__tool_29", json!({"fail": true}))),
        Reply::Events(text("done")),
    ];
    let (reqs, _, evs) = with_big_server(cfg_window(w), replies).await;
    let outputs = ["c1", "c2"].iter().map(|c| done_output(&evs, c)).collect();
    (reqs[0].clone(), outputs, evs)
}

/// The tokens the MCP tools of `r` take, as nexo counts them (four characters a token).
#[cfg(unix)]
fn mcp_tokens(r: &ModelRequest) -> u64 {
    r.tools
        .iter()
        .filter(|t| t.name.starts_with("mcp__"))
        .map(|t| serde_json::to_string(t).unwrap().chars().count() as u64 / 4)
        .sum()
}

#[cfg(unix)]
#[tokio::test]
async fn thirty_mcp_tools_on_an_8k_window_stay_in_their_share_are_named_and_still_run() {
    let _g = config_lock().lock().await;
    let (req, outputs, _) = big_mcp_session(8_000).await;
    let tokens = mcp_tokens(&req);
    eprintln!("30 MCP tools at 8k: {tokens} tokens");
    assert_eq!(
        req.tools.iter().filter(|t| t.name.starts_with("mcp__big__")).count(),
        30,
        "every tool is still offered"
    );
    assert!(tokens <= nexo_core::tool::MCP_SMALL_TOKENS, "the MCP tools take {tokens} tokens");
    // the fact the model gets: the server, the tools, and the numbers
    let short: Vec<&str> =
        req.tools.iter().filter(|t| t.parameters == json!({"type": "object"})).map(|t| t.name.as_str()).collect();
    assert!(short.len() > 20, "{short:?}");
    let note = req.system.lines().find(|l| l.starts_with("MCP tools: ")).expect("no notice in the system text");
    assert!(note.contains(&format!("{} of 30", short.len())) && note.contains("share of 1000"), "{note}");
    assert!(req.system.contains(&format!("- big: {}", short[0])), "{}", req.system);
    // a shortened tool runs; wrong arguments bring its whole schema back
    assert_eq!(outputs[0], "ran");
    assert!(outputs[1].contains("bad arguments") && outputs[1].contains("\"required\":[\"q\"]"), "{}", outputs[1]);
}

#[cfg(unix)]
#[tokio::test]
async fn thirty_mcp_tools_on_a_large_window_are_offered_whole_as_before() {
    let _g = config_lock().lock().await;
    let (req, outputs, _) = big_mcp_session(200_000).await;
    let mcp: Vec<_> = req.tools.iter().filter(|t| t.name.starts_with("mcp__big__")).collect();
    assert_eq!(mcp.len(), 30);
    eprintln!("30 MCP tools at 200k: {} tokens", mcp_tokens(&req));
    assert!(mcp.iter().all(|t| t.description.len() > 100 && t.parameters["properties"]["q"].is_object()));
    assert!(!req.system.contains("MCP tools:"));
    assert_eq!(outputs[0], "ran");
    assert!(!outputs[1].contains("whole input schema"), "{}", outputs[1]);
}

/// A scripted reply with no usage: a local server that counts nothing, so nexo goes by its own estimate alone.
fn uncounted(events: Vec<StreamEvent>) -> Vec<StreamEvent> {
    events.into_iter().filter(|e| !matches!(e, StreamEvent::Usage(_))).collect()
}

/// The requests of a session under `policy` at window `w` (none when `None`) that reads a 64 000-character file
/// and then answers, and what the turn said.
async fn a_big_read(w: Option<u32>, policy: &str) -> (Vec<ModelRequest>, Vec<AgentEvent>) {
    let d = project();
    let body: String = (0..1_400).map(|i| format!("fn item_{i:05}(x: u32) -> u32 {{ x + {i} }} // line\n")).collect();
    std::fs::write(d.path().join("big.rs"), &body).unwrap();
    let client = Scripted::new(vec![
        Reply::Events(uncounted(call("c1", "read", json!({"path": "big.rs"})))),
        Reply::Events(uncounted(text("read it"))),
    ]);
    let mut cfg = cfg_window(w.unwrap_or(200_000));
    if w.is_none() {
        cfg.providers.get_mut("mock").unwrap().context_window.clear();
    }
    cfg.context.policy = policy.into();
    let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look at the file big.rs".into(), attachments: vec![] });
    let evs = r.turn().await;
    r.stop().await;
    (client.requests(), evs)
}

/// Check the request captured at the provider boundary, not the size of the Curator view reported to the UI.
fn assert_fits(r: &ModelRequest, window: u32, ratio: f64) {
    let estimate = fixed_tokens(r) + nexo_core::message::estimate_tokens(&r.messages);
    let input = (estimate as f64 * ratio).ceil() as u64;
    assert!(input + u64::from(r.max_tokens) <= u64::from(window), "{input} input + {} reply > {window}", r.max_tokens);
}

#[tokio::test]
async fn a_64k_read_under_the_plain_policy_at_8k_is_sent_from_the_curators_view_and_fits_with_its_reply() {
    let _g = config_lock().lock().await;
    let (reqs, evs) = a_big_read(Some(8_000), "plain").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(reqs.len(), 2, "the request after the read actually went to the model");
    for req in &reqs {
        assert_fits(req, 8_000, 4.0 / 3.0);
    }
    let last = &reqs[1];
    assert!(last.max_tokens >= 256, "the reply keeps room for a call: {}", last.max_tokens);
    assert!(last.messages.iter().any(|m| m.text().contains("look at the file big.rs")), "the person's words stay");
}

#[tokio::test]
async fn an_impossible_message_never_reaches_the_provider_or_blocks_the_next_turn_in_the_same_session() {
    let _g = config_lock().lock().await;
    let words = "0123456789".repeat(2_220);
    let message_tokens = nexo_core::message::message_tokens(&Message::user(&words));
    // Before the first provider count, the words alone leave a useful reply. Even omitting ALL older
    // messages, they cannot be sent whole once the system prompt and tools are counted as well.
    let cold = 4.0 / 3.0;
    assert!((message_tokens as f64 * cold).ceil() as u64 + 256 <= 8_000);
    for policy in ["plain", "curator"] {
        let d = project();
        let client =
            Scripted::new(vec![Reply::Events(uncounted(text("first"))), Reply::Events(uncounted(text("pong")))]);
        let mut cfg = cfg_window(8_000);
        cfg.context.policy = policy.into();
        let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
        let AgentEvent::Ready { session_id: id, .. } = r.next().await else { panic!("no Ready") };
        r.send(AgentCommand::Submit { text: "first question".into(), attachments: vec![] });
        assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
        let first = client.requests();
        assert_eq!(first.len(), 1);
        assert_fits(&first[0], 8_000, cold);
        assert!(((fixed_tokens(&first[0]) + message_tokens) as f64 * cold).ceil() as u64 > 8_000);
        let history = || nexo_agent::Session::open(&id).unwrap().messages().to_vec();
        let prefix = history();
        assert_eq!(prefix.len(), 2);

        r.send(AgentCommand::Submit { text: words.clone(), attachments: vec![] });
        let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.expect("the blocked turn ends");
        assert_eq!(client.requests().len(), 1, "{policy}: no model, summary or retry for an impossible request");
        assert_eq!(end_of(&evs), TurnEnd::Error, "{policy}");
        let told = notices(&evs);
        let sizes: Vec<_> = told.iter().filter(|n| n.contains("8000") && n.contains("window")).collect();
        assert_eq!(sizes.len(), 1, "{policy}: {told:?}");
        let n = sizes[0];
        for part in ["system", "tools", "conversation", "reply"] {
            assert!(n.contains(part), "{policy}: missing {part} in {n}");
        }
        assert!(!n.contains("Sent as it is"), "{n}");
        let blocked = history();
        assert!(blocked.starts_with(&prefix), "{policy}: previous turns changed");
        assert_eq!(nexo_core::agent::person_words(&blocked[prefix.len()].text()), words);

        // No reset, resume, rewind or manual compaction between submits.
        r.send(AgentCommand::Submit { text: "ping".into(), attachments: vec![] });
        let next = r.turn().await;
        assert_eq!((end_of(&next), text_of(&next).as_str()), (TurnEnd::Done, "pong"), "{policy}");
        let reqs = client.requests();
        assert_eq!(reqs.len(), 2, "{policy}: exactly one new provider call");
        assert_eq!(reqs[1].session_id.as_deref(), Some(id.as_str()));
        assert_fits(&reqs[1], 8_000, cold);
        assert!(reqs[1].messages.iter().any(|m| m.is_person() && nexo_core::agent::person_words(&m.text()) == "ping"));
        r.stop().await;
        let after = history();
        assert!(after.starts_with(&blocked), "{policy}: making room changed the saved conversation");
        assert_eq!(nexo_core::agent::person_words(&after[blocked.len()].text()), "ping");
        assert_eq!(after.last().unwrap().text(), "pong");
    }
}

#[tokio::test]
async fn curator_fits_a_batch_of_24_large_results_before_asking_the_model_again() {
    const WINDOW: u32 = 20_000;
    const COUNTED_INPUT: u64 = 8_000;
    let _g = config_lock().lock().await;
    let d = project();
    let mut batch = Vec::new();
    for i in 0..24 {
        let path = format!("section-{i:02}.txt");
        // Distinct paths AND bodies: deduplicating repeat reads would hide the overflow.
        let body: String =
            (0..240).map(|j| format!("section-{i:02}, entry {j}: recorded value {}\n", i * 240 + j)).collect();
        std::fs::write(d.path().join(&path), body).unwrap();
        let id = format!("r{i}");
        batch.push(StreamEvent::ToolCallStart { id: id.clone(), name: "read".into() });
        batch.push(StreamEvent::ToolCallEnd { id, name: "read".into(), args: json!({"path":path}) });
    }
    batch.extend([
        StreamEvent::Usage(Usage { input: COUNTED_INPUT, output: 900, ..Default::default() }),
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]);
    let client = Scripted::new(vec![Reply::Events(batch), Reply::Events(uncounted(text("checked all sections")))]);
    let mut cfg = cfg_window(WINDOW);
    cfg.context.policy = "curator".into();
    cfg.providers.get_mut("mock").unwrap().max_output.insert("m1".into(), 1_024);
    let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
    let AgentEvent::Ready { session_id: id, .. } = r.next().await else { panic!("no Ready") };
    r.send(AgentCommand::Submit { text: "check the sections".into(), attachments: vec![] });
    let evs = r.turn().await;
    r.stop().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Context { policy, .. } if policy == "curator")));
    let reqs = client.requests();
    let first = &reqs[0];
    assert_fits(first, WINDOW, 4.0 / 3.0);
    // The first (small) request is sent whole. Its provider usage is the only calibration sample.
    let est = fixed_tokens(first) + nexo_core::message::estimate_tokens(&first.messages);
    assert!(est >= 500);
    let ratio = (COUNTED_INPUT as f64 / est as f64).clamp(0.5, 3.0);
    assert!(ratio > 4.0 / 3.0, "the calibration must matter here: {ratio}");
    for next in &reqs[1..] {
        assert_fits(next, WINDOW, ratio);
    }
    assert_eq!((end_of(&evs), text_of(&evs).as_str()), (TurnEnd::Done, "checked all sections"));
    assert_eq!(reqs.len(), 2, "the batch was fitted without another user turn or a provider retry");
    let next = &reqs[1].messages;
    assert!(nexo_core::message::is_well_formed(next));
    assert!(next.iter().any(Message::is_tool_results), "the continuation must see tool results");
    assert!(next.iter().any(|m| m.is_person() && m.text().contains("check the sections")));

    let saved = nexo_agent::Session::open(&id).unwrap();
    let results = saved.messages().iter().find(|m| m.is_tool_results()).expect("saved batch");
    assert_eq!(results.content.len(), 24, "one batch, not 24 rounds");
    for (i, part) in results.content.iter().enumerate() {
        let Part::ToolResult { call_id, content, is_error: false, .. } = part else {
            panic!("result {i} missing or failed")
        };
        assert_eq!(call_id, &format!("r{i}"));
        let output = tool_answer(&evs, call_id);
        assert!(output.contains(&format!("section-{i:02}, entry 0")) && output.len() > 1_500);
        assert_eq!(nexo_core::message::result_text(content), output, "{call_id}: only the outbound view may be cut");
    }
    let full = nexo_core::message::message_tokens(results);
    assert!((full as f64 * ratio).ceil() as u64 > u64::from(WINDOW), "without fitting, the results alone overflow");
}

#[tokio::test]
async fn the_same_plain_session_at_200k_sends_the_requests_it_sent_before() {
    let _g = config_lock().lock().await;
    let (at_200k, evs) = a_big_read(Some(200_000), "plain").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(at_200k.len(), 2);
    // with no window known nothing is fitted: the same session at 200k sends the same messages and reply size
    let (unknown, _) = a_big_read(None, "plain").await;
    assert_eq!(at_200k.len(), unknown.len());
    for (a, b) in at_200k.iter().zip(&unknown) {
        assert_eq!(sent_without_clock(&a.messages), sent_without_clock(&b.messages));
        assert_eq!(a.max_tokens, b.max_tokens);
    }
}

/// A read that answers as many characters of code as it holds, whatever the window: a tool with no cap of its own.
struct Fill(usize);

#[async_trait]
impl Tool for Fill {
    fn name(&self) -> &str {
        "read"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "read".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _args: &Value) -> String {
        "read".into()
    }
    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        let body: String =
            (0..).map(|i| format!("fn item_{i:05}(x: u32) -> u32 {{ x + {i} }}\n")).take(self.0 / 36 + 1).collect();
        ToolOutput::ok(body.chars().take(self.0).collect::<String>())
    }
}

/// The requests of a plain session at window `w` (none when `None`) whose read answers `chars` characters, and
/// what the request after the read takes by nexo's own estimate, without its reply.
async fn a_plain_read_of(chars: usize, w: Option<u32>) -> (Vec<ModelRequest>, u64) {
    let d = project();
    let client = Scripted::new(vec![
        Reply::Events(uncounted(call("c1", "read", json!({"path": "lib.rs"})))),
        Reply::Events(uncounted(text("read it"))),
    ]);
    let mut cfg = cfg_window(w.unwrap_or(200_000));
    if w.is_none() {
        cfg.providers.get_mut("mock").unwrap().context_window.clear();
    }
    cfg.context.policy = "plain".into();
    let tools = ToolSet::new().with(Arc::new(Fill(chars)));
    let mut r = start_with(client.clone(), cfg, None, d.path(), tools).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look at lib.rs".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    let last = reqs.last().unwrap();
    let est = total_with_reply(last) - u64::from(last.max_tokens);
    (reqs, est)
}

/// The characters of read result that leave the request after the read at about `tokens` estimated tokens in a
/// session at 8k, and that session with no window known (where nothing is fitted). Measured with no window known,
/// less what the system prompt and tools at 8k leave out.
async fn read_for(tokens: f64) -> (usize, Vec<ModelRequest>, u64) {
    let (open, at_4k) = a_plain_read_of(4_000, None).await;
    let (_, at_8k) = a_plain_read_of(8_000, None).await;
    let (small, _) = a_plain_read_of(10, Some(8_000)).await;
    let fixed_less = fixed_tokens(&open[0]) - fixed_tokens(&small[0]);
    let per_char = (at_8k - at_4k) as f64 / 4_000.0;
    let chars = 4_000 + ((tokens + fixed_less as f64 - at_4k as f64) / per_char) as usize;
    let (whole, est) = a_plain_read_of(chars, None).await;
    (chars, whole, est - fixed_less)
}

/// The provider's count of `est` estimated tokens of code, at about 3 characters a token.
fn as_code(est: u64) -> u64 {
    (est as f64 * 1.33).ceil() as u64
}

#[tokio::test]
async fn a_plain_session_at_8k_with_room_for_a_short_reply_only_goes_from_the_curators_view_with_a_useful_reply() {
    let _g = config_lock().lock().await;
    // a request of code at about 7 350 of 8 000 tokens before the provider has counted any: room for a 300-token
    // reply, none for 1 000
    let (chars, whole, est) = read_for(7_350.0 / 1.33).await;
    assert!(as_code(est) > 7_000 && as_code(est) + 300 <= 8_000, "the request takes {} tokens at 8k", as_code(est));
    let (reqs, _) = a_plain_read_of(chars, Some(8_000)).await;
    let last = reqs.last().unwrap();
    assert!(last.max_tokens >= 1_000, "the reply has room for an edit: {}", last.max_tokens);
    let total = as_code(total_with_reply(last) - u64::from(last.max_tokens)) + u64::from(last.max_tokens);
    assert!(total <= 8_000, "the request and its reply take {total} tokens of 8000");
    let size = |r: &ModelRequest| serde_json::to_string(&r.messages).unwrap().len();
    assert!(size(last) < size(whole.last().unwrap()), "the curator's view is shorter than the whole read");
    assert!(last.messages.iter().any(|m| m.text().contains("look at lib.rs")), "the person's words stay");
    // at 200k the same session sends what it sends with no window known
    let (at_200k, _) = a_plain_read_of(chars, Some(200_000)).await;
    assert_eq!(at_200k.len(), whole.len());
    for (a, b) in at_200k.iter().zip(&whole) {
        assert_eq!(sent_without_clock(&a.messages), sent_without_clock(&b.messages));
        assert_eq!(a.max_tokens, b.max_tokens);
    }
}

/// The person notices of `evs` that say which MCP tools went out by name.
#[cfg(unix)]
fn mcp_notices(evs: &[AgentEvent]) -> Vec<String> {
    notices(evs).into_iter().filter(|n| n.starts_with("MCP tools: ")).collect()
}

#[cfg(unix)]
#[tokio::test]
async fn thirty_mcp_tools_on_a_16k_window_take_an_eighth_of_it_and_the_person_is_told_once() {
    let _g = config_lock().lock().await;
    let (req, outputs, evs) = big_mcp_session(16_000).await;
    let tokens = mcp_tokens(&req);
    eprintln!("30 MCP tools at 16k: {tokens} tokens");
    // the share is the window's eighth, 2 000 tokens, so more of them go whole than at 8k
    assert!(tokens <= 2_000 && tokens > nexo_core::tool::MCP_SMALL_TOKENS, "the MCP tools take {tokens} tokens");
    let note = req.system.lines().find(|l| l.starts_with("MCP tools: ")).expect("no note in the system text");
    assert!(note.contains("for a 16000-token window") && note.contains("share of 2000"), "{note}");
    // the person reads the same numbers once, though the turn sent three requests
    let told = mcp_notices(&evs);
    assert_eq!(told.len(), 1, "{:?}", notices(&evs));
    assert!(told[0].starts_with(note), "{}", told[0]);
    assert!(outputs[1].contains("\"required\":[\"q\"]"), "{}", outputs[1]);
}

#[cfg(unix)]
#[tokio::test]
async fn the_person_is_not_told_of_mcp_tools_on_a_large_window() {
    let _g = config_lock().lock().await;
    let (_, _, evs) = big_mcp_session(200_000).await;
    assert!(mcp_notices(&evs).is_empty(), "{:?}", notices(&evs));
}

#[cfg(unix)]
#[tokio::test]
async fn a_main_agent_at_200k_and_its_sub_agent_at_8k_on_one_mcp_server_each_get_the_schema_by_what_they_were_offered()
{
    let _g = config_lock().lock().await;
    let mut cfg = cfg_window(200_000);
    let mut small = ProviderConfig::new("mock", "http://small", None);
    small.models = vec!["s1".into()];
    small.context_window.insert("s1".into(), 8_000);
    cfg.providers.insert("small".into(), small);
    let replies = vec![
        Reply::Events(call(
            "t1",
            "task",
            json!({"prompt": "look", "provider": "small", "model": "s1", "description": "look", "agent": "general"}),
        )),
        Reply::Events(call("s1", "mcp__big__tool_29", json!({"fail": true}))),
        Reply::Events(text("found it")),
        // the main agent calls the same tool wrongly after its sub-agent was offered it short
        Reply::Events(call("c2", "mcp__big__tool_29", json!({"fail": true}))),
        Reply::Events(text("done")),
    ];
    let (reqs, bases, evs) = with_big_server(cfg, replies).await;
    let sub: Vec<_> = bases.iter().zip(&reqs).filter(|(b, _)| *b == "http://small").map(|(_, r)| r).collect();
    assert_eq!(sub.len(), 2, "{bases:?}");
    let offered = sub[0]
        .tools
        .iter()
        .find(|t| t.name == "mcp__big__tool_29")
        .unwrap_or_else(|| panic!("{:?}", sub[0].tools.iter().map(|t| &t.name).collect::<Vec<_>>()));
    assert_eq!(offered.parameters, json!({"type": "object"}), "the sub-agent at 8k has it by name");
    // the sub-agent's failed call brings the whole schema back to it
    let answer = serde_json::to_string(sub[1].messages.last().unwrap()).unwrap();
    assert!(answer.contains("bad arguments") && answer.contains("whole input schema"), "{answer}");
    // the main agent was offered the tool whole and its failed call says only what failed
    let main = reqs.last().unwrap();
    let whole = main.tools.iter().find(|t| t.name == "mcp__big__tool_29").unwrap();
    assert!(whole.parameters["properties"]["q"].is_object());
    let c2 = done_output(&evs, "c2");
    assert!(c2.contains("bad arguments") && !c2.contains("whole input schema"), "{c2}");
    assert!(mcp_notices(&evs).is_empty(), "the main agent's own tools were not shortened: {:?}", notices(&evs));
}

#[tokio::test]
async fn a_first_request_of_code_at_8k_fits_the_window_at_three_characters_a_token() {
    let _g = config_lock().lock().await;
    // a request that fits at four characters a token with its whole reply, and passes the window at three
    let (chars, _, est) = read_for(5_250.0).await;
    assert!(est + 2_000 <= 8_000 && as_code(est) + 2_000 > 8_000, "the request takes {est} tokens at 8k");
    // no reply carries a count, so every request goes before the first one
    let (reqs, _) = a_plain_read_of(chars, Some(8_000)).await;
    for (i, r) in reqs.iter().enumerate() {
        let reply = u64::from(r.max_tokens);
        let total = as_code(total_with_reply(r) - reply) + reply;
        assert!(total <= 8_000, "request {i} and its reply take {total} tokens of 8000");
        assert!(reply >= 1_000, "request {i} keeps a useful reply: {reply}");
    }
}

/// The characters one tool result may take at window `w` for the first request `r` of a session: the view
/// window (85% of what the reply leaves) less the fixed text, as the session works it out.
fn result_budget(r: &ModelRequest, w: u32) -> usize {
    let view = u64::from(w - r.max_tokens) * 17 / 20;
    view.saturating_sub(fixed_tokens(r)).max(2_000) as usize
}

/// What the tool call `id` answered in a turn.
fn tool_answer(evs: &[AgentEvent], id: &str) -> String {
    evs.iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id: i, output, .. } if i == id => Some(output.content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_66k_read_under_the_plain_policy_at_8k_comes_back_within_the_room_and_says_what_was_kept() {
    let _g = config_lock().lock().await;
    let (reqs, evs) = a_big_read(Some(8_000), "plain").await;
    let answer = tool_answer(&evs, "c1");
    let budget = result_budget(&reqs[0], 8_000);
    eprintln!("read at 8k: {} characters of a budget of {budget}", answer.chars().count());
    assert!(answer.chars().count() <= budget, "the read took {} of {budget} characters", answer.chars().count());
    assert!(answer.contains("item_00000"), "the head of the file is there");
    let line = answer.lines().last().unwrap();
    assert!(line.contains("characters for a 8000-token window; read with offset "), "{line}");
    assert!(line.ends_with("to continue"), "{line}");
    // the second request carries that result as it came
    let last = reqs.last().unwrap();
    assert!(total_with_reply(last) <= 8_000, "{}", total_with_reply(last));
}

#[tokio::test]
async fn a_64k_read_at_200k_comes_back_as_it_always_did() {
    let _g = config_lock().lock().await;
    let (_, evs) = a_big_read(Some(200_000), "plain").await;
    let (_, unknown) = a_big_read(None, "plain").await;
    let (a, b) = (tool_answer(&evs, "c1"), tool_answer(&unknown, "c1"));
    assert_eq!(a, b);
    assert!(!a.contains("-token window"), "{}", a.lines().last().unwrap());
}

/// The first request of a session at window `w` (none when `None`) whose prompt hook answers with `chars`
/// characters of context, under `policy`.
#[cfg(unix)]
async fn with_a_long_hook(w: Option<u32>, chars: usize, policy: &str) -> ModelRequest {
    let d = project();
    let mut cfg = cfg_window(w.unwrap_or(200_000));
    if w.is_none() {
        cfg.providers.get_mut("mock").unwrap().context_window.clear();
    }
    cfg.context.policy = policy.into();
    cfg.hooks.push(hook_cfg("UserPromptSubmit", None, &format!("head -c {chars} /dev/zero | tr '\\0' 'h'")));
    first_request(cfg, d.path()).await
}

#[cfg(unix)]
#[tokio::test]
async fn a_hook_that_answers_with_40k_characters_at_8k_is_cut_and_the_request_fits() {
    let _g = config_lock().lock().await;
    let req = with_a_long_hook(Some(8_000), 40_000, "plain").await;
    let sent = req.messages[0].text();
    let line = sent.lines().find(|l| l.starts_with("[hook context cut to ")).unwrap_or_else(|| panic!("{sent:.400}"));
    assert!(
        line.contains(" of 40000 characters for a 8000-token window; the rest of the hook's output is not kept]"),
        "{line}"
    );
    assert!(sent.contains(&"h".repeat(1_000)), "the head of the hook's text stays");
    let total = total_with_reply(&req);
    assert!(total <= 8_000, "the request and its reply take {total} tokens of 8000");
}

#[cfg(unix)]
#[tokio::test]
async fn the_same_hook_at_200k_is_sent_whole_as_before() {
    let _g = config_lock().lock().await;
    let at_200k = with_a_long_hook(Some(200_000), 40_000, "plain").await;
    let unknown = with_a_long_hook(None, 40_000, "plain").await;
    assert_eq!(sent_without_clock(&at_200k.messages), sent_without_clock(&unknown.messages));
    assert!(!at_200k.messages[0].text().contains("-token window"));
}

/// A project of `n` source files of about 1 600 characters each.
fn many_files(n: usize) -> tempfile::TempDir {
    let d = project();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    for i in 0..n {
        let body: String = (0..30).map(|k| format!("fn f{i}_{k}(x: u32) -> u32 {{ x + {k} }} // padding\n")).collect();
        std::fs::write(d.path().join(format!("src/m{i:02}.rs")), body).unwrap();
    }
    d
}

#[tokio::test]
async fn the_files_nexo_attaches_by_itself_at_8k_fit_what_the_window_leaves_and_at_200k_are_as_before() {
    let _g = config_lock().lock().await;
    let attached = |r: &ModelRequest| r.messages[0].text().matches("(project file, included in full by nexo").count();
    let d = many_files(13);
    let with_files = |w: u32| {
        let mut cfg = cfg_window(w);
        cfg.agent.attach_budget = 6_000;
        cfg
    };
    let small = first_request(with_files(8_000), d.path()).await;
    let total = total_with_reply(&small);
    assert!(total <= 8_000, "the request and its reply take {total} tokens of 8000");
    let big = first_request(with_files(200_000), d.path()).await;
    eprintln!("attached at 8k: {} files; at 200k: {}", attached(&small), attached(&big));
    assert!(attached(&small) < attached(&big), "the small window gets fewer files");
    // what is attached stays within what the window leaves after the fixed text and the reply
    let room = 8_000 - fixed_tokens(&small) - u64::from(small.max_tokens);
    let files = nexo_core::message::estimate_tokens(&small.messages);
    assert!(files <= room, "{files} tokens of messages in {room} left");
    // with no window known the same files are attached as at 200k
    let mut none = with_files(200_000);
    none.providers.get_mut("mock").unwrap().context_window.clear();
    let unknown = first_request(none, d.path()).await;
    assert_eq!(sent_without_clock(&big.messages), sent_without_clock(&unknown.messages));
}

/// The requests of a session at window `w` (none when `None`) that is given two long messages, compacted with a
/// summary of `summary_chars` characters, and then given a third message.
async fn compacted(w: Option<u32>, summary_chars: usize) -> Vec<ModelRequest> {
    let d = project();
    let client = Scripted::new(vec![
        Reply::Events(uncounted(text("first answer"))),
        Reply::Events(uncounted(text("second answer"))),
        Reply::Events(uncounted(text(&"s".repeat(summary_chars)))),
        Reply::Events(uncounted(text("third answer"))),
    ]);
    let mut cfg = cfg_window(w.unwrap_or(200_000));
    if w.is_none() {
        cfg.providers.get_mut("mock").unwrap().context_window.clear();
    }
    cfg.context.policy = "plain".into();
    let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
    r.next().await;
    for words in ["first", "second"] {
        let long = format!("{words}: please look at the parser and say why it fails on empty input. ").repeat(130);
        r.send(AgentCommand::Submit { text: long, attachments: vec![] });
        r.turn().await;
    }
    r.send(AgentCommand::Compact);
    r.until(|e| matches!(e, AgentEvent::Compacted { .. })).await;
    r.send(AgentCommand::Submit { text: "and now the lexer".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    client.requests()
}

#[tokio::test]
async fn a_summary_at_8k_is_asked_for_half_of_what_the_window_leaves_and_the_request_after_it_fits() {
    let _g = config_lock().lock().await;
    // a model that fills the whole reply it was allowed
    let reqs = compacted(Some(8_000), 8_000).await;
    let left = 8_000 - fixed_tokens(&reqs[0]) - u64::from(reqs[0].max_tokens);
    let summary = &reqs[2];
    eprintln!("room {left}; summary asked {} tokens", summary.max_tokens);
    assert!(u64::from(summary.max_tokens) <= left / 2, "the summary is asked for {} of {left}", summary.max_tokens);
    assert!(
        summary.messages[0].text().contains(&format!("up to {} tokens", summary.max_tokens)),
        "the summary request says its limit"
    );
    // the summary the model wrote fills what it was given (4 000 characters), then the request after it
    let after = reqs.last().unwrap();
    assert!(after.messages[0].text().contains("[summary]"), "the history was compacted");
    let total = total_with_reply(after);
    assert!(total <= 8_000, "the request after the compaction and its reply take {total} tokens of 8000");
}

#[tokio::test]
async fn a_summary_at_200k_is_asked_for_as_before() {
    let _g = config_lock().lock().await;
    let at_200k = compacted(Some(200_000), 400).await;
    let unknown = compacted(None, 400).await;
    assert_eq!(at_200k[2].max_tokens, 4_096);
    assert_eq!(at_200k[2].max_tokens, unknown[2].max_tokens);
    assert_eq!(at_200k[3].messages[0].text(), unknown[3].messages[0].text(), "the kept messages are the same");
}
