//! A sub-agent sends what the session's context policy keeps of its own messages: under the model's window
//! however long it runs, everything while that fits and pays, and `recall` answered from its own history.

use super::*;
use std::sync::Mutex;

use nexo_core::config::ContextConfig;
use nexo_core::message::estimate_tokens;
use nexo_core::provider::EventStream;

/// Replies with a call of `read` (its `n` the request's place) until `calls` of them, then a call of `recall` when
/// asked to, then text; keeps every request.
struct Scripted {
    calls: usize,
    recall: Option<&'static str>,
    seen: Mutex<Vec<ModelRequest>>,
}

impl Scripted {
    fn new(calls: usize, recall: Option<&'static str>) -> Arc<Scripted> {
        Arc::new(Scripted { calls, recall, seen: Mutex::new(Vec::new()) })
    }
}

fn call(name: &str, args: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: "c".into(), name: name.into() },
        StreamEvent::ToolCallEnd { id: "c".into(), name: name.into(), args },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

#[async_trait]
impl ModelClient for Scripted {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let n = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(req.clone());
            seen.len() - 1
        };
        let events = if n < self.calls {
            call("read", json!({ "n": n }))
        } else if let (true, Some(m)) = (n == self.calls, self.recall) {
            call("recall", json!({ "message": m }))
        } else {
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// A read whose output is `words` distinct words naming the call.
struct Read(usize);

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "read".into(), description: "reads".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "read".into()
    }
    async fn run(&self, args: Value, _: &ToolContext) -> ToolOutput {
        let n = args["n"].as_u64().unwrap_or(0);
        ToolOutput::ok((0..self.0).map(|w| format!("out{n}w{w}")).collect::<Vec<_>>().join(" "))
    }
}

const BRIEF_TASK: &str = "Find every caller of parse_header and say which of them ignore its error.";

/// Runs an `explore` sub-agent (17 requests at most) over `client` with the context `policy`, on a model
/// with a window of `window` tokens (0: none), and gives back its answer.
async fn run(client: Arc<Scripted>, policy: &str, window: u32, words: usize) -> ToolOutput {
    let mut cfg = ProviderConfig { default_model: Some("m".into()), ..Default::default() };
    if window > 0 {
        cfg.context_window.insert("m".into(), window);
    }
    let mut tools = ToolSet::new();
    tools.add(Arc::new(Read(words)));
    let sub = SubAgent::new(client, Target::new("p", cfg, "m"), &tools, Vec::new(), Default::default())
        .with_context(ContextConfig { policy: policy.into(), ..Default::default() });
    let dir = tempfile::tempdir().unwrap();
    sub.run(json!({ "prompt": BRIEF_TASK, "agent": "explore" }), &ToolContext::new(dir.path().into())).await
}

#[tokio::test]
async fn a_long_sub_agent_run_never_sends_more_than_the_window() {
    let window = 24_000u32;
    let client = Scripted::new(16, None);
    // 16 outputs of about 2500 tokens: 40 000 in all, far past the window
    let out = run(client.clone(), "curator", window, 1_000).await;
    assert!(!out.is_error, "{out:?}");
    let seen = client.seen.lock().unwrap();
    assert_eq!(seen.len(), 17, "every round ran");
    // the window less the reply the run asked for (the main agent's size, held to a quarter of this small window)
    let room = u64::from(window - seen[0].max_tokens);
    let sent: Vec<u64> =
        seen.iter().map(|r| crate::turn::fixed_tokens(&r.system, &r.tools) + estimate_tokens(&r.messages)).collect();
    let plain = crate::turn::fixed_tokens(&seen[16].system, &seen[16].tools) + 16 * 2_500;
    assert!(plain > room, "the run must outgrow the window to test anything: {plain}");
    assert!(sent.iter().all(|&t| t <= room), "a request passed the window of {room}: {sent:?}");
    // the brief and the task stay whole in every request, cut or not
    for r in seen.iter() {
        assert!(r.messages[0].text().contains(BRIEF_TASK), "{:?}", r.messages[0]);
    }
}

