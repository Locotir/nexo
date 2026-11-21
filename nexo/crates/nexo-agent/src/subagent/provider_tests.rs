use super::*;
use std::collections::BTreeMap;

struct Mute;
#[async_trait]
impl ModelClient for Mute {
    async fn stream(
        &self,
        _: &ProviderConfig,
        _: &ModelRequest,
        _: u32,
    ) -> Result<nexo_core::provider::EventStream, ProviderError> {
        Err(ProviderError::new(ErrorKind::Other, "mute"))
    }
}

fn cfg(default: &str, models: &[&str]) -> ProviderConfig {
    ProviderConfig {
        default_model: Some(default.into()),
        models: models.iter().map(|m| m.to_string()).collect(),
        ..Default::default()
    }
}

/// The most tokens the fixed part of a request may take in a window of 8k and 16k, and in one of 32k
/// (3375 in all three before the tools a small window cannot use well were left out; `ask_user`, which a
/// session adds, takes about 115 more than these tools).
const FIXED_8K: u64 = 1_900;
const FIXED_32K: u64 = 2_400;

/// The most characters the tools of a new session may take in a request (see the test that reads it). 11 472 until
/// the browser verbs (back to type_text, and `batch`) went in with 28 more; 11 595 with the four recording verbs
/// of the browser (`start_recording`, `stop_recording`, `export_gif{value?,gif?}`, `clear_recording`: 94 more).
/// `read_page` and its four parameters (filter, depth, ref_id, max_chars) add 186 to the browser's short text.
/// The browser's `open` action with its `profile` and `visible` added about 230, and `browsers`, `select_browser` and
/// `switch_browser` 94 more; trimming the browser's short text took 208 off (the rules for the person's own browser come in
/// the result of `select_browser`). `find{target}`, the mouse verbs with `modifiers?` and `network{to?}` add 107
/// (measured 12 009). Saying that `browsers` lists the person's logged-in browsers took 1 more (12 010). The capture
/// verbs add about 50 to the short text (estimated, not measured).
/// Integration 1118 with land/1117: the 12 144 of the merged re branches plus the 282 that spelling out the keys of an
/// `edit` change added (12 412 on land/1117 alone). Summed by hand at the merge, not measured yet; the first build
/// measured 12 671.
// Explicit task effort adds 100 measured wire characters.
const SCHEMA_CEILING: usize = 12_771;
/// The most characters `task` may take in a request with the nineteen kinds of [`OWN_KINDS`]: what it
/// takes now, 5107 when it went out whole.
const TASK_WITH_OWN_KINDS: usize = 2_009;
/// What those kinds may add to the tools of a session, over [`SCHEMA_CEILING`]: `task` takes 819 of it
/// with the two built-in kinds alone.
const OWN_KINDS_BUDGET: usize = TASK_WITH_OWN_KINDS - 819;

fn map(list: &[(&str, ProviderConfig)]) -> BTreeMap<String, ProviderConfig> {
    list.iter().map(|(n, c)| (n.to_string(), c.clone())).collect()
}

#[test]
fn a_model_listed_by_one_other_provider_runs_there() {
    let all = map(&[("a", cfg("a1", &["a2"])), ("b", cfg("b1", &["shared-b"])), ("c", cfg("c1", &[]))]);
    let main = cfg("a1", &["a2"]);
    assert_eq!(provider_of("b1", ("a", &main), &all), Ok(Some("b")));
    assert_eq!(provider_of("shared-b", ("a", &main), &all), Ok(Some("b")));
}

#[test]
fn a_model_of_the_main_provider_stays_there() {
    // also when another provider lists it too
    let all = map(&[("a", cfg("a1", &["both"])), ("b", cfg("b1", &["both"]))]);
    let main = cfg("a1", &["both"]);
    assert_eq!(provider_of("a1", ("a", &main), &all), Ok(None));
    assert_eq!(provider_of("both", ("a", &main), &all), Ok(None));
}

