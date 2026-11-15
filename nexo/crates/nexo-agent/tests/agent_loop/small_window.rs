//! A small window: what nexo adds on its own (instruction files, a sub-agent's task and reply) is made to fit,
//! and the model is told what was cut.

use super::*;

/// The estimate nexo itself uses: four characters a token, over the system prompt and the tool text.
fn fixed_tokens(r: &ModelRequest) -> u64 {
    (r.system.chars().count() + serde_json::to_string(&r.tools).unwrap().chars().count()) as u64 / 4
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
    assert_eq!(req.max_tokens, 4_096);
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

/// What the 30-tool server's session sends on a window of `w` tokens: the first request, and what its two calls
/// to the last tool (right arguments, then wrong ones) answered.
#[cfg(unix)]
async fn big_mcp_session(w: u32) -> (ModelRequest, Vec<String>) {
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
    data_dir();
    let (handle, ends) = channels();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "mcp__big__tool_29", json!({"q": "x"}))),
        Reply::Events(call("c2", "mcp__big__tool_29", json!({"fail": true}))),
        Reply::Events(text("done")),
    ]);
    let setup = AgentSetup {
        config: cfg_window(w),
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: nexo_tools::builtin(),
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
    let outputs = ["c1", "c2"]
        .iter()
        .map(|c| {
            evs.iter()
                .find_map(|e| match e {
                    AgentEvent::ToolDone { id, output, .. } if id == c => Some(output.content.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        })
        .collect();
    (client.requests()[0].clone(), outputs)
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
    let (req, outputs) = big_mcp_session(8_000).await;
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
    let (req, outputs) = big_mcp_session(200_000).await;
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

fn over_window(evs: &[AgentEvent]) -> Vec<String> {
    notices(evs).into_iter().filter(|n| n.starts_with("request over the window")).collect()
}

#[tokio::test]
async fn a_64k_read_under_the_plain_policy_at_8k_is_sent_from_the_curators_view_and_fits_with_its_reply() {
    let _g = config_lock().lock().await;
    let (reqs, evs) = a_big_read(Some(8_000), "plain").await;
    let last = reqs.last().unwrap();
    let total = total_with_reply(last);
    assert!(total <= 8_000, "the request after the read and its reply take {total} tokens of 8000");
    assert!(last.max_tokens >= 256, "the reply keeps room for a call: {}", last.max_tokens);
    assert!(last.messages.iter().any(|m| m.text().contains("look at the file big.rs")), "the person's words stay");
    assert!(over_window(&evs).is_empty(), "{:?}", notices(&evs));
}

#[tokio::test]
async fn a_request_that_cannot_fit_keeps_the_persons_message_whole_and_says_once_what_it_holds() {
    let _g = config_lock().lock().await;
    let d = project();
    let words = "please look at the parser and say why it fails on empty input ".repeat(600);
    let client = Scripted::new(vec![Reply::Events(uncounted(text("ok")))]);
    let mut cfg = cfg_window(8_000);
    cfg.context.policy = "plain".into();
    let mut r = start_with(client.clone(), cfg, None, d.path(), nexo_tools::builtin()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: words.clone(), attachments: vec![] });
    let evs = r.turn().await;
    r.stop().await;
    let req = &client.requests()[0];
    assert!(req.messages.iter().any(|m| m.text().contains(words.trim())), "the person's message goes whole");
    assert_eq!(req.max_tokens, 256, "the reply is asked at its floor");
    let told = over_window(&evs);
    assert_eq!(told.len(), 1, "{:?}", notices(&evs));
    let n = &told[0];
    for part in ["for a 8000-token window (system ", ", tools ", ", conversation ", ", reply 256)"] {
        assert!(n.contains(part), "{part:?} in {n}");
    }
    assert!(n.contains("the largest part is your message (message "), "{n}");
}

#[tokio::test]
async fn the_same_plain_session_at_200k_sends_the_requests_it_sent_before() {
    let _g = config_lock().lock().await;
    let (at_200k, evs) = a_big_read(Some(200_000), "plain").await;
    assert!(over_window(&evs).is_empty(), "{:?}", notices(&evs));
    // with no window known nothing is fitted: the same session at 200k sends the same messages and reply size
    let (unknown, _) = a_big_read(None, "plain").await;
    assert_eq!(at_200k.len(), unknown.len());
    for (a, b) in at_200k.iter().zip(&unknown) {
        assert_eq!(serde_json::to_string(&a.messages).unwrap(), serde_json::to_string(&b.messages).unwrap());
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

#[tokio::test]
async fn a_plain_session_at_8k_with_room_for_a_short_reply_only_goes_from_the_curators_view_with_a_useful_reply() {
    let _g = config_lock().lock().await;
    // the size of read result that leaves the request at about 7 350 of 8 000 tokens: room for a 300-token reply,
    // none for 1 000
    // (measured with no window known, less what the system prompt and tools at 8k leave out)
    let (open, at_4k) = a_plain_read_of(4_000, None).await;
    let (_, at_8k) = a_plain_read_of(8_000, None).await;
    let (small, _) = a_plain_read_of(10, Some(8_000)).await;
    let fixed_less = fixed_tokens(&open[0]) - fixed_tokens(&small[0]);
    let per_char = (at_8k - at_4k) as f64 / 4_000.0;
    let chars = 4_000 + ((7_350.0 + fixed_less as f64 - at_4k as f64) / per_char) as usize;
    let (whole, est) = a_plain_read_of(chars, None).await;
    let est = est - fixed_less;
    assert!(est > 7_000 && est + 300 <= 8_000, "the request takes {est} tokens at 8k");
    let (reqs, _) = a_plain_read_of(chars, Some(8_000)).await;
    let last = reqs.last().unwrap();
    assert!(last.max_tokens >= 1_000, "the reply has room for an edit: {}", last.max_tokens);
    let total = total_with_reply(last);
    assert!(total <= 8_000, "the request and its reply take {total} tokens of 8000");
    let size = |r: &ModelRequest| serde_json::to_string(&r.messages).unwrap().len();
    assert!(size(last) < size(whole.last().unwrap()), "the curator's view is shorter than the whole read");
    assert!(last.messages.iter().any(|m| m.text().contains("look at lib.rs")), "the person's words stay");
    // at 200k the same session sends what it sends with no window known
    let (at_200k, _) = a_plain_read_of(chars, Some(200_000)).await;
    assert_eq!(at_200k.len(), whole.len());
    for (a, b) in at_200k.iter().zip(&whole) {
        assert_eq!(serde_json::to_string(&a.messages).unwrap(), serde_json::to_string(&b.messages).unwrap());
        assert_eq!(a.max_tokens, b.max_tokens);
    }
}