#[tokio::test]
async fn plain_and_compact_refuse_an_overgrown_history_before_sending_it() {
    for policy in ["plain", "compact"] {
        let client = Scripted::new(16, None);
        let out = run(client.clone(), policy, 24_000, 1_000).await;
        assert!(out.is_error && out.content.contains("request not sent"), "{policy}: {out:?}");
        assert!(out.content.contains("what they changed stays"), "progress must survive: {out:?}");
        let seen = client.seen.lock().unwrap();
        assert!(seen.len() > 1 && seen.len() < 17, "{policy}: {} requests", seen.len());
        for req in seen.iter() {
            assert_request_fits(req, 24_000);
            assert!(req.messages[0].text().contains(BRIEF_TASK));
        }
    }
}

fn assert_request_fits(req: &ModelRequest, window: u32) {
    let raw = crate::turn::fixed_tokens(&req.system, &req.tools) + estimate_tokens(&req.messages);
    let conservative = (raw as f64 * 4.0 / 3.0).ceil() as u64;
    assert!(
        conservative + u64::from(req.max_tokens) <= u64::from(window),
        "{conservative} + {} > {window}",
        req.max_tokens
    );
    assert_eq!(req.output_ceiling, Some(req.max_tokens));
}

/// Deliver mail during the first stream, so the next round must recheck even a policy's fresh view.
struct MailClient {
    inner: Arc<Scripted>,
    subs: nexo_core::subs::Subs,
    parent: bool,
    text: String,
}

#[async_trait]
impl ModelClient for MailClient {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError> {
        let first = self.inner.seen.lock().unwrap().is_empty();
        let stream = self.inner.stream(cfg, req, retries).await?;
        if first {
            let id = self.subs.list()[0].id.clone();
            if self.parent {
                self.subs.submit_from_parent(&id, &self.text).unwrap();
            } else {
                self.subs.submit(&id, &self.text).unwrap();
            }
        }
        Ok(stream)
    }
}

#[tokio::test]
async fn oversized_mail_is_refused_before_the_next_provider_call() {
    for policy in ["plain", "compact", "curator"] {
        for parent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let ctx = ToolContext::new(dir.path().into());
            let inner = Scripted::new(0, None);
            let client = Arc::new(MailClient {
                inner: inner.clone(),
                subs: ctx.jobs.subs(),
                parent,
                text: "MAIL_WORDS_MUST_NOT_BE_SILENTLY_TRUNCATED ".repeat(3000),
            });
            let mut cfg = ProviderConfig::default();
            cfg.context_window.insert("m".into(), 8192);
            let sub =
                SubAgent::new(client, Target::new("p", cfg, "m"), &ToolSet::new(), Vec::new(), Default::default())
                    .with_context(ContextConfig { policy: policy.into(), ..Default::default() });
            let out = sub.run(json!({"prompt": BRIEF_TASK, "agent":"explore"}), &ctx).await;
            assert!(out.is_error && out.content.contains("request not sent"), "{policy}, parent={parent}: {out:?}");
            let seen = inner.seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "oversized mail must not reach the provider");
            assert_request_fits(&seen[0], 8192);
        }
    }
}

/// A cut reply exercises the larger second attempt and the real serializers without a network call.
struct CutClient(Mutex<Vec<ModelRequest>>);
#[async_trait]
impl ModelClient for CutClient {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let mut seen = self.0.lock().unwrap();
        seen.push(req.clone());
        let finish = if seen.len() == 1 { FinishReason::Length } else { FinishReason::Stop };
        Ok(Box::pin(futures::stream::iter(vec![
            StreamEvent::TextDelta("report".into()),
            StreamEvent::Finish(finish),
            StreamEvent::Done,
        ])))
    }
}

fn assert_wire_ceiling(req: &ModelRequest) {
    use nexo_providers::{
        anthropic::Anthropic, gemini::Gemini, openai_chat::OpenAiChat, openai_responses::OpenAiResponses,
    };
    let cfg = ProviderConfig::default();
    let mut req = req.clone();
    // Large request-only overlays must not override a worker's physical/reserved room either.
    req.extra.insert("max_tokens".into(), json!(1_000_000));
    req.extra.insert("max_completion_tokens".into(), json!(1_000_000));
    req.extra.insert("max_output_tokens".into(), json!(1_000_000));
    req.extra.insert("generationConfig".into(), json!({"maxOutputTokens":1_000_000}));
    let bodies = [
        (Anthropic::body(&cfg, &req), "max_tokens"),
        (OpenAiChat::body(&cfg, &req), "max_tokens"),
        (OpenAiResponses::body(&cfg, &req), "max_output_tokens"),
    ];
    for (body, key) in bodies {
        assert_eq!(body[key].as_u64(), Some(u64::from(req.max_tokens)), "{key}: {body}");
    }
    assert_eq!(Gemini::body(&cfg, &req)["generationConfig"]["maxOutputTokens"], json!(req.max_tokens));
}