#[test]
fn a_model_listed_by_two_others_is_an_error_naming_both() {
    let all = map(&[("a", cfg("a1", &[])), ("b", cfg("x", &[])), ("c", cfg("c1", &["x"]))]);
    let main = cfg("a1", &[]);
    assert_eq!(provider_of("x", ("a", &main), &all), Err(vec!["b", "c"]));
}

#[test]
fn a_model_listed_nowhere_stays_on_the_main_provider() {
    let all = map(&[("a", cfg("a1", &[])), ("b", cfg("b1", &[]))]);
    let main = cfg("a1", &[]);
    assert_eq!(provider_of("unknown", ("a", &main), &all), Ok(None));
}

#[test]
fn the_model_lookup_ignores_ascii_case() {
    let all = map(&[("a", cfg("a1", &[])), ("b", cfg("GPT-Five", &[]))]);
    let main = cfg("a1", &[]);
    assert_eq!(provider_of("gpt-five", ("a", &main), &all), Ok(Some("b")));
    assert_eq!(provider_of("A1", ("a", &main), &all), Ok(None));
}

#[test]
fn the_provider_text_does_not_grow_with_the_providers() {
    let mut all = BTreeMap::new();
    for p in 0..23 {
        let models: Vec<String> = (1..6).map(|m| format!("model-{p}-{m}")).collect();
        all.insert(
            format!("prov{p:02}"),
            ProviderConfig { default_model: Some(format!("model-{p}-0")), models, ..Default::default() },
        );
    }
    let target = Target::new("prov00", all["prov00"].clone(), "model-0-0");
    let sub = SubAgent::new(Arc::new(Mute), target, &ToolSet::new(), Vec::new(), Default::default())
        .with_providers(all.clone());
    let spec = sub.spec();
    let desc = spec.parameters["properties"]["provider"]["description"].as_str().unwrap().to_string();
    let total = spec.description.len() + serde_json::to_string(&spec.parameters).unwrap().len();
    eprintln!("TASK_SPEC_BYTES {total} provider_desc {}", desc.len());
    assert!(desc.len() < 260, "{} chars: {desc}", desc.len());
    assert!(!desc.contains("model-"), "{desc}");
    assert!(!desc.contains("Set up"), "{desc}");
}

#[tokio::test]
async fn any_provider_name_is_offered_and_an_unknown_one_is_answered_with_the_names_there_are() {
    let all = map(&[("alpha", cfg("a1", &[])), ("beta", cfg("b1", &[])), ("gamma", cfg("g1", &[]))]);
    let target = Target::new("alpha", all["alpha"].clone(), "a1");
    let sub =
        SubAgent::new(Arc::new(Mute), target, &ToolSet::new(), Vec::new(), Default::default()).with_providers(all);
    // the schema takes a name as it is written: the list of them is not sent with every request
    let spec = sub.spec();
    let offered = &spec.parameters["properties"]["provider"];
    assert_eq!(offered["type"], "string");
    assert!(offered.get("enum").is_none(), "{offered}");
    assert!(!spec.description.contains("beta") && !offered.to_string().contains("beta"), "{spec:?}");
    let dir = tempfile::tempdir().unwrap();
    let out = sub.run(json!({"prompt": "look", "provider": "nope"}), &ToolContext::new(dir.path().into())).await;
    assert!(out.is_error, "{out:?}");
    assert_eq!(out.content, "no provider `nope` is set up; there are: alpha, beta, gamma");
}

#[test]
fn clean_launch_excludes_shared_memory_even_when_profile_lists_it() {
    let mut tools = nexo_tools::builtin();
    tools.add(Arc::new(crate::share_tool::ReadSharedMemory));
    tools.add(Arc::new(crate::share_tool::WriteSharedMemory));
    let sub =
        SubAgent::new(Arc::new(Mute), Target::new("p", cfg("m", &[]), "m"), &tools, Vec::new(), Default::default());
    let general = builtin().into_iter().find(|p| p.name == "general").unwrap();
    let profile = Profile {
        tools: Some(vec!["shared_memory_read".into(), "shared_memory_write".into(), "read".into()]),
        ..general
    };
    let normal = sub.tools_for_launch(&profile, false);
    assert!(normal.get("shared_memory_read").is_some());
    assert!(normal.get("shared_memory_write").is_some());
    let clean = sub.tools_for_launch(&profile, true);
    assert!(clean.get("shared_memory_read").is_none());
    assert!(clean.get("shared_memory_write").is_none());
    assert!(clean.get("read").is_some());
}

