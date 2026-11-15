//! Pins of every vendor fact the crate answers today, per place, so that moving the facts into one table
//! cannot change an answer. Each assertion is the answer before the table existed.

use crate::{catalogue, directory, keys, presets, probe};

/// Every id that appears in any of the places, plus one nobody knows.
const IDS: &[&str] = &[
    "openai",
    "anthropic",
    "gemini",
    "deepseek",
    "xai",
    "groq",
    "mistral",
    "openrouter",
    "together",
    "fireworks",
    "cerebras",
    "moonshot",
    "qwen",
    "zhipu",
    "minimax",
    "ollama",
    "lmstudio",
    "vllm",
    "llamacpp",
    "copilot",
    "codex",
    "google",
    "togetherai",
    "fireworks-ai",
    "moonshotai",
    "alibaba",
    "alibaba-cn",
    "zhipuai",
    "zai",
    "github-copilot",
    "llama.cpp",
    "no-such-vendor",
];

#[test]
fn key_pages_are_the_ones_known_today() {
    let want: &[(&str, &str)] = &[
        ("openai", "https://platform.openai.com/api-keys"),
        ("anthropic", "https://platform.claude.com/settings/keys"),
        ("gemini", "https://aistudio.google.com/apikey"),
        ("deepseek", "https://platform.deepseek.com/api_keys"),
        ("openrouter", "https://openrouter.ai/keys"),
        ("groq", "https://console.groq.com/keys"),
        ("mistral", "https://console.mistral.ai/api-keys"),
        ("together", "https://api.together.ai/settings/api-keys"),
    ];
    for id in IDS {
        let w = want.iter().find(|(n, _)| n == id).map(|(_, u)| *u);
        assert_eq!(keys::page(id), w, "{id}");
    }
}

#[test]
fn key_starts_name_the_service_they_belong_to() {
    let tail = "madeupmadeupmadeup";
    let want: &[(&str, &str)] = &[
        ("s\u{6b}-ant-", "anthropic"),
        ("s\u{6b}-or-v1-", "openrouter"),
        ("s\u{6b}-proj-", "openai"),
        ("gsk_", "groq"),
        ("AIza", "gemini"),
        ("cs\u{6b}-", "cerebras"),
    ];
    for (start, name) in want {
        assert_eq!(keys::recognise(&format!("{start}{tail}")), Some(*name), "{start}");
        assert!(keys::starts_like_key(&format!("{start}x"), 1), "{start}");
        assert_eq!(
            keys::starts_like_key(start, 1),
            start.starts_with("s\u{6b}-"),
            "{start} alone is a key start only through the shared s\u{6b}-"
        );
        // exactly 8 characters after the start name nothing yet, 9 do
        assert_eq!(keys::recognise(&format!("{start}12345678")), None, "{start}");
        assert_eq!(keys::recognise(&format!("{start}123456789")), Some(*name), "{start}");
    }
    assert_eq!(keys::recognise("s\u{6b}-0123456789abcdef"), None);
    assert!(keys::starts_like_key("s\u{6b}-ab", 2) && !keys::starts_like_key("s\u{6b}-ab", 3));
    assert!(!keys::starts_like_key("gpt-5", 0) && !keys::starts_like_key("", 0));
    assert_eq!(keys::recognise("  gsk_madeupmadeupmade "), Some("groq"), "surrounding spaces are ignored");
}

#[test]
fn catalogue_ids_map_to_presets_as_before() {
    let want: &[(&str, &str)] = &[
        ("google", "gemini"),
        ("togetherai", "together"),
        ("fireworks-ai", "fireworks"),
        ("moonshotai", "moonshot"),
        ("alibaba", "qwen"),
        ("alibaba-cn", "qwen"),
        ("zhipuai", "zhipu"),
        ("zai", "zhipu"),
        ("llama.cpp", "llamacpp"),
        ("lmstudio", "lmstudio"),
    ];
    for id in IDS {
        let w = want.iter().find(|(n, _)| n == id).map(|(_, p)| *p).unwrap_or(id);
        assert_eq!(catalogue::preset_name(id), w, "{id}");
    }
    let ids = |p: &str| catalogue::catalogue_ids(p);
    assert_eq!(ids("gemini"), ["gemini", "google"]);
    assert_eq!(ids("together"), ["together", "togetherai"]);
    assert_eq!(ids("fireworks"), ["fireworks", "fireworks-ai"]);
    assert_eq!(ids("moonshot"), ["moonshot", "moonshotai"]);
    assert_eq!(ids("qwen"), ["qwen", "alibaba", "alibaba-cn"]);
    assert_eq!(ids("zhipu"), ["zhipu", "zhipuai", "zai"]);
    assert_eq!(ids("llamacpp"), ["llamacpp", "llama.cpp"]);
    assert_eq!(ids("lmstudio"), ["lmstudio"]);
    assert_eq!(ids("openai"), ["openai"]);
    assert_eq!(ids("copilot"), ["copilot"], "the catalogue maps nothing to copilot");
    assert_eq!(ids("no-such-vendor"), ["no-such-vendor"]);
}

