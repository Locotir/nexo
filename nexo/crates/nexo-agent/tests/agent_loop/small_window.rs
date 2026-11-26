//! A small window: what nexo adds on its own (instruction files, a sub-agent's task and reply) is made to fit,
//! and the model is told what was cut.

use super::*;

/// The fixed part of the input estimate: UTF-8 bytes of system text and serialized tool schemas, divided by four.
fn fixed_tokens(r: &ModelRequest) -> u64 {
    (r.system.len() + serde_json::to_string(&r.tools).unwrap().len()) as u64 / 4
}

/// `messages` as they go out, as JSON, with the time of each person's message (`[16:13:05`, see `person_clock`)
/// replaced by `[<T>`: two runs of the same session type their messages in different seconds. Nothing else changes.
/// What the window fit leaves of old images: each run of them is one omission marker, in place.
fn omit_images(parts: &mut Vec<Part>) {
    let mut out = Vec::with_capacity(parts.len());
    let mut after_image = false;
    for mut part in std::mem::take(parts) {
        if matches!(part, Part::Image { .. }) {
            if !after_image {
                out.push(Part::text("[Historical image content omitted to fit the context window.]"));
            }
            after_image = true;
            continue;
        }
        after_image = false;
        if let Part::ToolResult { content, .. } = &mut part {
            omit_images(content);
        }
        out.push(part);
    }
    *parts = out;
}

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
/// summary of `summary_chars` characters (or the full reply budget when `None`), then a third message.
async fn compacted(w: Option<u32>, summary_chars: Option<usize>) -> Vec<ModelRequest> {
    let d = project();
    let client = Scripted::new(vec![
        Reply::Events(uncounted(text("first answer"))),
        Reply::Events(uncounted(text("second answer"))),
    ]);
    let mut cfg = cfg_window(w.unwrap_or(200_000));
    if w.is_none() {
        cfg.providers.get_mut("mock").unwrap().context_window.clear();
    }
    cfg.context.policy = "plain".into();
    let ordinary_reply = w.map_or(cfg.agent.max_tokens, |window| cfg.agent.max_tokens.min((window / 4).max(256)));
    let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
    r.next().await;
    for words in ["first", "second"] {
        let long = format!("{words}: please look at the parser and say why it fails on empty input. ").repeat(130);
        r.send(AgentCommand::Submit { text: long, attachments: vec![] });
        r.turn().await;
    }
    // The side call gets half the room remaining after the ordinary request's fixed input and reply.
    // Match that advertised budget rather than returning a hard-coded oversized "successful" summary.
    let requests = client.requests();
    let first = &requests[0];
    let reply = w.map_or(4_096, |window| {
        // Admission may reduce first.max_tokens; compaction reserves the configured reply instead.
        let room = u64::from(window) - fixed_tokens(first) - u64::from(ordinary_reply);
        4_096.min(window / 2).min((room / 2).max(256) as u32)
    });
    // The summariser's conservative contract allows three UTF-8 bytes per requested token.
    let summary_chars = summary_chars.unwrap_or(reply as usize * 3);
    client.replies.lock().unwrap().extend([
        Reply::Events(uncounted(text(&"s".repeat(summary_chars)))),
        Reply::Events(uncounted(text("third answer"))),
    ]);
    r.send(AgentCommand::Compact);
    let events =
        r.until(|e| matches!(e, AgentEvent::Compacted { .. } | AgentEvent::Notice { level: Level::Error, .. })).await;
    assert!(
        matches!(events.last(), Some(AgentEvent::Compacted { .. })),
        "expected compaction, not a rejected summary: {events:?}"
    );
    assert_eq!(client.requests()[2].max_tokens, reply, "the fixture follows the real summary reply cap");
    r.send(AgentCommand::Submit { text: "and now the lexer".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    client.requests()
}

#[tokio::test]
async fn a_summary_at_8k_is_asked_for_half_of_what_the_window_leaves_and_the_request_after_it_fits() {
    let _g = config_lock().lock().await;
    // a model that fills the whole reply it was allowed
    let reqs = compacted(Some(8_000), None).await;
    let left = 8_000 - fixed_tokens(&reqs[0]) - u64::from(reqs[0].max_tokens);
    let summary = &reqs[2];
    eprintln!("room {left}; summary asked {} tokens", summary.max_tokens);
    assert!(u64::from(summary.max_tokens) <= left / 2, "the summary is asked for {} of {left}", summary.max_tokens);
    assert!(
        summary.messages[0].text().contains(&format!("up to {} tokens", summary.max_tokens)),
        "the summary request says its limit"
    );
    // The summary fills the actual reply's conservative byte budget and survives in the next request.
    let after = reqs.last().unwrap();
    assert!(after.messages[0].text().contains("[summary]"), "the history was compacted");
    assert!(
        after.messages[0].text().contains(&"s".repeat(summary.max_tokens as usize * 3)),
        "the full budget-sized summary is retained, not silently truncated"
    );
    let total = total_with_reply(after);
    assert!(total <= 8_000, "the request after the compaction and its reply take {total} tokens of 8000");
}

#[tokio::test]
async fn a_summary_at_200k_is_asked_for_as_before() {
    let _g = config_lock().lock().await;
    let at_200k = compacted(Some(200_000), Some(400)).await;
    let unknown = compacted(None, Some(400)).await;
    assert_eq!(at_200k[2].max_tokens, 4_096);
    assert_eq!(at_200k[2].max_tokens, unknown[2].max_tokens);
    assert_eq!(at_200k[3].messages[0].text(), unknown[3].messages[0].text(), "the kept messages are the same");
}

// Isolated audit: exercises the real submit/attachment/render/admission path with no network.
#[tokio::test]
async fn audit_irreducible_image_admission() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for words in ["describe", ""] {
            let d = project();
            // Valid 1x1 PNG; actual image tokens are deliberately NOT inferred from bytes or dimensions.
            use base64::Engine;
            let png = base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jXioAAAAASUVORK5CYII=")
                .unwrap();
            let attachments: Vec<_> = (0..6)
                .map(|i| {
                    let path = PathBuf::from(format!("shot-{i}.png"));
                    std::fs::write(d.path().join(&path), &png).unwrap();
                    nexo_core::agent::Attachment { path }
                })
                .collect();
            let client = Scripted::new(vec![Reply::Events(uncounted(text("pong")))]);
            let mut cfg = cfg_window(8_000);
            cfg.context.policy = policy.into();
            let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
            let AgentEvent::Ready { session_id: id, .. } = r.next().await else { panic!("no Ready") };
            r.send(AgentCommand::Submit { text: words.into(), attachments: attachments.clone() });
            let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
            let history = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            let images = history.iter().flat_map(|m| &m.content).filter(|p| matches!(p, Part::Image { .. })).count();
            let history_est = nexo_core::message::estimate_tokens(&history);
            let told = notices(&evs);
            println!(
                "AUDIT policy={policy} words={words:?} window=8000 images={images} saved_conversation_est={history_est} sends={} end={:?} notices={told:?}",
                client.requests().len(),
                end_of(&evs)
            );
            assert_eq!(images, 6);
            assert_eq!(
                client.requests().len(),
                0,
                "{policy} {words:?}: irreducible images must not be silently dropped or sent"
            );
            assert_eq!(end_of(&evs), TurnEnd::Error);
            assert!(told.iter().any(|s| s.contains("request over the window") && s.contains("Not sent")), "{told:?}");
            r.send(AgentCommand::Submit { text: "ping".into(), attachments: vec![] });
            let next = r.turn().await;
            println!(
                "AUDIT recovery policy={policy} words={words:?} sends={} end={:?} notices={:?}",
                client.requests().len(),
                end_of(&next),
                notices(&next)
            );
            assert_eq!(
                client.requests().len(),
                1,
                "{policy} {words:?}: ping must recover after rejection: {:?}",
                notices(&next)
            );
            assert_eq!(end_of(&next), TurnEnd::Done);
            for req in client.requests() {
                assert_fits(&req, 8_000, 4.0 / 3.0);
            }
            let recovered = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            assert!(recovered.starts_with(&history));
            assert!(client.requests()[0].messages.iter().any(|m| m.text().contains("ping")));
            // The recovery must not turn admission into a blanket licence to drop current media.
            r.send(AgentCommand::Submit { text: "keep all six current images and these words".into(), attachments });
            let refused_again = r.turn().await;
            assert_eq!(end_of(&refused_again), TurnEnd::Error);
            assert_eq!(client.requests().len(), 1, "{policy}: a second irreducible turn must not be sent");
            let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            assert!(saved.starts_with(&recovered));
            assert!(saved.last().unwrap().text().contains("keep all six current images and these words"));
            r.stop().await;

            // The smaller outbound view is not a destructive compaction: reopening with enough room
            // recovers both rejected turns byte-for-byte, including all twelve image sources.
            let resumed_client = Scripted::new(vec![Reply::Events(uncounted(text("retrieved")))]);
            let mut cfg = cfg_window(64_000);
            cfg.context.policy = policy.into();
            let mut resumed =
                start_with(resumed_client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
            resumed.next().await;
            resumed.send(AgentCommand::Submit { text: "inspect the saved images".into(), attachments: vec![] });
            let evs = resumed.turn().await;
            assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}: {:?}", notices(&evs));
            let reqs = resumed_client.requests();
            assert_eq!(reqs.len(), 1);
            let pictures = |ms: &[Message]| {
                ms.iter()
                    .flat_map(|m| &m.content)
                    .filter(|p| matches!(p, Part::Image { .. }))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(pictures(&saved).len(), 12);
            assert_eq!(
                pictures(&reqs[0].messages),
                pictures(&saved),
                "{policy}: original image payloads remain retrievable"
            );
            assert!(reqs[0].messages.iter().any(|m| m.text().contains("keep all six current images and these words")));
            assert!(nexo_agent::Session::open(&id).unwrap().messages().starts_with(&saved));
            assert_fits(&reqs[0], 64_000, 1.0);
            resumed.stop().await;
        }
    }
}

#[tokio::test]
async fn audit_one_image_control_admission() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        let d = project();
        std::fs::write(d.path().join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
        let mut cfg = cfg_window(8_000);
        cfg.context.policy = policy.into();
        let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
        r.next().await;
        r.send(AgentCommand::Submit {
            text: String::new(),
            attachments: vec![nexo_core::agent::Attachment { path: PathBuf::from("shot.png") }],
        });
        let evs = r.turn().await;
        let reqs = client.requests();
        assert_eq!(reqs.len(), 1, "{policy}: {:?}", notices(&evs));
        let req = &reqs[0];
        let raw_input = fixed_tokens(req) + nexo_core::message::estimate_tokens(&req.messages);
        let fitted_input = (raw_input as f64 * (4.0 / 3.0)).ceil() as u64;
        let images = req.messages.iter().flat_map(|m| &m.content).filter(|p| matches!(p, Part::Image { .. })).count();
        println!(
            "AUDIT control policy={policy} words=empty window=8000 images={images} raw_input_est={raw_input} fitted_input_est={fitted_input} reply={} total={} sends=1 end={:?}",
            req.max_tokens,
            fitted_input + u64::from(req.max_tokens),
            end_of(&evs)
        );
        assert_eq!(images, 1);
        assert_eq!(end_of(&evs), TurnEnd::Done);
        assert_fits(req, 8_000, 4.0 / 3.0);
        r.stop().await;
    }
}