#[test]
fn a_general_sub_agent_leaves_note_todo_and_recall_to_its_parent() {
    let tools = nexo_tools::builtin();
    let providers = nexo_providers::presets::presets();
    let (name, main) = providers.iter().next().map(|(n, c)| (n.clone(), c.clone())).unwrap();
    let sub = SubAgent::new(Arc::new(Mute), Target::new(name, main, "m"), &tools, Vec::new(), Default::default());
    let names = |p: &Profile| -> Vec<String> { sub.tools_for(p).specs().iter().map(|s| s.name.clone()).collect() };
    let general = builtin().into_iter().find(|p| p.name == "general").unwrap();
    let g = names(&general);
    for gone in ["note", "todo", "recall"] {
        assert!(!g.iter().any(|n| n == gone), "{gone} in {g:?}");
    }
    assert!(g.iter().any(|n| n == "jobs") && g.iter().any(|n| n == "bash"), "{g:?}");
    let listed = Profile { tools: Some(vec!["note".into()]), ..general };
    assert_eq!(names(&listed), vec!["note".to_string()]);
}

#[test]
fn a_sub_agent_gets_the_session_mail_tools_only_when_its_kind_lists_them() {
    let dir = tempfile::tempdir().unwrap();
    let mailbox = nexo_core::mailbox::Mailbox::open(
        &dir.path().join("mail"),
        "aaaa1111",
        "api",
        "/w/api",
        false,
        nexo_core::mail::Inbound::Auto,
    )
    .unwrap();
    let mail = nexo_tools::mail::tools(mailbox);
    let mut tools = nexo_tools::builtin();
    for spec in mail.specs() {
        tools.add(mail.get(&spec.name).unwrap().clone());
    }
    let sub =
        SubAgent::new(Arc::new(Mute), Target::new("p", cfg("m", &[]), "m"), &tools, Vec::new(), Default::default());
    let names = |p: &Profile| -> Vec<String> { sub.tools_for(p).specs().iter().map(|s| s.name.clone()).collect() };
    let general = builtin().into_iter().find(|p| p.name == "general").unwrap();
    let g = names(&general);
    for gone in ["send_message", "send_to_session", "list_sessions"] {
        assert!(!g.iter().any(|n| n == gone), "{gone} in {g:?}");
    }
    assert!(g.iter().any(|n| n == "bash"), "{g:?}");
    let listed = Profile { tools: Some(vec!["send_message".into(), "list_sessions".into()]), ..general };
    assert_eq!(names(&listed), ["list_sessions", "send_message"]);
}

/// What a new session offers in an empty folder: the built-in tools and `task`, every preset provider set up.
fn default_tools() -> ToolSet {
    tools_with(Vec::new())
}

/// The tools of a new session whose sub-agents are of these `kinds` (none: the built-in two).
fn tools_with(kinds: Vec<Profile>) -> ToolSet {
    let providers = nexo_providers::presets::presets();
    let (name, main) = providers.iter().next().map(|(n, c)| (n.clone(), c.clone())).unwrap();
    let mut tools = nexo_tools::builtin();
    let sub = SubAgent::new(Arc::new(Mute), Target::new(name, main, "m"), &tools, kinds, Default::default())
        .with_providers(providers);
    tools.add(Arc::new(sub));
    tools
}