#[tokio::test]
async fn worker_output_ceiling_reaches_providers_and_grows_only_on_a_safe_retry() {
    for window in [None, Some(4096)] {
        let dir = tempfile::tempdir().unwrap();
        let client = Arc::new(CutClient(Mutex::new(Vec::new())));
        let mut cfg = ProviderConfig::default();
        if let Some(window) = window {
            cfg.context_window.insert("claude-sonnet-4-5".into(), window);
        }
        cfg.max_output.insert("claude-sonnet-4-5".into(), 1500);
        let sub = SubAgent::new(
            client.clone(),
            Target::new("p", cfg, "claude-sonnet-4-5"),
            &ToolSet::new(),
            Vec::new(),
            Default::default(),
        )
        .with_max_tokens(600);
        let out = sub.run(json!({"prompt":BRIEF_TASK, "agent":"explore"}), &ToolContext::new(dir.path().into())).await;
        assert!(!out.is_error, "{out:?}");
        let seen = client.0.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].max_tokens, 600);
        assert!(seen[1].max_tokens > 600 && seen[1].max_tokens <= 1200);
        for req in seen.iter() {
            assert_eq!(req.output_ceiling, Some(req.max_tokens));
            if let Some(w) = window {
                assert_request_fits(req, w);
            }
            assert_wire_ceiling(req);
        }
    }
}

struct BudgetHost;
#[async_trait]
impl Host for BudgetHost {
    async fn permit(
        &self,
        _: &str,
        _: &str,
        _: &dyn Tool,
        _: &Value,
        _: &CancellationToken,
        _: Option<crate::hooks::Decision>,
    ) -> Result<Why, (String, Why)> {
        Ok(Why::RULE)
    }
    async fn reserve(
        &self,
        _: &str,
        _: Option<Pricing>,
        _: u64,
        _: u32,
    ) -> Result<(u32, crate::budget::Flight), String> {
        Ok((300, crate::budget::Flight::default()))
    }
    async fn spent(&self, _: &str, _: Usage, _: Option<Pricing>, _: bool) -> f64 {
        0.0
    }
}

#[tokio::test]
async fn a_spend_reservation_is_a_hard_wire_ceiling_even_for_modern_claude() {
    let dir = tempfile::tempdir().unwrap();
    let client = Arc::new(CutClient(Mutex::new(Vec::new())));
    let sub = SubAgent::new(
        client.clone(),
        Target::new("p", ProviderConfig::default(), "claude-sonnet-4-5"),
        &ToolSet::new(),
        Vec::new(),
        Default::default(),
    )
    .with_max_tokens(4096);
    sub.host.set(Arc::new(BudgetHost)).ok().unwrap();
    let out = sub.run(json!({"prompt":BRIEF_TASK, "agent":"explore"}), &ToolContext::new(dir.path().into())).await;
    assert!(!out.is_error, "{out:?}");
    let seen = client.0.lock().unwrap();
    assert_eq!(seen.len(), 1, "a reserve-limited cut is not retried");
    assert_eq!(seen[0].max_tokens, 300);
    assert_eq!(seen[0].output_ceiling, Some(300));
    assert_wire_ceiling(&seen[0]);
}

#[tokio::test]
async fn a_short_sub_agent_run_sends_the_same_messages_as_plain() {
    let (cur, plain) = (Scripted::new(4, None), Scripted::new(4, None));
    run(cur.clone(), "curator", 128_000, 50).await;
    run(plain.clone(), "plain", 128_000, 50).await;
    let (cur, plain) = (cur.seen.lock().unwrap(), plain.seen.lock().unwrap());
    assert_eq!(cur.len(), plain.len());
    for (c, p) in cur.iter().zip(plain.iter()) {
        assert_eq!(serde_json::to_string(&c.messages).unwrap(), serde_json::to_string(&p.messages).unwrap());
        assert_eq!(c.system, p.system);
    }
    // the curator offers `recall`, plain does not
    assert!(cur[0].tools.iter().any(|t| t.name == "recall"));
    assert!(!plain[0].tools.iter().any(|t| t.name == "recall"));
}