#[tokio::test]
async fn audit_single_irreducible_image_at_2500() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for image in [false, true] {
            let d = project();
            use base64::Engine;
            let png = base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jXioAAAAASUVORK5CYII=")
                .unwrap();
            std::fs::write(d.path().join("shot.png"), png).unwrap();
            let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
            let mut cfg = cfg_window(2500);
            cfg.context.policy = policy.into();
            let mut r = start_with(client.clone(), cfg, None, d.path(), ToolSet::default()).await;
            let AgentEvent::Ready { session_id: id, .. } = r.next().await else { panic!("no Ready") };
            r.send(AgentCommand::Submit {
                text: String::new(),
                attachments: if image {
                    vec![nexo_core::agent::Attachment { path: PathBuf::from("shot.png") }]
                } else {
                    vec![]
                },
            });
            let evs = r.turn().await;
            let history = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            println!(
                "AUDIT single policy={policy} image={image} words=empty tools=empty window=2500 sends={} end={:?} notices={:?}",
                client.requests().len(),
                end_of(&evs),
                notices(&evs)
            );
            if image {
                assert_eq!(client.requests().len(), 0);
                assert_eq!(end_of(&evs), TurnEnd::Error);
                assert_eq!(
                    history.iter().flat_map(|m| &m.content).filter(|p| matches!(p, Part::Image { .. })).count(),
                    1
                );
            } else {
                assert_eq!(client.requests().len(), 1);
                let req = &client.requests()[0];
                let raw = fixed_tokens(req) + nexo_core::message::estimate_tokens(&req.messages);
                println!(
                    "AUDIT single baseline policy={policy} raw_input_est={raw} reply={} total={}",
                    req.max_tokens,
                    (raw as f64 * (4.0 / 3.0)).ceil() as u64 + u64::from(req.max_tokens)
                );
                assert_fits(req, 2500, 4.0 / 3.0);
            }
            r.stop().await;
        }
    }
}

#[tokio::test]
async fn audit_boundary_duplicate_humans_current_media_and_rewind() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        let d = project();
        data_dir();
        let picture = Part::Image {
            source: nexo_core::message::ImageSource::Base64 {
                media_type: "image/png".into(),
                data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jXioAAAAASUVORK5CYII="
                    .into(),
            },
        };
        let mut duplicate = Message::user("identical words");
        duplicate.content.extend(vec![picture.clone(); 6]);
        let original = vec![duplicate.clone(), duplicate.clone()];
        let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
        for m in &original {
            session.append(m).unwrap();
        }
        let id = session.id().to_string();
        drop(session);
        use base64::Engine;
        let Part::Image { source: nexo_core::message::ImageSource::Base64 { data, .. } } = &picture else {
            unreachable!()
        };
        std::fs::write(d.path().join("shot.png"), base64::engine::general_purpose::STANDARD.decode(data).unwrap())
            .unwrap();
        let client =
            Scripted::new(vec![Reply::Events(uncounted(text("ok"))), Reply::Events(uncounted(text("rewound ok")))]);
        let mut cfg = cfg_window(8_000);
        cfg.context.policy = policy.into();
        let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
        r.next().await;
        r.send(AgentCommand::Submit {
            text: "identical words".into(),
            attachments: vec![nexo_core::agent::Attachment { path: PathBuf::from("shot.png") }],
        });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}: {:?}", notices(&evs));
        let sent = client.requests();
        assert_eq!(sent.len(), 1);
        let pictures = |ms: &[Message]| {
            ms.iter().flat_map(|m| &m.content).filter(|p| matches!(p, Part::Image { .. })).cloned().collect::<Vec<_>>()
        };
        assert_eq!(
            pictures(&sent[0].messages),
            vec![picture.clone()],
            "{policy}: only obsolete media dropped, current image intact"
        );
        assert!(sent[0].messages.iter().any(|m| m.text().contains("identical words")));
        assert_fits(&sent[0], 8_000, 4.0 / 3.0);
        let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
        assert!(saved.starts_with(&original));
        assert_eq!(pictures(&saved).len(), 13);

        r.send(AgentCommand::Rewind { index: 1 });
        r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("rewound"))).await;
        r.send(AgentCommand::Submit { text: "ping after rewind".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}: {:?}", notices(&evs));
        let sent = client.requests();
        assert_eq!(sent.len(), 2);
        assert!(pictures(&sent[1].messages).is_empty());
        assert!(sent[1].messages.iter().any(|m| m.text().contains("ping after rewind")));
        assert_fits(&sent[1], 8_000, 4.0 / 3.0);
        let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
        assert_eq!(saved[0], duplicate);
        r.stop().await;
    }
}

#[tokio::test]
async fn audit_image_recovery_preserves_prior_instructions_and_tool_ids() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for oversized_text in [false, true] {
            let d = project();
            data_dir();
            let image = Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/historical.png".into() },
            };
            let instruction = Message::user(if oversized_text {
                "NEVER delete project files. ".repeat(2000)
            } else {
                "NEVER delete project files. Answer in Spanish and preserve both repositories.".into()
            });
            let mut old = Message::user("previous rejected image request: keep these exact words");
            old.content.extend(vec![image.clone(); 6]);
            let original = vec![
                instruction.clone(),
                Message::new(
                    Role::Assistant,
                    vec![Part::tool_call("prior-call", "read", serde_json::json!({"path":"policy.txt"}))],
                ),
                Message::tool_results(vec![Part::ToolResult {
                    call_id: "prior-call".into(),
                    name: Some("read".into()),
                    content: vec![Part::text("historical tool text must survive"), image.clone()],
                    is_error: false,
                }]),
                Message::assistant("I will follow those instructions."),
                old,
            ];
            let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
            for m in &original {
                session.append(m).unwrap();
            }
            let id = session.id().to_string();
            drop(session);
            let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
            let mut cfg = cfg_window(8_000);
            cfg.context.policy = policy.into();
            let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
            r.next().await;
            r.send(AgentCommand::Submit { text: "ping: obey previous instructions".into(), attachments: vec![] });
            let evs = r.turn().await;
            let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            assert!(saved.starts_with(&original), "{policy}: persistent raw history changed");
            let sent = client.requests();
            if oversized_text {
                assert!(sent.is_empty(), "{policy}: do not delete historical text to force admission");
                assert_eq!(end_of(&evs), TurnEnd::Error);
            } else {
                assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}: {:?}", notices(&evs));
                assert_eq!(sent.len(), 1);
                let mut expected = original.clone();
                omit_images(&mut expected[4].content);
                let Part::ToolResult { content, .. } = &mut expected[2].content[0] else { unreachable!() };
                omit_images(content);
                expected.push(saved[original.len()].clone());
                assert_eq!(sent[0].messages, expected, "{policy}: only historical image parts may change");
                assert!(nexo_core::message::is_well_formed(&sent[0].messages));
                assert_fits(&sent[0], 8_000, 4.0 / 3.0);
            }
            r.stop().await;
        }
    }
}