/// Nineteen kinds of sub-agent as a person keeps them in `~/.claude/agents`: made up, each described in
/// about 150 characters (2 756 in all).
const OWN_KINDS: [(&str, &str); 19] = [
    (
        "api-designer",
        "Designs REST and gRPC interfaces before any code is written. Use when a feature needs new endpoints, a versioning plan or a review of request shapes.",
    ),
    (
        "bug-hunter",
        "Debugging specialist for crashes, stack traces and flaky tests; reproduces the failure first, then narrows it to one cause and proposes the smallest fix.",
    ),
    (
        "changelog-writer",
        "Writes release notes from the commits since the last tag: groups them by area, drops internal noise and keeps each entry to one plain sentence.",
    ),
    (
        "code-reviewer",
        "Use this agent after a change is written to review the diff for correctness, missed edge cases, naming and tests. It reports findings by severity.",
    ),
    (
        "db-migrator",
        "Plans and writes database migrations (PostgreSQL, SQLite), checks they can be rolled back and flags locks that would stall a busy production table.",
    ),
    (
        "dependency-auditor",
        "Checks the dependency tree for known vulnerabilities, abandoned packages and licence conflicts, and proposes the smallest safe set of upgrades.",
    ),
    (
        "docs-writer",
        "Technical writer for READMEs, guides and API references. Use when behaviour changed and the documentation must follow, or a page reads unclear.",
    ),
    (
        "frontend-polisher",
        "Refines UI code - spacing, states, keyboard access and responsive breakpoints - without changing behaviour; reports every file it touched.",
    ),
    (
        "i18n-checker",
        "Finds hard-coded strings, missing translation keys and broken plural forms across the locale files, then lists them by screen with a suggested key.",
    ),
    (
        "infra-planner",
        "Use proactively when a task touches deployment: reads the Terraform and CI files, explains what would change and warns about anything irreversible.",
    ),
    (
        "log-analyst",
        "Reads large log files and traces to find when an incident started, what errors cluster around it and which service emitted the first bad line.",
    ),
    (
        "perf-profiler",
        "Performance specialist: measures before guessing, finds the hot path with a profiler or timing probes, and reports the gain of each proposed change.",
    ),
    (
        "refactor-planner",
        "Plans a refactor as a sequence of small safe steps, each with its own check, and names the callers that would break at every step of the way.",
    ),
    (
        "release-manager",
        "Prepares a release: bumps versions, verifies the build on every target, checks the changelog and lists what still blocks the tag from going out.",
    ),
    (
        "security-reviewer",
        "Reviews code for injection, broken access control, secrets in the tree and unsafe deserialisation. Read-only; reports each issue with a location.",
    ),
    (
        "sql-tuner",
        "Explains slow queries from their plans, proposes indexes or rewrites and estimates the cost of each on write-heavy tables before recommending one.",
    ),
    (
        "test-writer",
        "Writes focused unit and integration tests for code that has none, following the patterns of the suite in place, and runs them until they pass.",
    ),
    (
        "ticket-triager",
        "Reads an issue report, reproduces it when it can, labels its area and severity, and writes the first reply asking only for what is truly missing.",
    ),
    (
        "ux-critic",
        "Critiques a screen or flow as a first-time user would meet it: what is unclear, what takes too many steps and which words mislead. No code changes.",
    ),
];

/// The kinds a session has with [`OWN_KINDS`] as files in the person's `~/.claude/agents`, read as a
/// session reads them.
fn own_kinds() -> Vec<Profile> {
    let (home, cfg, proj) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let dir = home.path().join(".claude/agents");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, description) in OWN_KINDS {
        let text = format!(
            "---\nname: {name}\ndescription: {description}\ntools: Read, Grep, Glob, Bash\nmodel: inherit\n---\nYou are {name}.\n"
        );
        std::fs::write(dir.join(format!("{name}.md")), text).unwrap();
    }
    let kinds = load_from(proj.path(), cfg.path(), &cfg.path().join("trusted-projects"), Some(home.path()));
    assert_eq!(kinds.len(), 2 + OWN_KINDS.len(), "{kinds:?}");
    for (name, description) in OWN_KINDS {
        assert_eq!(kinds.iter().find(|k| k.name == name).unwrap().description, description, "{name}");
    }
    kinds
}

