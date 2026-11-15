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