#[tokio::test]
async fn audit_review_image_recovery_tool_round_and_opaque_budget() {
    let _g = config_lock().lock().await;
    let mut failures = Vec::new();
    for policy in ["plain", "curator", "auto"] {
        for kind in ["tool", "signed", "redacted"] {
            let d = project();
            data_dir();
            std::fs::write(d.path().join("policy.txt"), "Never delete files; answer in Spanish.").unwrap();
            let mut old = Message::user("Keep this historical instruction verbatim: never delete files.");
            old.content.extend((0..6).map(|_| Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/not-fetched.png".into() },
            }));
            let mut original = vec![old];
            if !kind.starts_with("tool") {
                let part = if kind == "signed" {
                    Part::Reasoning {
                        text: "thinking".into(),
                        signature: Some("s".repeat(200_000)),
                        encrypted: None,
                        model: Some("m1".into()),
                    }
                } else {
                    Part::RedactedReasoning { data: "r".repeat(200_000), model: Some("m1".into()) }
                };
                original.push(Message::new(Role::Assistant, vec![part, Part::text("previous answer")]));
                let mut nominal = original.clone();
                nominal.push(Message::user("next"));
                let hidden: u64 = nominal.iter().map(nexo_core::message::reasoning_tokens).sum();
                println!(
                    "REVIEW {policy}/{kind} nominal={} omitted_opaque_tokens={hidden} window=8000",
                    nexo_core::message::estimate_tokens(&nominal)
                );
                assert!(hidden > 8_000);
            }
            let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
            for m in &original {
                session.append(m).unwrap();
            }
            let id = session.id().to_string();
            drop(session);
            let client = Scripted::new(if kind.starts_with("tool") {
                vec![
                    Reply::Events(uncounted(call(
                        "current-read",
                        "read",
                        json!({"path":d.path().join("policy.txt"), "limit":2000}),
                    ))),
                    Reply::Events(uncounted(text("He leído la política."))),
                ]
            } else {
                vec![Reply::Events(uncounted(text("unsafe admission")))]
            });
            let mut cfg = cfg_window(8_000);
            cfg.context.policy = policy.into();
            let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
            r.next().await;
            r.send(AgentCommand::Submit {
                text: "consulta policy.txt y conserva todas las instrucciones".into(),
                attachments: vec![],
            });
            let evs = r.turn().await;
            let sent = client.requests();
            println!("REVIEW {policy}/{kind} sends={} end={:?} notices={:?}", sent.len(), end_of(&evs), notices(&evs));
            let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            assert!(saved.starts_with(&original));
            if kind.starts_with("tool") {
                if sent.len() != 2 || end_of(&evs) != TurnEnd::Done {
                    failures.push(format!("{policy}/{kind}: second round blocked"));
                }
                for (i, req) in sent.iter().enumerate() {
                    let mut expected = saved[..original.len() + 1 + i * 2].to_vec();
                    omit_images(&mut expected[0].content);
                    assert_eq!(req.messages, expected, "{policy}: complete turn and instructions preserved");
                    assert!(nexo_core::message::is_well_formed(&req.messages));
                    assert_fits(req, 8_000, 4.0 / 3.0);
                    if i == 1 {
                        assert!(
                            req.messages.iter().any(|m| m.content.iter().any(|p| {
                                matches!(p, Part::ToolResult { call_id, content, is_error: false, .. }
                                if call_id == "current-read" && !nexo_core::message::result_text(content).is_empty())
                            })),
                            "the actual successful tool output reaches the second round: {:?}",
                            req.messages
                        );
                    }
                }
            } else if !sent.is_empty() || end_of(&evs) != TurnEnd::Error {
                failures.push(format!("{policy}/{kind}: opaque overbudget request admitted"));
            }
            r.stop().await;
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[tokio::test]
async fn audit_followup_small_signature_and_internal_continuation() {
    let _g = config_lock().lock().await;
    let mut failures = Vec::new();
    for policy in ["plain", "curator", "auto"] {
        for continuation in [false, true] {
            let d = project();
            data_dir();
            let mut old = Message::user("Never delete files; preserve these words.");
            old.content.extend((0..6).map(|_| Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/old.png".into() },
            }));
            let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
            session.append(&old).unwrap();
            let id = session.id().to_string();
            drop(session);
            let mut signed = vec![
                StreamEvent::Start { response_id: None, model: Some("m1".into()) },
                StreamEvent::ReasoningDelta("Plan carefully.".into()),
                StreamEvent::ReasoningMeta { signature: Some("small-signature".into()), encrypted: None },
            ];
            if !continuation {
                signed.push(StreamEvent::TextDelta("First answer.".into()));
            }
            signed.extend([StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]);
            let client = Scripted::new(vec![
                Reply::Events(signed),
                Reply::Events(uncounted(text("Second answer."))),
                Reply::Events(uncounted(text("Foreign model answer."))),
            ]);
            let mut cfg = cfg_window(8_000);
            cfg.context.policy = policy.into();
            cfg.providers.get_mut("mock").unwrap().context_window.insert("m2".into(), 8_000);
            let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
            r.next().await;
            r.send(AgentCommand::Submit { text: "First human request.".into(), attachments: vec![] });
            let first = r.turn().await;
            if !continuation {
                assert_eq!(end_of(&first), TurnEnd::Done);
                r.send(AgentCommand::Submit { text: "Second human request.".into(), attachments: vec![] });
                let next = r.turn().await;
                if end_of(&next) != TurnEnd::Done {
                    failures.push(format!("{policy}: signed followup: {:?}", notices(&next)));
                }
            } else if end_of(&first) != TurnEnd::Done {
                failures.push(format!("{policy}: internal continuation: {:?}", notices(&first)));
            }
            if client.requests().len() == 2 {
                r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
                r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
                r.send(AgentCommand::Submit { text: "Continue after model switch.".into(), attachments: vec![] });
                let next = r.turn().await;
                assert_eq!(end_of(&next), TurnEnd::Done, "{policy}: {:?}", notices(&next));
            }
            let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            assert_eq!(saved[0], old);
            for req in client.requests() {
                let last = req.messages.last().unwrap();
                let end = saved.iter().rposition(|m| m == last).unwrap();
                let mut expected = saved[..=end].to_vec();
                omit_images(&mut expected[0].content);
                assert_eq!(req.messages, expected, "{policy}: complete raw turn/protocol");
                assert!(nexo_core::message::is_well_formed(&req.messages));
                assert_fits(&req, 8_000, 4.0 / 3.0);
            }
            r.stop().await;
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[tokio::test]
async fn audit_image_only_history_recovers_with_nonempty_tool_results() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        let d = project();
        data_dir();
        let image = Part::Image {
            source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/historical.png".into() },
        };
        let original = vec![
            Message::user("Keep prior instructions exactly."),
            Message::new(
                Role::Assistant,
                vec![Part::tool_call("image-only-call", "read", serde_json::json!({"path":"shot.png"}))],
            ),
            Message::tool_results(vec![Part::ToolResult {
                call_id: "image-only-call".into(),
                name: Some("read".into()),
                content: vec![image.clone(); 6],
                is_error: false,
            }]),
            Message::assistant("Previous image result received."),
            Message::new(Role::User, vec![image; 6]),
        ];
        let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
        for m in &original {
            session.append(m).unwrap();
        }
        let id = session.id().to_string();
        drop(session);
        let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
        let mut cfg = cfg_window(8_000);
        cfg.context.policy = policy.into();
        let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "ping after image-only history".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}: {:?}", notices(&evs));
        let sent = client.requests();
        assert_eq!(sent.len(), 1, "{policy}");
        let marker = Part::text("[Historical image content omitted to fit the context window.]");
        let mut expected = original.clone();
        expected[4].content = vec![marker.clone()];
        let Part::ToolResult { content, .. } = &mut expected[2].content[0] else { unreachable!() };
        *content = vec![marker];
        let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
        expected.push(saved[original.len()].clone());
        assert_eq!(sent[0].messages, expected, "{policy}: preserve roles, order and tool metadata");
        assert!(nexo_core::message::is_well_formed(&sent[0].messages));
        assert!(sent[0].messages.iter().all(|m| !m.content.is_empty()));
        assert_fits(&sent[0], 8_000, 4.0 / 3.0);
        assert!(nexo_agent::Session::open(&id).unwrap().messages().starts_with(&original));
        r.stop().await;
    }
}

/// Cold admission ignores foreign opaque reserves omitted by the real serializers.
/// This is not a provider token-count test.
#[tokio::test]
async fn audit_foreign_opaque_reserve_allows_serializer_omission() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for protocol in ["openai-responses", "anthropic"] {
            let mut control = None;
            for oversized in [false, true] {
                let d = project();
                data_dir();
                let mut old = Message::user("Never delete files. Preserve this historical instruction.");
                old.content.extend((0..6).map(|_| Part::Image {
                    source: nexo_core::message::ImageSource::Url {
                        url: "https://example.invalid/never-fetched.png".into(),
                    },
                }));
                // Both controls are foreign. Only the opaque payload length changes.
                let foreign = Part::Reasoning {
                    text: "FOREIGN_THINKING_MUST_NOT_REACH_WIRE".into(),
                    signature: Some("FOREIGN_SIGNATURE".repeat(if oversized { 12_500 } else { 1 })),
                    encrypted: Some("FOREIGN_ENCRYPTED".repeat(if oversized { 12_500 } else { 1 })),
                    model: Some("other-model".into()),
                };
                let original =
                    vec![old, Message::new(Role::Assistant, vec![foreign.clone(), Part::text("Previous answer.")])];
                let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
                for m in &original {
                    session.append(m).unwrap();
                }
                let id = session.id().to_string();
                drop(session);
                let mut cfg = cfg_window(8_000);
                cfg.context.policy = policy.into();
                cfg.providers.get_mut("mock").unwrap().protocol = protocol.into();
                let provider_cfg = cfg.providers["mock"].clone();
                let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
                let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), ToolSet::default()).await;
                r.next().await;
                r.send(AgentCommand::Submit {
                    text: "Continue; obey all previous instructions.".into(),
                    attachments: vec![],
                });
                let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
                let sent = client.requests();
                let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
                assert!(saved.starts_with(&original), "persistent history must stay intact");
                let body = |req: &ModelRequest| {
                    if protocol == "anthropic" {
                        nexo_providers::anthropic::Anthropic::body(&provider_cfg, req)
                    } else {
                        nexo_providers::openai_responses::OpenAiResponses::body(&provider_cfg, req)
                    }
                };
                assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}/{protocol}: {:?}", notices(&evs));
                assert_eq!(sent.len(), 1);
                assert_eq!(sent[0].messages[1].content[0], foreign);
                assert!(sent[0].messages.iter().flat_map(|m| &m.content).all(|p| !matches!(p, Part::Image { .. })));
                if !oversized {
                    control = Some(sent[0].clone());
                } else {
                    let candidate = &sent[0];
                    // Compare actual admitted requests, normalizing only per-session metadata.
                    let mut short = candidate.clone();
                    short.messages[1].content[0] = control.as_ref().unwrap().messages[1].content[0].clone();
                    let actual = body(candidate);
                    assert_eq!(actual, body(&short), "opaque length must have ZERO effect on real wire body");
                    let wire = serde_json::to_string(&actual).unwrap();
                    assert!(!wire.contains("FOREIGN_"));
                    assert!(wire.contains("Never delete files."));
                    assert!(wire.contains("Previous answer."));
                    assert!(wire.contains("Continue; obey all previous instructions."));
                    let opaque_bytes = serde_json::to_vec(&foreign).unwrap().len();
                    let generic = fixed_tokens(candidate) + nexo_core::message::estimate_tokens(&candidate.messages);
                    assert!(generic + u64::from(candidate.max_tokens) < 8_000);
                    assert!(opaque_bytes > 8_000);
                    assert!(wire.len() as u64 + u64::from(candidate.max_tokens) < 8_000);
                    println!(
                        "FOREIGN_OPAQUE_FIXED policy={policy} protocol={protocol} reserve_bytes={opaque_bytes} wire_bytes={} oversized_sends=1",
                        wire.len()
                    );
                }
                r.stop().await;
            }
        }
    }
}