/// The characters `specs` take as the `tools` of a chat request.
fn wire_chars(specs: &[ToolSpec]) -> usize {
    specs.iter().map(|s| json!({"type": "function", "function": s}).to_string().len()).sum()
}

#[test]
fn the_default_tools_keep_under_their_schema_ceiling_with_the_rare_ones_still_offered() {
    let tools = default_tools();
    let specs = tools.specs_for(false);
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "bash",
            "browser",
            "diagnostics",
            "edit",
            "glob",
            "grep",
            "jobs",
            "ls",
            "note",
            "read",
            "recall",
            "task",
            "todo",
            "web_fetch",
            "web_search",
            "write"
        ]
    );
    // every request of a session carries these characters. 13619 before the tools seldom called went out
    // with their short text; the ceiling is what they take now, so a text that grows is seen here
    // (Windows: the PowerShell line of bash is longer, 44 chars on CI).
    // `bash` names the shell: PowerShell on CI takes 44 characters more than `sh -c`, so 64 of room.
    let ceiling = SCHEMA_CEILING + if cfg!(windows) { 64 } else { 0 };
    let chars = wire_chars(&specs);
    let each: Vec<String> =
        specs.iter().map(|s| format!("{} {}", s.name, wire_chars(std::slice::from_ref(s)))).collect();
    assert!(chars <= ceiling, "the default tools take {chars} characters, over {ceiling}: {}", each.join(", "));
    // the rare ones: the short text, and every parameter of the full one with its type
    for name in ["task", "diagnostics", "recall"] {
        let tool = tools.get(name).unwrap();
        let (short, full) = (specs.iter().find(|s| s.name == name).unwrap(), tool.spec());
        assert!(tool.rare(), "{name}");
        assert_eq!(short.description, tool.compact().unwrap().description, "{name}");
        assert!(wire_chars(std::slice::from_ref(short)) < wire_chars(std::slice::from_ref(&full)), "{name}");
        assert_eq!(short.parameters["required"], full.parameters["required"], "{name}");
        let (got, want) = (&short.parameters["properties"], full.parameters["properties"].as_object().unwrap());
        assert_eq!(got.as_object().unwrap().len(), want.len(), "{name}");
        for (param, schema) in want {
            assert_eq!(got[param]["type"], schema["type"], "{name}.{param}");
        }
    }
    assert_eq!(
        specs.iter().find(|s| s.name == "task").unwrap().parameters["properties"]["agent"]["enum"],
        json!(["explore", "general"])
    );
}

#[test]
fn task_keeps_its_short_text_with_nineteen_kinds_of_the_persons_own() {
    let tools = tools_with(own_kinds());
    let full = tools.get("task").unwrap().spec();
    // one text for every window, as for the two built-in kinds
    for small_window in [false, true] {
        let specs = tools.specs_for(small_window);
        let task = specs.iter().find(|s| s.name == "task").unwrap();
        let chars = wire_chars(std::slice::from_ref(task));
        assert!(
            chars <= TASK_WITH_OWN_KINDS,
            "task takes {chars} characters, over {TASK_WITH_OWN_KINDS}: {}",
            task.description
        );
        for name in ["explore", "general"].into_iter().chain(OWN_KINDS.iter().map(|k| k.0)) {
            assert!(task.description.contains(&format!("`{name}`")), "{name} is not named: {}", task.description);
        }
        // the call takes what the whole text says it takes
        assert_eq!(task.parameters["required"], full.parameters["required"]);
        let (got, want) = (&task.parameters["properties"], full.parameters["properties"].as_object().unwrap());
        assert_eq!(got.as_object().unwrap().len(), want.len());
        for (param, schema) in want {
            assert_eq!(got[param]["type"], schema["type"], "{param}");
        }
    }
    let specs = tools.specs_for(false);
    let chars = wire_chars(&specs);
    // 15924 when `task` went out whole (Windows: see the ceiling of the default tools)
    let ceiling = SCHEMA_CEILING + OWN_KINDS_BUDGET + if cfg!(windows) { 64 } else { 0 };
    let each: Vec<String> =
        specs.iter().map(|s| format!("{} {}", s.name, wire_chars(std::slice::from_ref(s)))).collect();
    assert!(chars <= ceiling, "the tools take {chars} characters, over {ceiling}: {}", each.join(", "));
}