#[tokio::test]
async fn recall_is_answered_from_the_sub_agents_own_messages() {
    // m00003 is the output of the first read
    let client = Scripted::new(2, Some("m00003"));
    run(client.clone(), "curator", 128_000, 30).await;
    let seen = client.seen.lock().unwrap();
    let last = seen.last().unwrap().messages.last().unwrap();
    let text = match last.content.as_slice() {
        [Part::ToolResult { content, is_error: false, .. }] => content
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>(),
        other => panic!("not one result: {other:?}"),
    };
    assert!(text.contains("out0w0") && text.contains("out0w29"), "{text}");
}

#[tokio::test]
async fn explicit_skill_is_private_and_failures_stop_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let folder = nexo_core::paths::canonical(dir.path()).unwrap().join("private-skill");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("SKILL.md"), "---\nname: private-skill\ndescription: test\n---\nSECRET_SKILL_BODY\n")
        .unwrap();
    let skill = nexo_core::skills::Skill {
        name: "private-skill".into(),
        description: "test".into(),
        dir: folder,
        allowed_tools: vec![],
        source: "test".into(),
    };
    let mut tools = ToolSet::new();
    tools.add(nexo_tools::skill::tool(vec![skill]).unwrap());
    tools.add(Arc::new(nexo_tools::read::Read));
    let client = Scripted::new(0, None);
    let mut cfg = ProviderConfig { default_model: Some("m".into()), ..Default::default() };
    cfg.context_window.insert("m".into(), 32_000);
    let sub = SubAgent::new(client.clone(), Target::new("p", cfg, "m"), &tools, Vec::new(), Default::default());
    let ctx = ToolContext::new(dir.path().into());
    let out = sub.run(json!({"prompt":"do it", "agent":"explore", "start_clean":true, "skill":"private-skill", "skill_arguments":"ARG_MARKER"}), &ctx).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(!out.content.contains("SECRET_SKILL_BODY"));
    {
        let seen = client.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(!seen[0].system.contains("SECRET_SKILL_BODY"));
        assert!(!seen[0].messages[0].text().contains("SECRET_SKILL_BODY"));
        let Part::ToolResult { content, .. } = &seen[0].messages[2].content[0] else { panic!("not skill tool output") };
        let task = nexo_core::message::result_text(content);
        assert!(task.contains("SECRET_SKILL_BODY") && task.contains("ARG_MARKER"));
    }
    let out = sub.run(json!({"prompt":"do it", "agent":"explore", "skill":"missing"}), &ctx).await;
    assert!(out.is_error && out.content.contains("missing"), "{}", out.content);
    assert_eq!(client.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn explicit_skill_requires_profile_tool_and_fits_window() {
    let dir = tempfile::tempdir().unwrap();
    let folder = nexo_core::paths::canonical(dir.path()).unwrap().join("huge");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("SKILL.md"),
        format!("---\nname: huge\ndescription: test\n---\n{}", "large content ".repeat(10000)),
    )
    .unwrap();
    let skill = nexo_core::skills::Skill {
        name: "huge".into(),
        description: "test".into(),
        dir: folder,
        allowed_tools: vec![],
        source: "test".into(),
    };
    let mut tools = ToolSet::new();
    tools.add(nexo_tools::skill::tool(vec![skill]).unwrap());
    tools.add(Arc::new(nexo_tools::read::Read));
    let client = Scripted::new(0, None);
    let mut cfg = ProviderConfig { default_model: Some("m".into()), ..Default::default() };
    cfg.context_window.insert("m".into(), 8192);
    let sub = SubAgent::new(client.clone(), Target::new("p", cfg.clone(), "m"), &tools, Vec::new(), Default::default());
    let ctx = ToolContext::new(dir.path().into());
    let out = sub.run(json!({"prompt":"do it", "agent":"explore", "skill":"huge"}), &ctx).await;
    assert!(out.is_error && out.content.contains("does not fit"), "{}", out.content);
    let sub =
        SubAgent::new(client.clone(), Target::new("p", cfg, "m"), &ToolSet::new(), Vec::new(), Default::default());
    let out = sub.run(json!({"prompt":"do it", "agent":"explore", "skill":"huge"}), &ctx).await;
    assert!(out.is_error && out.content.contains("no tool"), "{}", out.content);
    assert!(client.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn builtin_skill_can_be_loaded_in_worker() {
    let dir = tempfile::tempdir().unwrap();
    let mut tools = ToolSet::new();
    tools.add(nexo_tools::skill::tool(nexo_core::skills::discover(dir.path(), false)).unwrap());
    let client = Scripted::new(0, None);
    let cfg = ProviderConfig { default_model: Some("m".into()), ..Default::default() };
    let sub = SubAgent::new(client.clone(), Target::new("p", cfg, "m"), &tools, Vec::new(), Default::default());
    let out = sub
        .run(json!({"prompt":"do it", "agent":"explore", "skill":"research"}), &ToolContext::new(dir.path().into()))
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(format!("{:?}", client.seen.lock().unwrap()[0].messages).contains("Prefer primary sources"));
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_skill_is_refused_before_model_request() {
    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    std::fs::write(external.path().join("secret"), "SECRET_SYMLINK_BODY").unwrap();
    let folder = nexo_core::paths::canonical(dir.path()).unwrap().join("skill");
    std::fs::create_dir(&folder).unwrap();
    std::os::unix::fs::symlink(external.path().join("secret"), folder.join("SKILL.md")).unwrap();
    let skill = nexo_core::skills::Skill {
        name: "linked".into(),
        description: "test".into(),
        dir: folder,
        allowed_tools: vec![],
        source: "test".into(),
    };
    let mut tools = ToolSet::new();
    tools.add(nexo_tools::skill::tool(vec![skill]).unwrap());
    tools.add(Arc::new(nexo_tools::read::Read));
    let client = Scripted::new(0, None);
    let cfg = ProviderConfig { default_model: Some("m".into()), ..Default::default() };
    let sub = SubAgent::new(client.clone(), Target::new("p", cfg, "m"), &tools, Vec::new(), Default::default());
    let out = sub
        .run(json!({"prompt":"do it", "agent":"explore", "skill":"linked"}), &ToolContext::new(dir.path().into()))
        .await;
    assert!(out.is_error, "{}", out.content);
    assert!(!out.content.contains("SECRET_SYMLINK_BODY"));
    assert!(client.seen.lock().unwrap().is_empty());
}

struct AllowSkill;
#[async_trait]
impl Host for AllowSkill {
    async fn permit(
        &self,
        _: &str,
        _: &str,
        _: &dyn Tool,
        _: &Value,
        _: &CancellationToken,
        _: Option<crate::hooks::Decision>,
    ) -> Result<Why, (String, Why)> {
        Ok(Why::RULE)
    }
    async fn spent(&self, _: &str, _: Usage, _: Option<Pricing>, _: bool) -> f64 {
        0.0
    }
}

struct RewriteSkill;
#[async_trait]
impl Host for RewriteSkill {
    async fn permit(
        &self,
        _: &str,
        _: &str,
        _: &dyn Tool,
        _: &Value,
        _: &CancellationToken,
        _: Option<crate::hooks::Decision>,
    ) -> Result<Why, (String, Why)> {
        Ok(Why::RULE)
    }
    async fn hook(&self, event: &str, subject: &str, _: Value) -> crate::hooks::Outcome {
        if event == "pre_tool" && subject == "skill" {
            crate::hooks::Outcome {
                updated_input: Some(json!({"name":"research", "arguments":"rewritten"})),
                ..Default::default()
            }
        } else {
            Default::default()
        }
    }
    async fn spent(&self, _: &str, _: Usage, _: Option<Pricing>, _: bool) -> f64 {
        0.0
    }
}

#[tokio::test]
async fn explicit_skill_history_uses_hook_rewritten_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut tools = ToolSet::new();
    tools.add(nexo_tools::skill::tool(nexo_core::skills::discover(dir.path(), false)).unwrap());
    let client = Scripted::new(0, None);
    let host: HostCell = Default::default();
    host.set(Arc::new(RewriteSkill)).ok().unwrap();
    let sub = SubAgent::new(client.clone(), Target::new("p", ProviderConfig::default(), "m"), &tools, Vec::new(), host);
    let out = sub
        .run(json!({"prompt":"do it", "agent":"explore", "skill":"original"}), &ToolContext::new(dir.path().into()))
        .await;
    assert!(!out.is_error, "{}", out.content);
    let seen = client.seen.lock().unwrap();
    let Part::ToolCall { args, .. } = &seen[0].messages[1].content[0] else { panic!("missing call") };
    assert_eq!(args["name"], "research");
    assert_eq!(args["arguments"], "rewritten");
    let Part::ToolResult { content, .. } = &seen[0].messages[2].content[0] else { panic!("missing result") };
    assert!(nexo_core::message::result_text(content).contains("Prefer primary sources"));
}

#[tokio::test]
async fn explicit_skill_untracked_in_worktree_is_refused() {
    worktree_skill(false).await;
}

#[tokio::test]
async fn explicit_skill_tracked_in_worktree_uses_worker_copy() {
    worktree_skill(true).await;
}

async fn worktree_skill(tracked: bool) {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
        &["commit", "--allow-empty", "-qm", "initial"],
    ] {
        assert!(std::process::Command::new("git").current_dir(dir.path()).args(args).status().unwrap().success());
    }
    let folder = nexo_core::paths::canonical(dir.path()).unwrap().join(".nexo/skills/local");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("SKILL.md"), "---\nname: local\ndescription: test\n---\nUNTRACKED_WORKTREE_SKILL\n")
        .unwrap();
    if tracked {
        for args in [&["add", ".nexo/skills/local/SKILL.md"][..], &["commit", "-qm", "add skill"]] {
            assert!(std::process::Command::new("git").current_dir(dir.path()).args(args).status().unwrap().success());
        }
    }
    let original_folder = folder.clone();
    let skill = nexo_core::skills::Skill {
        name: "local".into(),
        description: "test".into(),
        dir: folder,
        allowed_tools: vec![],
        source: ".nexo/skills".into(),
    };
    let mut tools = ToolSet::new();
    tools.add(nexo_tools::skill::tool(vec![skill]).unwrap());
    tools.add(Arc::new(nexo_tools::read::Read));
    tools.add(Arc::new(nexo_tools::write::Write));
    let client = Scripted::new(0, None);
    let mut profile = builtin().into_iter().find(|p| p.name == "general").unwrap();
    profile.isolation = true;
    let host: HostCell = Default::default();
    host.set(Arc::new(AllowSkill)).ok().unwrap();
    let sub =
        SubAgent::new(client.clone(), Target::new("p", ProviderConfig::default(), "m"), &tools, vec![profile], host);
    let out = sub
        .run(json!({"prompt":"do it", "agent":"general", "skill":"local"}), &ToolContext::new(dir.path().into()))
        .await;
    let seen = client.seen.lock().unwrap();
    if tracked {
        assert!(!out.is_error, "{}", out.content);
        let messages = format!("{:?}", seen[0].messages);
        assert!(messages.contains("UNTRACKED_WORKTREE_SKILL"));
        assert!(!messages.contains(&original_folder.display().to_string()));
        assert!(messages.contains(".nexo/skills/local"));
    } else {
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("unavailable in the worker worktree"), "{}", out.content);
        assert!(seen.is_empty());
    }
}