/// Sent opaque content in the current tool round must still fail closed.
#[tokio::test]
async fn audit_transmitted_opaque_reserve_stays_protected() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for kind in ["anthropic-signed", "anthropic-redacted", "responses-encrypted"] {
            for oversized in [false, true] {
                let d = project();
                data_dir();
                let payload = "SENT_OPAQUE_".repeat(if oversized { 20_000 } else { 1 });
                let model = Some("m1".into());
                let part = if kind == "anthropic-redacted" {
                    Part::RedactedReasoning { data: payload.clone(), model }
                } else {
                    Part::Reasoning {
                        text: "Visible thinking".into(),
                        signature: Some(if kind == "responses-encrypted" { "rs_id".into() } else { payload.clone() }),
                        encrypted: (kind == "responses-encrypted").then(|| payload.clone()),
                        model,
                    }
                };
                let mut old = Message::user("Never delete files.");
                old.content.extend((0..6).map(|_| Part::Image {
                    source: nexo_core::message::ImageSource::Url {
                        url: "https://example.invalid/not-fetched.png".into(),
                    },
                }));
                let original = vec![old];
                let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
                session.append(&original[0]).unwrap();
                let id = session.id().to_string();
                drop(session);
                let protocol = if kind.starts_with("anthropic") { "anthropic" } else { "openai-responses" };
                let mut cfg = cfg_window(8_000);
                cfg.context.policy = policy.into();
                cfg.providers.get_mut("mock").unwrap().protocol = protocol.into();
                let provider = cfg.providers["mock"].clone();
                // Produce the opaque block in the current tool round, where Responses echoes it.
                let mut events = vec![StreamEvent::Start { response_id: None, model: Some("m1".into()) }];
                match &part {
                    Part::RedactedReasoning { data, .. } => {
                        events.push(StreamEvent::RedactedReasoning { data: data.clone() })
                    }
                    Part::Reasoning { text, signature, encrypted, .. } => {
                        events.push(StreamEvent::ReasoningDelta(text.clone()));
                        events.push(StreamEvent::ReasoningMeta {
                            signature: signature.clone(),
                            encrypted: encrypted.clone(),
                        });
                    }
                    _ => unreachable!(),
                }
                let path = d.path().join("policy.txt");
                std::fs::write(&path, "Keep instructions").unwrap();
                events.extend(call("opaque-read", "read", json!({"path":path})));
                let client = Scripted::new(vec![Reply::Events(events), Reply::Events(uncounted(text("ok")))]);
                let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
                r.next().await;
                r.send(AgentCommand::Submit { text: "Read policy.txt, then continue.".into(), attachments: vec![] });
                let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
                let sent = client.requests();
                assert_eq!(
                    sent.len(),
                    if oversized { 1 } else { 2 },
                    "{policy}/{kind}/{oversized}: {:?}",
                    notices(&evs)
                );
                assert_eq!(end_of(&evs), if oversized { TurnEnd::Error } else { TurnEnd::Done });
                let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
                assert!(saved.starts_with(&original));
                // Serialize the would-be second request, including real call/result placement.
                let mut candidate = sent[0].clone();
                candidate.messages = saved;
                omit_images(&mut candidate.messages[0].content);
                let body = if protocol == "anthropic" {
                    nexo_providers::anthropic::Anthropic::body(&provider, &candidate)
                } else {
                    nexo_providers::openai_responses::OpenAiResponses::body(&provider, &candidate)
                };
                let wire = serde_json::to_string(&body).unwrap();
                assert!(wire.contains(&payload), "control must really transmit opaque bytes: {kind}");
                assert!(wire.contains("Never delete files."));
                if oversized {
                    assert!(wire.len() > 8_000);
                }
                r.stop().await;
            }
        }
    }
}