/// The tokens (nexo's estimator) of what every request carries whatever the work: the built-in text with
/// its tool guidance, and the tools as the wire gets them, for a model with context window `window`. The
/// directory, date and project block and the person's instruction files are not in it.
fn fixed_tokens(tools: &ToolSet, window: u64) -> u64 {
    let cfg = nexo_core::config::AgentConfig { hermetic: true, ..Default::default() };
    let system = crate::prompt::system_prompt_with(
        std::path::Path::new("/w"),
        &cfg,
        tools,
        Some(window),
        &crate::prompt::Volatile::default(),
    );
    let specs: Vec<ToolSpec> = tools
        .specs_for(crate::prompt::is_compact(Some(window)))
        .into_iter()
        .filter(|s| !crate::prompt::left_out(Some(window), &s.name))
        .collect();
    let wire: Vec<_> = specs.iter().map(|s| json!({"type": "function", "function": s})).collect();
    nexo_core::message::estimate_tokens(&[
        nexo_core::message::Message::user(system),
        nexo_core::message::Message::user(json!(wire).to_string()),
    ])
}

/// The tools of a session with sub-agents and sixteen skills the way a person keeps them.
fn session_tools() -> ToolSet {
    let mut tools = default_tools();
    let skills = (0..16)
        .map(|i| nexo_core::skills::Skill {
            name: format!("skill-number-{i}"),
            description: "Use when the person asks for something of this kind and wants the steps spelled out for it, with the usual checks and the files it touches.".into(),
            dir: std::path::PathBuf::from("/skills"),
            allowed_tools: vec![],
            source: ".claude/skills".into(),
        })
        .collect();
    tools.add(nexo_tools::skill::tool(skills).unwrap());
    tools
}

#[test]
fn the_fixed_part_of_a_request_is_small_in_a_small_window_and_the_same_as_before_in_a_large_one() {
    let tools = session_tools();
    let (w8, w16, w32, w33, w128) = (
        fixed_tokens(&tools, 8_000),
        fixed_tokens(&tools, 16_000),
        fixed_tokens(&tools, crate::prompt::COMPACT_WINDOW),
        fixed_tokens(&tools, crate::prompt::COMPACT_WINDOW + 1),
        fixed_tokens(&tools, 128_000),
    );
    eprintln!("fixed tokens: 8k {w8}, 16k {w16}, 32k {w32}, 33k {w33}, 128k {w128}");
    assert!(w8 <= FIXED_8K && w16 <= FIXED_8K, "8k {w8}, 16k {w16}, over {FIXED_8K}");
    assert!(w32 <= FIXED_32K, "32k {w32} over {FIXED_32K}");
    // above 32k nothing is shortened and no tool is left out: the request is the one of a window that
    // is not known (the prompt cache and every current user keep their bytes)
    assert_eq!(w33, w128);
    assert!(w128 > 2 * w32, "{w128} {w32}");
    let unknown = tools.specs_for(false);
    for w in [crate::prompt::COMPACT_WINDOW + 1, 128_000, 1_000_000] {
        let offered: Vec<ToolSpec> = tools
            .specs_for(crate::prompt::is_compact(Some(w)))
            .into_iter()
            .filter(|s| !crate::prompt::left_out(Some(w), &s.name))
            .collect();
        assert_eq!(json!(offered), json!(unknown), "{w}");
    }
    let cfg = nexo_core::config::AgentConfig { hermetic: true, ..Default::default() };
    let (cwd, vol) = (std::path::Path::new("/w"), crate::prompt::Volatile::default());
    let none = crate::prompt::system_prompt_with(cwd, &cfg, &tools, None, &vol);
    for w in [crate::prompt::COMPACT_WINDOW + 1, 128_000] {
        assert_eq!(crate::prompt::system_prompt_with(cwd, &cfg, &tools, Some(w), &vol), none, "{w}");
    }
}