/// Codex chooses its own remote output limit. Exercise the local guard without a server.
struct UncappedClient {
    mode: &'static str,
    seen: Mutex<Vec<ModelRequest>>,
    polled: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl ModelClient for UncappedClient {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        retries: u32,
    ) -> Result<EventStream, ProviderError> {
        assert_eq!(retries, 0, "no hidden client retries for an uncapped worker");
        let body = nexo_providers::openai_responses::OpenAiResponses::body(cfg, req);
        assert!(body.get("max_output_tokens").is_none(), "Codex rejects this field");
        let n = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(req.clone());
            seen.len()
        };
        if n == 1 {
            return Ok(Box::pin(futures::stream::iter(call("read", json!({"n":0})))));
        }
        // The previous completed tool pair and original task must survive unchanged.
        assert!(req.messages[0].text().contains(BRIEF_TASK));
        assert!(req.messages.iter().flat_map(|m| &m.content).any(|p| matches!(p, Part::ToolResult { .. })));
        let polled = self.polled.clone();
        let mode = self.mode;
        if mode == "wire_arguments" {
            let events = futures::stream::unfold(0usize, move |i| {
                let polled = polled.clone();
                async move {
                    polled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let data = if i == 0 {
                        json!({"type":"response.output_item.added", "output_index":0,
                            "item":{"type":"function_call", "call_id":"partial", "name":"read"}})
                    } else {
                        json!({"type":"response.function_call_arguments.delta", "output_index":0, "delta":"x".repeat(500)})
                    };
                    Some((Ok(nexo_providers::sse::SseEvent { data: data.to_string(), ..Default::default() }), i + 1))
                }
            });
            return Ok(nexo_providers::openai_responses::OpenAiResponses::parse(events));
        }
        Ok(Box::pin(futures::stream::unfold(0usize, move |i| {
            let polled = polled.clone();
            async move {
                polled.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let ev = match (mode, i) {
                    ("normal", 0) | ("length", 0) => StreamEvent::TextDelta("bounded report".into()),
                    ("normal", 1) => StreamEvent::Finish(FinishReason::Stop),
                    ("length", 1) => StreamEvent::Finish(FinishReason::Length),
                    ("normal" | "length", _) => StreamEvent::Done,
                    ("usage", 0) => StreamEvent::Usage(Usage { output: 10000, ..Default::default() }),
                    ("error", _) => StreamEvent::Error(ProviderError::new(ErrorKind::Network, "broke")),
                    ("reason", _) => StreamEvent::ReasoningDelta("x".repeat(500)),
                    ("meta", _) => StreamEvent::ReasoningMeta { signature: None, encrypted: Some("x".repeat(500)) },
                    ("arguments", 0) => StreamEvent::ToolCallStart { id: "unfinished".into(), name: "read".into() },
                    ("arguments", _) => {
                        StreamEvent::ToolCallDelta { id: "unfinished".into(), partial_json: "x".repeat(500) }
                    }
                    ("end", _) => StreamEvent::ToolCallEnd {
                        id: "c".into(),
                        name: "read".into(),
                        args: json!({"huge":"x".repeat(5000)}),
                    },
                    ("events", _) => StreamEvent::ReasoningDelta(String::new()),
                    _ => StreamEvent::TextDelta("x".repeat(500)),
                };
                Some((ev, i + 1))
            }
        })))
    }
}