/// Independent cold sessions: exception boundaries must not expand to legacy,
/// nested or non-exempt protocol data. Small controls serialize the admitted request.
#[tokio::test]
async fn audit_opaque_exception_boundaries_cold() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for (protocol, kind) in [
            ("anthropic", "foreign-redacted"),
            ("openai-responses", "foreign-redacted"),
            ("anthropic", "legacy"),
            ("openai-responses", "legacy"),
            ("anthropic", "nested-foreign"),
            ("openai-responses", "nested-foreign"),
            ("openai", "foreign-nonexempt"),
        ] {
            for large in [false, true] {
                let d = project();
                data_dir();
                let payload = "BOUNDARY_OPAQUE_".repeat(if large { 1500 } else { 1 });
                let part = if kind == "foreign-redacted" {
                    Part::RedactedReasoning { data: payload.clone(), model: Some("foreign".into()) }
                } else {
                    Part::Reasoning {
                        text: if kind == "nested-foreign" { payload.clone() } else { "visible reasoning".into() },
                        signature: Some(payload.clone()),
                        encrypted: Some(payload.clone()),
                        model: (kind != "legacy").then(|| "foreign".into()),
                    }
                };
                let mut old = Message::user("Never delete files. Keep historical instructions.");
                old.content.extend((0..6).map(|_| Part::Image {
                    source: nexo_core::message::ImageSource::Url {
                        url: "https://example.invalid/never-fetched.png".into(),
                    },
                }));
                let mut original = vec![old];
                if kind == "nested-foreign" {
                    original.push(Message::new(Role::Assistant, vec![Part::tool_call("nested", "read", json!({}))]));
                    original.push(Message::tool_results(vec![Part::ToolResult {
                        call_id: "nested".into(),
                        name: Some("read".into()),
                        content: vec![Part::text("Keep this result"), part.clone()],
                        is_error: false,
                    }]));
                } else {
                    original.push(Message::new(Role::Assistant, vec![part.clone(), Part::text("Prior answer")]));
                }
                let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
                for m in &original {
                    session.append(m).unwrap();
                }
                let id = session.id().to_string();
                drop(session);
                let mut cfg = cfg_window(8_000);
                cfg.context.policy = policy.into();
                cfg.providers.get_mut("mock").unwrap().protocol = protocol.into();
                let provider = cfg.providers["mock"].clone();
                let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
                let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), ToolSet::default()).await;
                r.next().await;
                r.send(AgentCommand::Submit { text: "Continue, preserve instructions.".into(), attachments: vec![] });
                let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
                let sent = client.requests();
                // Responses never sends top-level reasoning from before the latest person
                // message, so a large legacy block there is no reason to refuse the request.
                let allowed =
                    !large || kind == "foreign-redacted" || (protocol == "openai-responses" && kind == "legacy");
                assert_eq!(sent.len(), usize::from(allowed), "{policy}/{protocol}/{kind}/{large}: {:?}", notices(&evs));
                assert_eq!(end_of(&evs), if allowed { TurnEnd::Done } else { TurnEnd::Error });
                assert!(nexo_agent::Session::open(&id).unwrap().messages().starts_with(&original));
                if allowed && protocol != "openai" {
                    let req = &sent[0];
                    assert!(req.messages.iter().any(|m| m.content.contains(&part)) || kind == "nested-foreign");
                    let body = if protocol == "anthropic" {
                        nexo_providers::anthropic::Anthropic::body(&provider, req)
                    } else {
                        nexo_providers::openai_responses::OpenAiResponses::body(&provider, req)
                    };
                    let wire = serde_json::to_string(&body).unwrap();
                    // Responses omits historical reasoning even without a model tag.
                    let transmitted =
                        kind != "foreign-redacted" && !(protocol == "openai-responses" && kind == "legacy");
                    assert_eq!(wire.contains(&payload), transmitted, "{protocol}/{kind}: actual outbound body");
                    assert!(wire.contains("Never delete files."));
                    assert!(wire.contains("Continue, preserve instructions."));
                }
                r.stop().await;
            }
        }
    }
}

/// The first real usage sample learns a ratio below one. Mixed own + foreign
/// blocks in its tool continuation must still reserve every own opaque byte.
#[tokio::test]
async fn audit_opaque_mixed_foreign_own_learned_low_ratio() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for protocol in ["anthropic", "openai-responses"] {
            for large in [false, true] {
                let d = project();
                data_dir();
                let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
                let foreign = Part::Reasoning {
                    text: "FOREIGN_MIXED_TEXT".into(),
                    signature: Some("FOREIGN_MIXED_".repeat(2000)),
                    encrypted: Some("FOREIGN_MIXED_".repeat(2000)),
                    model: Some("foreign".into()),
                };
                let prior = Message::new(Role::Assistant, vec![foreign.clone(), Part::text("Prior answer")]);
                session.append(&Message::user("Never delete files.")).unwrap();
                session.append(&prior).unwrap();
                let id = session.id().to_string();
                drop(session);
                let payload = "OWN_MIXED_".repeat(if large { 2500 } else { 1 });
                let path = d.path().join("policy.txt");
                std::fs::write(&path, "Keep instructions").unwrap();
                let mut events = vec![
                    StreamEvent::Start { response_id: None, model: Some("m1".into()) },
                    StreamEvent::ReasoningDelta("Own thinking".into()),
                    StreamEvent::ReasoningMeta { signature: Some(payload.clone()), encrypted: Some(payload.clone()) },
                ];
                events.extend(
                    uncounted(call("mixed-read", "read", json!({"path":path})))
                        .into_iter()
                        .filter(|event| !matches!(event, StreamEvent::Done)),
                );
                events.push(StreamEvent::Usage(Usage { input: 1, output: 1, ..Default::default() }));
                events.push(StreamEvent::Done);
                let client = Scripted::new(vec![Reply::Events(events), Reply::Events(uncounted(text("ok")))]);
                let mut cfg = cfg_window(8_000);
                cfg.context.policy = policy.into();
                cfg.providers.get_mut("mock").unwrap().protocol = protocol.into();
                let provider = cfg.providers["mock"].clone();
                let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), nexo_tools::builtin()).await;
                r.next().await;
                r.send(AgentCommand::Submit { text: "Read policy.txt then continue.".into(), attachments: vec![] });
                let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
                let sent = client.requests();
                let est = fixed_tokens(&sent[0]) + nexo_core::message::estimate_tokens(&sent[0].messages);
                assert!((1.0 / est as f64).clamp(0.5, 3.0) < 1.0, "usage sample must learn ratio below one");
                assert_eq!(sent.len(), if large { 1 } else { 2 }, "{policy}/{protocol}/{large}: {:?}", notices(&evs));
                assert_eq!(end_of(&evs), if large { TurnEnd::Error } else { TurnEnd::Done });
                for (i, req) in sent.iter().enumerate() {
                    let body = if protocol == "anthropic" {
                        nexo_providers::anthropic::Anthropic::body(&provider, req)
                    } else {
                        nexo_providers::openai_responses::OpenAiResponses::body(&provider, req)
                    };
                    let wire = serde_json::to_string(&body).unwrap();
                    assert!(!wire.contains("FOREIGN_MIXED_"));
                    assert!(wire.contains("Never delete files."));
                    if i == 1 {
                        assert!(wire.contains(&payload), "own bytes must be in actual second request");
                    }
                }
                assert!(nexo_agent::Session::open(&id).unwrap().messages().contains(&prior));
                r.stop().await;
            }
        }
    }
}

#[tokio::test]
async fn audit_historical_image_and_oversized_current_result() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        let d = project();
        data_dir();
        let mut old = Message::user("Never delete files; keep this historical instruction.");
        old.content.extend((0..6).map(|_| Part::Image {
            source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/old.png".into() },
        }));
        let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
        session.append(&old).unwrap();
        let id = session.id().to_string();
        drop(session);
        let client = Scripted::new(vec![
            Reply::Events(uncounted(call("current-large", "read", json!({"path":"data"})))),
            Reply::Events(uncounted(text("Recovered after large result."))),
        ]);
        let mut cfg = cfg_window(8_000);
        cfg.context.policy = policy.into();
        let tools = ToolSet::new().with(Arc::new(Fill(200_000)));
        let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), tools).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "Read data; preserve all instructions.".into(), attachments: vec![] });
        let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
        let sent = client.requests();
        assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}: {:?}", notices(&evs));
        assert_eq!(sent.len(), 2, "{policy}");
        let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
        assert_eq!(saved[0], old);
        let original_result = saved
            .iter()
            .flat_map(|m| &m.content)
            .find(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == "current-large"))
            .unwrap();
        let Part::ToolResult { content, .. } = original_result else { unreachable!() };
        assert!(nexo_core::message::result_text(content).len() > 100_000, "raw result remains stored");
        let outgoing = &sent[1];
        let mut expected = saved[..saved.len() - 1].to_vec();
        omit_images(&mut expected[0].content);
        for message in &mut expected {
            for part in &mut message.content {
                if let Part::ToolResult { call_id, content, .. } = part {
                    if call_id == "current-large" {
                        *content = vec![Part::text(
                            "[curator] Tool output omitted to fit the context window. Request a smaller range if needed.",
                        )];
                    }
                }
            }
        }
        assert_eq!(
            outgoing.messages, expected,
            "{policy}: only historical image and oversized current body may change"
        );
        assert!(nexo_core::message::is_well_formed(&outgoing.messages));
        assert_fits(outgoing, 8_000, 4.0 / 3.0);
        r.stop().await;
    }
}