#[test]
fn a_small_window_is_not_offered_sub_agents_notes_or_the_task_list_and_a_tiny_one_no_skills_or_web() {
    let tools = session_tools();
    let names = |w: u64| -> Vec<String> {
        tools
            .specs_for(true)
            .into_iter()
            .filter(|s| !crate::prompt::left_out(Some(w), &s.name))
            .map(|s| s.name)
            .collect()
    };
    let tiny = names(crate::prompt::TINY_WINDOW);
    for kept in ["bash", "read", "write", "edit", "grep", "glob", "ls", "jobs", "recall"] {
        assert!(tiny.iter().any(|n| n == kept), "{kept} is missing: {tiny:?}");
    }
    for gone in
        ["task", "workflow", "skill", "todo", "note", "debug", "diagnostics", "web_search", "web_fetch", "browser"]
    {
        assert!(!tiny.iter().any(|n| n == gone), "{gone} is offered: {tiny:?}");
    }
    // a window of 32k keeps the skills and the web, and not the sub-agents and the notes
    let small = names(crate::prompt::COMPACT_WINDOW);
    assert!(["skill", "web_search", "bash", "jobs"].iter().all(|k| small.iter().any(|n| n == k)), "{small:?}");
    assert!(["task", "todo", "note"].iter().all(|k| !small.iter().any(|n| n == k)), "{small:?}");
}

#[tokio::test]
async fn an_unknown_kind_is_answered_with_every_kind_and_its_whole_description() {
    let tools = tools_with(own_kinds());
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().into());
    let out = tools.get("task").unwrap().run(json!({"prompt": "look", "agent": "reviewer"}), &ctx).await;
    assert!(out.is_error, "{out:?}");
    assert!(out.content.starts_with("no sub-agent `reviewer`; there are:\n"), "{}", out.content);
    for p in builtin() {
        assert!(out.content.contains(&format!("\n- {}: {}", p.name, p.description)), "{}: {}", p.name, out.content);
    }
    for (name, description) in OWN_KINDS {
        assert!(out.content.contains(&format!("\n- {name}: {description}")), "{name}: {}", out.content);
    }
}

#[tokio::test]
async fn empty_task_requires_prompt_and_never_returns_a_catalog() {
    let all = map(&[("alpha", cfg("a1", &[])), ("beta", cfg("b1", &["b2"]))]);
    let sub = SubAgent::new(
        Arc::new(Mute),
        Target::new("alpha", all["alpha"].clone(), "active"),
        &ToolSet::new(),
        Vec::new(),
        Default::default(),
    )
    .with_providers(all);
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().into());
    for args in [json!({}), json!({"prompt": ""}), json!({"prompt": "  "})] {
        let out = sub.run(args, &ctx).await;
        assert!(out.is_error && out.content.contains("prompt"), "{out:?}");
        assert!(!out.content.contains("b2"));
    }
}

#[tokio::test]
async fn invalid_task_effort_is_rejected_before_starting_a_job() {
    let sub = SubAgent::new(
        Arc::new(Mute),
        Target::new("p", cfg("m", &[]), "m"),
        &ToolSet::new(),
        Vec::new(),
        Default::default(),
    );
    assert!(sub.spec().parameters["properties"]["effort"]["enum"].as_array().unwrap().iter().any(|v| v == "max"));
    assert!(sub.compact().unwrap().params.iter().any(|(k, _)| *k == "effort"));
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path().into());
    for effort in [json!("ultra"), json!(""), json!(42), json!(null)] {
        let out = sub.run(json!({"prompt": "look", "effort": effort}), &ctx).await;
        assert!(out.is_error && out.content.contains("`effort`"), "{out:?}");
        assert!(ctx.jobs.subs().list().is_empty());
    }
}