#[tokio::test]
async fn codex_uncapped_stream_is_bounded_terminal_and_keeps_previous_tool_progress() {
    for mode in
        ["text", "reason", "meta", "arguments", "wire_arguments", "end", "events", "usage", "error", "normal", "length"]
    {
        let dir = tempfile::tempdir().unwrap();
        let polled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let client = Arc::new(UncappedClient { mode, seen: Mutex::new(Vec::new()), polled: polled.clone() });
        let mut cfg = ProviderConfig::default();
        cfg.extra.insert("omit_max_tokens".into(), toml::Value::Boolean(true));
        let mut tools = ToolSet::new();
        tools.add(Arc::new(Read(10)));
        let sub = SubAgent::new(client.clone(), Target::new("p", cfg, "m"), &tools, Vec::new(), Default::default())
            .with_max_tokens(128)
            .with_context(ContextConfig { policy: "plain".into(), ..Default::default() });
        let out = sub.run(json!({"prompt":BRIEF_TASK, "agent":"explore"}), &ToolContext::new(dir.path().into())).await;
        assert_eq!(client.seen.lock().unwrap().len(), 2, "{mode}: must not resend a guarded or cut response");
        assert!(polled.load(std::sync::atomic::Ordering::SeqCst) <= 1025, "{mode}: stopped polling");
        if matches!(mode, "normal" | "length") {
            assert!(!out.is_error && out.content.contains("bounded report"), "{mode}: {out:?}");
        } else {
            assert!(out.is_error && out.content.contains("what they changed stays"), "{mode}: {out:?}");
            if mode != "error" {
                assert!(
                    out.content.contains("local stream guard") && out.content.contains("Remote generation/cost"),
                    "{mode}: {out:?}"
                );
            }
        }
    }
}