#[test]
fn audit_current_foreign_signature_serializer_omission() {
    for protocol in ["anthropic", "openai-responses"] {
        let cfg = cfg_window(8_000);
        let provider = &cfg.providers["mock"];
        let body = |req: &ModelRequest| {
            if protocol == "anthropic" {
                nexo_providers::anthropic::Anthropic::body(provider, req)
            } else {
                nexo_providers::openai_responses::OpenAiResponses::body(provider, req)
            }
        };
        let mut req = ModelRequest::new(
            "m1",
            "Keep system instructions.",
            vec![
                Message::user("Never delete files."),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning {
                            text: "FOREIGN_TEXT".into(),
                            signature: Some("FOREIGN_SIGNATURE".repeat(12_500)),
                            encrypted: Some("FOREIGN_ENCRYPTED".repeat(12_500)),
                            model: Some("other".into()),
                        },
                        Part::tool_call("current", "read", json!({"path":"data"})),
                    ],
                ),
                Message::tool_results(vec![Part::tool_result("current", "Keep this current result.", false)]),
            ],
        );
        assert!(
            nexo_core::message::estimate_tokens(&req.messages) > 8_000,
            "regression needs generic current-turn opaque counting"
        );
        let actual = body(&req);
        req.messages[1].content.remove(0);
        assert_eq!(actual, body(&req), "{protocol}: removing foreign reasoning must not change actual wire request");
        let wire = serde_json::to_string(&actual).unwrap();
        assert!(!wire.contains("FOREIGN_"));
        for text in ["Keep system instructions.", "Never delete files.", "current", "Keep this current result."] {
            assert!(wire.contains(text), "{protocol}: missing {text}");
        }
        assert!(wire.len() < 8_000);
    }
}

/// Cold full-loop admission of foreign reasoning produced in this tool round.
/// The real serializers, not a token estimate, establish that it is not transmitted.
#[tokio::test]
async fn audit_current_foreign_reasoning_cold_loop_serializer_omission() {
    // Hold the tool round open while a real model switch makes its reasoning foreign.
    struct SwitchRead {
        release: Arc<tokio::sync::Notify>,
        expected_args: Value,
        result: String,
    }
    #[async_trait]
    impl Tool for SwitchRead {
        fn name(&self) -> &str {
            "read"
        }
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "read".into(), description: "read policy".into(), parameters: json!({"type":"object"}) }
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Read
        }
        fn summary(&self, _args: &Value) -> String {
            "read policy".into()
        }
        async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
            assert_eq!(args, self.expected_args, "the tool must receive the fixture arguments unchanged");
            self.release.notified().await;
            ToolOutput::ok(self.result.clone())
        }
    }
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for protocol in ["anthropic", "openai-responses"] {
            let d = project();
            data_dir();
            std::fs::write(d.path().join("AGENTS.md"), "Keep system instructions unchanged.").unwrap();
            let path = d.path().join("policy.txt");
            // Freeze the oracle before running: neither the saved history nor the outgoing
            // request may supply the expected call arguments or tool result.
            let fixture_args = json!({"path": path});
            let fixture_result = "Keep this current result unchanged.";
            let fixture_call = Part::tool_call("current-foreign-read", "read", fixture_args.clone());
            let fixture_output = Part::ToolResult {
                call_id: "current-foreign-read".into(),
                name: Some("read".into()),
                content: vec![Part::text(fixture_result)],
                is_error: false,
            };
            std::fs::write(&path, fixture_result).unwrap();
            let mut old = Message::user("Never delete files; keep this historical instruction.");
            old.content.extend((0..6).map(|_| Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/not-fetched.png".into() },
            }));
            let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
            session.append(&old).unwrap();
            let id = session.id().to_string();
            drop(session);
            let foreign = Part::Reasoning {
                text: "FOREIGN_THINKING_MUST_NOT_REACH_WIRE".into(),
                signature: Some("FOREIGN_SIGNATURE".repeat(12_500)),
                encrypted: Some("FOREIGN_ENCRYPTED".repeat(12_500)),
                model: Some("m1".into()),
            };
            let Part::Reasoning { text: thinking, signature, encrypted, model } = &foreign else { unreachable!() };
            let mut events = vec![
                StreamEvent::Start { response_id: None, model: model.clone() },
                StreamEvent::ReasoningDelta(thinking.clone()),
                StreamEvent::ReasoningMeta { signature: signature.clone(), encrypted: encrypted.clone() },
            ];
            events.extend(uncounted(call("current-foreign-read", "read", fixture_args.clone())));
            let client = Scripted::new(vec![
                Reply::Events(events),
                Reply::Events(uncounted(text("Continued after foreign reasoning."))),
            ]);
            let mut cfg = cfg_window(8_000);
            cfg.context.policy = policy.into();
            cfg.providers.get_mut("mock").unwrap().protocol = protocol.into();
            cfg.providers.get_mut("mock").unwrap().context_window.insert("m2".into(), 8_000);
            let provider = cfg.providers["mock"].clone();
            let release = Arc::new(tokio::sync::Notify::new());
            let tools = ToolSet::new().with(Arc::new(SwitchRead {
                release: release.clone(),
                expected_args: fixture_args.clone(),
                result: fixture_result.into(),
            }));
            let mut r = start_with(client.clone(), cfg, Some(id.clone()), d.path(), tools).await;
            r.next().await;
            // Do not name policy.txt here: automatic file inclusion would make the
            // subsequent read legitimately deduplicate into an unchanged-file note.
            r.send(AgentCommand::Submit {
                text: "Read the requested policy; preserve all instructions.".into(),
                attachments: vec![],
            });
            tokio::time::timeout(Duration::from_secs(30), r.until(|e| matches!(e, AgentEvent::ToolStart { .. })))
                .await
                .unwrap();
            r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
            tokio::time::timeout(Duration::from_secs(30), r.until(|e| matches!(e, AgentEvent::Effort { .. })))
                .await
                .unwrap();
            release.notify_one();
            let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
            let sent = client.requests();
            assert_eq!(end_of(&evs), TurnEnd::Done, "{policy}/{protocol}: {:?}", notices(&evs));
            assert_eq!(sent.len(), 2, "{policy}/{protocol}: must send after the current tool result");
            let saved = nexo_agent::Session::open(&id).unwrap().messages().to_vec();
            assert_eq!(saved[0], old, "persistent images/instructions must not change");
            assert_eq!(sent[0].model, "m1");
            assert_eq!(sent[1].model, "m2");
            let outgoing = &sent[1];
            for (label, messages) in [("outgoing", &outgoing.messages), ("saved", &saved)] {
                let calls: Vec<_> =
                    messages.iter().flat_map(|m| &m.content).filter(|p| matches!(p, Part::ToolCall { .. })).collect();
                let results: Vec<_> =
                    messages.iter().flat_map(|m| &m.content).filter(|p| matches!(p, Part::ToolResult { .. })).collect();
                assert_eq!(calls, vec![&fixture_call], "{policy}/{protocol}/{label}: exact fixture call");
                assert_eq!(results, vec![&fixture_output], "{policy}/{protocol}/{label}: exact fixture result");
            }
            // Secondary consistency check only; the independent fixture checks above
            // also catch a mutation shared by persisted history and the request.
            let mut expected = saved[..saved.len() - 1].to_vec();
            omit_images(&mut expected[0].content);
            assert_eq!(outgoing.messages, expected, "{policy}/{protocol}: preserve the whole current turn");
            assert!(nexo_core::message::is_well_formed(&outgoing.messages));
            let reasoning_message = outgoing.messages.iter().position(|m| m.content.contains(&foreign)).unwrap();
            let current_user = outgoing
                .messages
                .iter()
                .position(|m| m.text().contains("Read the requested policy; preserve all instructions."))
                .unwrap();
            assert!(reasoning_message > current_user, "foreign reasoning must belong to the current turn");
            assert!(
                nexo_core::message::estimate_tokens(&outgoing.messages) > 8_000,
                "generic counting must exceed the window before serializer-aware omission"
            );
            let body = |req: &ModelRequest| {
                if protocol == "anthropic" {
                    nexo_providers::anthropic::Anthropic::body(&provider, req)
                } else {
                    nexo_providers::openai_responses::OpenAiResponses::body(&provider, req)
                }
            };
            let actual = body(outgoing);
            let mut without_foreign = outgoing.clone();
            without_foreign.messages[reasoning_message].content.retain(|p| p != &foreign);
            assert_eq!(actual, body(&without_foreign), "{policy}/{protocol}: omission must be exact on the real wire");
            // Independent admission arithmetic: all replies are uncounted, so this cold
            // 8k session uses the 4/3 fallback ratio, not a learned provider ratio.
            // Once the serializer-omitted block is removed there is no opaque reserve.
            // This is the documented heuristic, NOT serialized wire bytes as tokens.
            let estimated_input =
                fixed_tokens(&without_foreign) + nexo_core::message::estimate_tokens(&without_foreign.messages);
            let admitted_input = (estimated_input as f64 * (4.0 / 3.0)).ceil() as u64;
            let room = 8_000u64.checked_sub(admitted_input).expect("estimated input must fit");
            let ceiling = outgoing.output_ceiling.expect("known window must set an output ceiling");
            assert!(u64::from(ceiling) <= room, "{policy}/{protocol}: ceiling {ceiling} exceeds remaining {room}");
            assert!(outgoing.max_tokens >= 256, "{policy}/{protocol}: preserve a useful reply");
            assert!(outgoing.max_tokens <= ceiling, "{policy}/{protocol}: reply exceeds output ceiling");
            assert!(
                admitted_input + u64::from(outgoing.max_tokens) <= 8_000,
                "{policy}/{protocol}: estimated input {admitted_input} plus reply {} exceeds window",
                outgoing.max_tokens,
            );
            // Check the provider's actual output field, not only ModelRequest: a
            // serializer must not drop the limit or expand it beyond admission.
            let output_field = if protocol == "anthropic" { "max_tokens" } else { "max_output_tokens" };
            let wire_output = actual[output_field].as_u64().expect("wire output limit must be an unsigned integer");
            assert!(wire_output >= 256, "{policy}/{protocol}: wire must preserve a useful reply");
            assert!(wire_output <= u64::from(ceiling), "{policy}/{protocol}: wire reply exceeds output ceiling");
            assert!(
                wire_output <= room,
                "{policy}/{protocol}: estimated input {admitted_input} plus wire reply {wire_output} exceeds 8000",
            );

            // Select by structural type, not by fixture ID: a renamed, duplicated,
            // or missing call/result must fail even if its text occurs elsewhere.
            let (items, call_type, result_type) = if protocol == "anthropic" {
                let items = actual["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|m| m["content"].as_array().unwrap())
                    .collect::<Vec<_>>();
                (items, "tool_use", "tool_result")
            } else {
                (
                    actual["input"].as_array().unwrap().iter().collect::<Vec<_>>(),
                    "function_call",
                    "function_call_output",
                )
            };
            let calls: Vec<_> = items.iter().copied().filter(|v| v["type"] == call_type).collect();
            let results: Vec<_> = items.iter().copied().filter(|v| v["type"] == result_type).collect();
            assert_eq!(calls.len(), 1, "{policy}/{protocol}: exactly one serialized call");
            assert_eq!(results.len(), 1, "{policy}/{protocol}: exactly one serialized result");
            let call = calls[0];
            let result = results[0];
            assert_eq!(call["name"], "read", "{policy}/{protocol}: fixture tool name");
            if protocol == "anthropic" {
                assert_eq!(call["id"], "current-foreign-read", "{policy}/{protocol}: fixture call ID");
                assert_eq!(call["input"], fixture_args, "{policy}/{protocol}: fixture JSON arguments");
                assert_eq!(result["tool_use_id"], "current-foreign-read", "{policy}/{protocol}: fixture result ID");
                assert_eq!(result["content"], fixture_result, "{policy}/{protocol}: fixture result content");
                assert!(!result.get("is_error").and_then(Value::as_bool).unwrap_or(false));
            } else {
                assert_eq!(call["call_id"], "current-foreign-read", "{policy}/{protocol}: fixture call ID");
                let args: Value =
                    serde_json::from_str(call["arguments"].as_str().expect("arguments must be JSON text"))
                        .expect("arguments must decode as JSON");
                assert_eq!(args, fixture_args, "{policy}/{protocol}: fixture JSON arguments");
                assert_eq!(result["call_id"], "current-foreign-read", "{policy}/{protocol}: fixture result ID");
                assert_eq!(result["output"], fixture_result, "{policy}/{protocol}: fixture result content");
            }
            let wire = serde_json::to_string(&actual).unwrap();
            assert!(!wire.contains("FOREIGN_"), "{policy}/{protocol}: no foreign payload on wire");
            for instruction in [
                "Keep system instructions unchanged.",
                "Never delete files; keep this historical instruction.",
                "Read the requested policy; preserve all instructions.",
                "current-foreign-read",
                "Keep this current result unchanged.",
            ] {
                assert!(wire.contains(instruction), "{policy}/{protocol}: missing {instruction}");
            }
            assert_eq!(outgoing.system, sent[0].system, "system instructions must remain intact");
            r.stop().await;
        }
    }
}