#[test]
fn directory_ids_stand_for_presets_as_before() {
    let want: &[(&str, &str)] = &[
        ("google", "gemini"),
        ("togetherai", "together"),
        ("fireworks-ai", "fireworks"),
        ("moonshotai", "moonshot"),
        ("alibaba", "qwen"),
        ("zhipuai", "zhipu"),
        ("zai", "zhipu"),
        ("github-copilot", "copilot"),
        ("lmstudio", "lmstudio"),
    ];
    for id in IDS {
        let w = want.iter().find(|(n, _)| n == id).map(|(_, p)| *p);
        assert_eq!(directory::alias(id), w, "{id}");
    }
    // the two alias tables disagree on purpose: the catalogue knows `alibaba-cn` and `llama.cpp`, the
    // directory knows `github-copilot`
    assert_eq!(directory::alias("alibaba-cn"), None);
    assert_eq!(directory::alias("llama.cpp"), None);
    assert_eq!(catalogue::preset_name("github-copilot"), "github-copilot");
}

#[test]
fn presets_hold_the_protocol_address_and_key_variable_they_held() {
    let want: &[(&str, &str, &str, Option<&str>)] = &[
        ("openai", "openai-chat", "https://api.openai.com/v1", Some("OPENAI_API_KEY")),
        ("anthropic", "anthropic", "https://api.anthropic.com", Some("ANTHROPIC_API_KEY")),
        ("gemini", "gemini", "https://generativelanguage.googleapis.com", Some("GEMINI_API_KEY")),
        ("deepseek", "openai-chat", "https://api.deepseek.com", Some("DEEPSEEK_API_KEY")),
        ("xai", "openai-chat", "https://api.x.ai/v1", Some("XAI_API_KEY")),
        ("groq", "openai-chat", "https://api.groq.com/openai/v1", Some("GROQ_API_KEY")),
        ("mistral", "openai-chat", "https://api.mistral.ai/v1", Some("MISTRAL_API_KEY")),
        ("openrouter", "openai-chat", "https://openrouter.ai/api/v1", Some("OPENROUTER_API_KEY")),
        ("together", "openai-chat", "https://api.together.xyz/v1", Some("TOGETHER_API_KEY")),
        ("fireworks", "openai-chat", "https://api.fireworks.ai/inference/v1", Some("FIREWORKS_API_KEY")),
        ("cerebras", "openai-chat", "https://api.cerebras.ai/v1", Some("CEREBRAS_API_KEY")),
        ("moonshot", "openai-chat", "https://api.moonshot.ai/v1", Some("MOONSHOT_API_KEY")),
        ("qwen", "openai-chat", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1", Some("DASHSCOPE_API_KEY")),
        ("zhipu", "openai-chat", "https://api.z.ai/api/paas/v4", Some("ZHIPU_API_KEY")),
        ("minimax", "openai-chat", "https://api.minimax.io/v1", Some("MINIMAX_API_KEY")),
        ("ollama", "openai-chat", "http://localhost:11434/v1", None),
        ("lmstudio", "openai-chat", "http://localhost:1234/v1", None),
        ("vllm", "openai-chat", "http://localhost:8000/v1", None),
        ("llamacpp", "openai-chat", "http://localhost:8080/v1", None),
    ];
    let all = presets::presets();
    for (name, protocol, base, key) in want {
        let c = &all[*name];
        assert_eq!(
            (c.protocol.as_str(), c.base_url.as_str(), c.api_key_env.as_deref()),
            (*protocol, *base, *key),
            "{name}"
        );
    }
    assert_eq!(all.len(), want.len() + 2, "the written ones plus copilot and codex");
    let order: Vec<&str> = want.iter().map(|w| w.0).chain(["copilot", "codex"]).collect();
    assert_eq!(presets::names(), order);
    assert_eq!(all["copilot"].protocol, "copilot");
    assert_eq!(all["copilot"].api_key_env, None);
    assert_eq!(all["codex"].protocol, "openai-responses");
    assert_eq!(all["codex"].api_key_env, None);
    // a name finds a preset, an alias finds the preset it stands for, an unknown id finds nothing
    assert_eq!(presets::find("google").map(|c| c.protocol), Some("gemini".into()));
    assert_eq!(presets::find("zai").map(|c| c.base_url), Some("https://api.z.ai/api/paas/v4".into()));
    assert!(presets::find("no-such-vendor").is_none());
}

#[test]
fn effort_levels_are_what_each_protocol_and_vendor_offers() {
    let lv = |p: &str, k: &str, b: &str, m: &str, r: bool| probe::effort_levels(p, k, b, m, r);
    let none: Vec<String> = Vec::new();
    for (p, k, b, m) in [
        ("anthropic", "anthropic", "", "claude-x"),
        ("gemini", "gemini", "", "gemini-2.5-pro"),
        ("openai-chat", "ollama", "http://localhost:11434/v1", "qwen3"),
        ("openai-chat", "openai-compatible", "https://api.openai.com/v1", "gpt-5"),
    ] {
        assert_eq!(lv(p, k, b, m, false), none, "{p} {m} without reasoning");
    }
    assert_eq!(lv("anthropic", "anthropic", "", "claude-x", true), ["off", "low", "medium", "high"]);
    assert_eq!(lv("anthropic", "x", "https://api.deepseek.com", "claude-x", true), none, "the deepseek host wins");
    assert_eq!(lv("gemini", "gemini", "", "gemini-2.5-pro", true), ["low", "medium", "high"]);
    assert_eq!(lv("gemini", "gemini", "", "Gemini-2.5-PRO", true), ["low", "medium", "high"]);
    assert_eq!(lv("gemini", "gemini", "", "gemini-2.5-flash", true), ["off", "low", "medium", "high"]);
    assert_eq!(lv("openai-chat", "ollama", "http://localhost:11434/v1", "qwen3:8b", true), ["off", "on"]);
    assert_eq!(
        lv("openai-chat", "ollama", "http://localhost:11434/v1", "gpt-oss:20b", true),
        ["low", "medium", "high"]
    );
    assert_eq!(
        lv("openai-chat", "x", "https://api.openai.com/v1", "gpt-5-mini", true),
        ["minimal", "low", "medium", "high"]
    );
    assert_eq!(lv("openai-responses", "x", "", "gpt-5", true), ["minimal", "low", "medium", "high"]);
    assert_eq!(lv("openai-chat", "x", "https://api.openai.com/v1", "o3", true), ["low", "medium", "high"]);
    assert_eq!(lv("openai-chat", "x", "https://api.deepseek.com/v1", "deepseek-reasoner", true), none);
    assert_eq!(lv("openai-chat", "x", "https://api.x.ai/v1", "grok-4", true), ["low", "medium", "high"]);
    assert_eq!(lv("no-such-protocol", "no-such-vendor", "", "m", true), ["low", "medium", "high"]);
}

#[test]
fn off_thinks_less_follows_the_protocol() {
    assert!(probe::off_thinks_less("gemini", "gemini-2.5-flash"));
    assert!(probe::off_thinks_less("gemini", "anything"));
    assert!(probe::off_thinks_less("anthropic", "claude-sonnet-5-5"));
    assert!(probe::off_thinks_less("anthropic", "claude-sonnet-4-5"), "before 5.5 off is no thinking");
    assert!(!probe::off_thinks_less("anthropic", "claude-opus-5-5"));
    assert!(!probe::off_thinks_less("openai-chat", "claude-sonnet-5-5"));
    assert!(!probe::off_thinks_less("openai-responses", "gpt-5"));
    assert!(!probe::off_thinks_less("no-such-protocol", "m"));
    for m in ["claude-sonnet-5-5", "claude-sonnet-4-5", "gpt-5"] {
        let routed = crate::copilot::route(m) == crate::copilot::Route::Messages;
        assert_eq!(probe::off_thinks_less("copilot", m), routed && probe::off_thinks_less("anthropic", m), "{m}");
    }
}

#[tokio::test]
async fn only_anthropic_and_gemini_have_a_list_route_of_their_own() {
    use httpmock::prelude::*;
    use nexo_core::ProviderConfig;
    use serde_json::json;
    // Anthropic lists at `/v1/models`, Gemini at `/v1beta/models`; each answer is a probe of that kind
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(GET).path("/v1/models");
        t.status(200).json_body(json!({"data": [{"id": "claude-x", "display_name": "X"}]}));
    });
    s.mock(|w, t| {
        w.method(GET).path("/v1beta/models");
        t.status(200).json_body(json!({"models": [{"name": "models/gemini-x"}]}));
    });
    let a = probe::server(&ProviderConfig::new("anthropic", &s.base_url(), None), Some("k")).await.unwrap();
    assert_eq!((a.kind.as_str(), a.protocol.as_str()), ("anthropic", "anthropic"));
    let g = probe::server(&ProviderConfig::new("gemini", &s.base_url(), None), Some("k")).await.unwrap();
    assert_eq!((g.kind.as_str(), g.protocol.as_str()), ("gemini", "gemini"));
    // the label a person reads for each kind is the core's, unchanged
    for (k, l) in [("anthropic", "Anthropic"), ("gemini", "Gemini"), ("no-such-vendor", "OpenAI-compatible")] {
        assert_eq!(probe::kind_label(k), l);
    }
}