/// Exercise admission and the actual protocol bodies for a current multimodal tool result.
#[tokio::test]
async fn mixed_image_tool_result_survives_small_window_wire_protocols() {
    struct Picture;
    #[async_trait]
    impl Tool for Picture {
        fn name(&self) -> &str {
            "picture"
        }
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "picture".into(),
                description: "inspect image".into(),
                parameters: json!({"type":"object"}),
            }
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Read
        }
        fn summary(&self, _: &Value) -> String {
            "inspect image".into()
        }
        async fn run(&self, _: Value, _: &ToolContext) -> ToolOutput {
            ToolOutput::ok("current image description").with_images(vec![nexo_core::message::ImageSource::Url {
                url: "https://example.invalid/current.png".into(),
            }])
        }
    }
    let _g = config_lock().lock().await;
    for protocol in ["anthropic", "openai-responses", "openai-chat", "gemini"] {
        let d = project();
        data_dir();
        let mut old = Message::user("Historical instruction: do not delete files.");
        old.content.extend((0..6).map(|_| Part::Image {
            source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/old.png".into() },
        }));
        let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
        session.append(&old).unwrap();
        let id = session.id().to_string();
        drop(session);
        let args = json!({"path":"current.png"});
        let client = Scripted::new(vec![
            Reply::Events(uncounted(call("pic-1", "picture", args.clone()))),
            Reply::Events(uncounted(text("done"))),
        ]);
        let mut cfg = cfg_window(8_000);
        cfg.context.policy = "plain".into();
        cfg.providers.get_mut("mock").unwrap().protocol = protocol.into();
        let provider = cfg.providers["mock"].clone();
        let mut r =
            start_with(client.clone(), cfg, Some(id.clone()), d.path(), ToolSet::new().with(Arc::new(Picture))).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "Inspect the current picture.".into(), attachments: vec![] });
        let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
        assert_eq!(end_of(&evs), TurnEnd::Done, "{protocol}: {:?}", notices(&evs));
        let sent = client.requests();
        assert_eq!(sent.len(), 2, "{protocol}");
        let req = &sent[1];
        assert_fits(req, 8_000, 4.0 / 3.0);
        assert_eq!(nexo_agent::Session::open(&id).unwrap().messages()[0], old);
        let body = match protocol {
            "anthropic" => nexo_providers::anthropic::Anthropic::body(&provider, req),
            "openai-responses" => nexo_providers::openai_responses::OpenAiResponses::body(&provider, req),
            "openai-chat" => nexo_providers::openai_chat::OpenAiChat::body(&provider, req),
            _ => nexo_providers::gemini::Gemini::body(&provider, req),
        };
        let wire = serde_json::to_string(&body).unwrap();
        for s in [
            "Historical instruction: do not delete files.",
            "Inspect the current picture.",
            "current image description",
            "https://example.invalid/current.png",
        ] {
            assert!(wire.contains(s), "{protocol} missing {s}: {wire}");
        }
        assert!(!wire.contains("https://example.invalid/old.png"), "{protocol}: historical images should be omitted");
        match protocol {
            "anthropic" => {
                let items: Vec<_> = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|m| m["content"].as_array().unwrap())
                    .collect();
                let call = items.iter().position(|p| p["type"] == "tool_use").unwrap();
                let result = items.iter().position(|p| p["type"] == "tool_result").unwrap();
                assert!(call < result);
                assert_eq!(items[call]["id"], "pic-1");
                assert_eq!(items[call]["input"], args);
                assert_eq!(items[result]["tool_use_id"], "pic-1");
                assert_eq!(items[result]["content"][1]["source"]["url"], "https://example.invalid/current.png");
            }
            "openai-responses" => {
                let items = body["input"].as_array().unwrap();
                let call = items.iter().position(|p| p["type"] == "function_call").unwrap();
                let result = items.iter().position(|p| p["type"] == "function_call_output").unwrap();
                assert!(call < result);
                assert_eq!(items[call]["call_id"], "pic-1");
                assert_eq!(serde_json::from_str::<Value>(items[call]["arguments"].as_str().unwrap()).unwrap(), args);
                assert_eq!(items[result]["call_id"], "pic-1");
                assert_eq!(items[result]["output"][1]["image_url"], "https://example.invalid/current.png");
            }
            "openai-chat" => {
                let items = body["messages"].as_array().unwrap();
                let call = items.iter().position(|p| p["tool_calls"].is_array()).unwrap();
                let result = items.iter().position(|p| p["role"] == "tool").unwrap();
                assert!(call < result);
                assert_eq!(items[call]["tool_calls"][0]["id"], "pic-1");
                assert_eq!(
                    serde_json::from_str::<Value>(
                        items[call]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()
                    )
                    .unwrap(),
                    args
                );
                assert_eq!(items[result]["tool_call_id"], "pic-1");
                assert_eq!(items[result + 1]["content"][0]["image_url"]["url"], "https://example.invalid/current.png");
            }
            _ => {
                let items: Vec<_> =
                    body["contents"].as_array().unwrap().iter().flat_map(|m| m["parts"].as_array().unwrap()).collect();
                let call = items.iter().position(|p| p.get("functionCall").is_some()).unwrap();
                let result = items.iter().position(|p| p.get("functionResponse").is_some()).unwrap();
                assert!(call < result);
                assert_eq!(items[call]["functionCall"]["name"], "picture");
                assert_eq!(items[call]["functionCall"]["args"], args);
                assert_eq!(items[result]["functionResponse"]["name"], "picture");
                assert_eq!(items[result + 1]["fileData"]["fileUri"], "https://example.invalid/current.png");
            }
        }
        r.stop().await;
    }
}

/// A real tool's oversized caption and images must be admitted together or not sent.
#[tokio::test]
async fn oversized_image_tool_result_is_never_sent_with_truncated_caption() {
    struct LargePicture;
    #[async_trait]
    impl Tool for LargePicture {
        fn name(&self) -> &str {
            "large_picture"
        }
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name().into(),
                description: "inspect pictures".into(),
                parameters: json!({"type":"object"}),
            }
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Read
        }
        fn summary(&self, _: &Value) -> String {
            "inspect pictures".into()
        }
        async fn run(&self, _: Value, _: &ToolContext) -> ToolOutput {
            ToolOutput::ok(format!("CAPTION_START {} CAPTION_END", "caption detail ".repeat(6000))).with_images(vec![
                nexo_core::message::ImageSource::Url { url: "https://example.invalid/one.png".into() },
                nexo_core::message::ImageSource::Url { url: "https://example.invalid/two.png".into() },
            ])
        }
    }
    let _g = config_lock().lock().await;
    let caption = format!("CAPTION_START {} CAPTION_END", "caption detail ".repeat(6000));
    for policy in ["plain", "curator", "auto"] {
        for window in [8_000, 200_000] {
            let d = project();
            data_dir();
            let client = Scripted::new(vec![
                Reply::Events(uncounted(call("large-1", "large_picture", json!({})))),
                Reply::Events(uncounted(text("done"))),
            ]);
            let mut cfg = cfg_window(window);
            cfg.context.policy = policy.into();
            let mut r =
                start_with(client.clone(), cfg, None, d.path(), ToolSet::new().with(Arc::new(LargePicture))).await;
            let AgentEvent::Ready { session_id: id, .. } = r.next().await else { panic!("no Ready") };
            r.send(AgentCommand::Submit { text: "Inspect both pictures.".into(), attachments: vec![] });
            let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
            let sent = client.requests();
            let saved = nexo_agent::Session::open(&id).unwrap();
            let result = saved
                .messages()
                .iter()
                .flat_map(|m| &m.content)
                .find(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == "large-1"))
                .expect("tool executed");
            let Part::ToolResult { content, .. } = result else { unreachable!() };
            assert_eq!(nexo_core::message::result_text(content), caption);
            assert_eq!(content.iter().filter(|p| matches!(p, Part::Image { .. })).count(), 2);
            if sent.len() == 1 {
                assert_eq!(end_of(&evs), TurnEnd::Error, "{policy}/{window}: {:?}", notices(&evs));
            } else {
                assert_eq!(sent.len(), 2, "{policy}/{window}: unexpected retry");
                assert_eq!(end_of(&evs), TurnEnd::Done);
                let outgoing = sent[1]
                    .messages
                    .iter()
                    .flat_map(|m| &m.content)
                    .find(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == "large-1"))
                    .expect("sent result");
                let Part::ToolResult { content: sent_content, .. } = outgoing else { unreachable!() };
                if window == 200_000 {
                    assert_eq!(
                        nexo_core::message::result_text(sent_content),
                        caption,
                        "{policy}/{window}: caption must not be shortened"
                    );
                    assert_eq!(outgoing, result, "{policy}/{window}: full caption and images in order");
                } else {
                    // a request never goes over the window: the text gives way whole, never cut in half,
                    // and both images stay in their place
                    let text = nexo_core::message::result_text(sent_content);
                    assert!(
                        text == caption || text.starts_with("[curator] Tool output omitted"),
                        "{policy}/{window}: caption cut in half: {text}"
                    );
                    assert_eq!(sent_content.iter().filter(|p| matches!(p, Part::Image { .. })).count(), 2);
                }
            }
            if window == 200_000 {
                assert_eq!(sent.len(), 2, "{policy}: large-window control must send");
            }
            r.stop().await;
        }
    }
}

#[tokio::test]
async fn user_image_and_long_text_are_sent_whole_or_rejected() {
    let _g = config_lock().lock().await;
    for policy in ["plain", "curator", "auto"] {
        for window in [8_000, 64_000, 200_000] {
            let d = project();
            std::fs::write(d.path().join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
            let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
            let mut cfg = cfg_window(window);
            cfg.context.policy = policy.into();
            let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
            r.next().await;
            let caption = (0..4000).map(|i| format!("line {i}: explain this image in detail\n")).collect::<String>();
            r.send(AgentCommand::Submit {
                text: caption.clone(),
                attachments: vec![nexo_core::agent::Attachment { path: PathBuf::from("shot.png") }],
            });
            let events = r.turn().await;
            let requests = client.requests();
            if requests.is_empty() {
                assert_eq!(end_of(&events), TurnEnd::Error, "{policy} {window}");
                assert!(window != 200_000, "large-window control rejected: {:?}", notices(&events));
            } else {
                assert_eq!(requests.len(), 1);
                let message = requests[0]
                    .messages
                    .iter()
                    .find(|m| m.content.iter().any(|p| matches!(p, Part::Image { .. })))
                    .expect("image must remain in request");
                assert!(message.text().contains(&caption), "{policy} {window}: user caption was truncated");
                assert_eq!(end_of(&events), TurnEnd::Done);
            }
            r.stop().await;
        }
    }
}

#[tokio::test]
async fn old_own_encrypted_reasoning_does_not_block_responses_request() {
    let _g = config_lock().lock().await;
    let d = project();
    data_dir();
    let opaque = Part::Reasoning {
        text: String::new(),
        signature: Some("rs_old".into()),
        encrypted: Some("E".repeat(188_423)),
        model: Some("m1".into()),
    };
    let mut session = nexo_agent::Session::create(d.path(), "mock", "m1").unwrap();
    session.append(&Message::user("Earlier request")).unwrap();
    session.append(&Message::new(Role::Assistant, vec![opaque, Part::text("Earlier answer")])).unwrap();
    let id = session.id().to_string();
    drop(session);
    let mut cfg = cfg_window(8_000);
    cfg.providers.get_mut("mock").unwrap().protocol = "openai-responses".into();
    let provider = cfg.providers["mock"].clone();
    let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
    let mut r = start_with(client.clone(), cfg, Some(id), d.path(), ToolSet::default()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "Continue".into(), attachments: vec![] });
    let evs = tokio::time::timeout(Duration::from_secs(30), r.turn()).await.unwrap();
    assert_eq!(end_of(&evs), TurnEnd::Done, "{:?}", notices(&evs));
    let sent = client.requests();
    assert_eq!(sent.len(), 1);
    let wire = nexo_providers::openai_responses::OpenAiResponses::body(&provider, &sent[0]).to_string();
    assert!(!wire.contains(&"E".repeat(1000)));
    assert!(wire.contains("Earlier answer"));
    r.stop().await;
}
