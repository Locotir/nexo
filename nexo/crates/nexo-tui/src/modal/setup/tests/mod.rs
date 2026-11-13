use super::*;
use nexo_core::ModelInfo;

mod late_answers_and_held_pastes;

fn press(s: &mut Setup, code: KeyCode) -> SetupOut {
    s.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typing(s: &mut Setup, text: &str) {
    for c in text.chars() {
        press(s, KeyCode::Char(c));
    }
}

fn setup() -> Setup {
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_TEST_NO_SUCH_KEY"));
    ds.default_model = Some("deepseek-chat".into());
    let ol = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
    Setup::new(
        vec![("deepseek".into(), ds), ("ollama".into(), ol)],
        vec![("deepseek".into(), "deepseek-chat".into())],
        "deepseek/deepseek-chat",
    )
}

#[test]
fn a_pasted_key_or_address_loses_escapes_controls_and_line_breaks() {
    let mut s = setup();
    for step in [Step::Key, Step::Address] {
        s.step = step;
        s.field.clear();
        s.paste("  key-abc\x1b[2J\r\ndef\x07\r\n");
        assert_eq!(s.field, "key-abcdef", "{step:?}");
    }
}

#[test]
fn the_scripted_mock_provider_needs_no_key_or_address() {
    let m = ProviderConfig::new("mock", "", None);
    assert!(provider_ready("mock", &m, &[]));
}

/// Every test of the setup reads a login store that is not there, not the person's own.
fn no_auth() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json"));
}

#[test]
fn a_key_kept_with_auth_login_counts_as_found() {
    no_auth();
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("auth.json");
    let mut map = std::collections::BTreeMap::new();
    map.insert("acme-auth".to_string(), nexo_core::auth::Credential::Api { key: "stored".into() });
    nexo_core::auth::write_to(&f, &map).unwrap();
    std::env::set_var("NEXO_AUTH_FILE", &f);
    let acme = ProviderConfig::new("openai-chat", "https://acme.example", Some("NEXO_TEST_NO_SUCH_KEY"));
    let s = Setup::new(vec![("acme-auth".into(), acme)], vec![("acme-auth".into(), "m".into())], "acme-auth/m");
    // a store that is not there, not the person's own, for the tests that run beside this one
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let row = s.list.items.iter().find(|i| i.value == "new:acme-auth").unwrap();
    assert!(row.hint.ends_with("key ✓"), "{}", row.hint);
}

#[test]
fn an_unset_variable_at_the_key_step_is_named_not_probed() {
    let mut s = setup();
    s.step = Step::Key;
    typing(&mut s, "$NEXO_TEST_NO_SUCH_KEY");
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
    assert_eq!(s.error.as_deref(), Some("$NEXO_TEST_NO_SUCH_KEY is not set in this shell"));
    assert_eq!(s.step, Step::Key);
}

#[test]
fn a_configured_model_switches_at_once() {
    let mut s = setup();
    assert_eq!(s.list.current().unwrap().value, "use:deepseek/deepseek-chat");
    let out = press(&mut s, KeyCode::Enter);
    assert_eq!(
        out,
        SetupOut::SendAndClose(AgentCommand::SetModel {
            provider: Some("deepseek".into()),
            model: "deepseek-chat".into()
        })
    );
}

#[test]
fn a_running_local_server_moves_up_and_is_asked_at_once() {
    let mut s = setup();
    s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    let labels: Vec<&str> = s.list.ranked.iter().map(|&i| s.list.items[i].label.as_str()).collect();
    assert_eq!(
        labels,
        ["▾ deepseek", "   deepseek-chat", "   set up again", "▸ ollama", "set up your own server"],
        "{labels:?}"
    );
    assert!(s.list.items[3].hint.contains("● running · Ollama 0.12.6"));
    typing(&mut s, "set up oll");
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, .. }) if name == "ollama"), "{out:?}");
    assert!(s.busy.is_some());
}

#[test]
fn the_whole_setup_of_a_keyed_provider() {
    let mut s = setup();
    typing(&mut s, "set up deep");
    // a named provider goes straight to its key: no address to type
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
    assert_eq!((s.step, s.field.as_str()), (Step::Key, ""));
    typing(&mut s, "sk-secret");
    let rendered: String = s
        .lines(80, &Theme::nexo(), 0, false)
        .0
        .iter()
        .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>())
        .collect();
    assert!(!rendered.contains("sk-secret") && rendered.contains("•••"), "the key is masked");
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == "sk-secret"));
    s.probed(Ok(ServerProbe {
        kind: "openai-compatible".into(),
        protocol: "openai-chat".into(),
        base_url: "https://api.deepseek.com".into(),
        models: vec![
            ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() },
            ModelInfo { id: "deepseek-reasoner".into(), reasoning: true, ..Default::default() },
        ],
        ..Default::default()
    }));
    assert_eq!(s.step, Step::Model);
    assert_eq!(s.list.current().unwrap().value, "deepseek-chat", "the provider's default model comes first");
    press(&mut s, KeyCode::Down);
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::CheckModel { ref model, .. }) if model == "deepseek-reasoner"));
    s.checked(ModelCheck {
        info: ModelInfo {
            id: "deepseek-reasoner".into(),
            context_window: Some(128_000),
            reasoning: true,
            efforts: vec!["low".into(), "high".into()],
            ..Default::default()
        },
        sources: vec!["context 128000 from the models.dev catalogue".into()],
        latency_ms: Some(900),
        called_tool: true,
        reasoned: true,
        ..Default::default()
    });
    assert_eq!(s.step, Step::Review);
    assert_eq!(s.fields[1].value, "128000");
    assert_eq!(s.fields[1].source, "the models.dev catalogue");
    // the facts wait behind Tab; a price typed in, the effort set to high
    press(&mut s, KeyCode::Tab);
    for _ in 0..3 {
        press(&mut s, KeyCode::Down);
    }
    typing(&mut s, "0.28");
    press(&mut s, KeyCode::Down);
    typing(&mut s, "0.42");
    press(&mut s, KeyCode::Down);
    press(&mut s, KeyCode::Down);
    press(&mut s, KeyCode::Left);
    assert_eq!(s.effort, Some(1));
    let out = press(&mut s, KeyCode::Enter);
    let SetupOut::Send(AgentCommand::SaveModel { name, cfg, model, key }) = out else { panic!("{out:?}") };
    assert_eq!((name.as_str(), model.as_str(), key.as_deref()), ("deepseek", "deepseek-reasoner", Some("sk-secret")));
    assert_eq!(cfg.context_window["deepseek-reasoner"], 128_000);
    assert_eq!(cfg.pricing["deepseek-reasoner"].output, 0.42);
    assert_eq!(cfg.effort["deepseek-reasoner"], "high");
    // the last screen waits for the save to be confirmed
    assert_eq!(s.step, Step::Review);
    s.saved("deepseek", "deepseek-reasoner");
    assert_eq!(s.step, Step::Done);
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Close);
}

#[test]
fn a_dead_address_stays_with_its_reason_and_esc_walks_back() {
    let mut s = setup();
    typing(&mut s, "your own");
    press(&mut s, KeyCode::Enter);
    assert_eq!((s.step, s.name.as_str()), (Step::Address, "custom"));
    typing(&mut s, "localhost:9");
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.cfg.base_url, "http://localhost:9");
    s.probed(Err("nothing answers at http://localhost:9: connection refused".into()));
    // the reason is a card, with a way on: Enter edits the address, kept as typed
    assert_eq!(s.step, Step::Trouble);
    assert!(s.problem.as_ref().unwrap().title.contains("localhost"), "{:?}", s.problem);
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
    assert_eq!((s.step, s.field.as_str()), (Step::Address, "localhost:9"));
    s.probed(Err("x".into()));
    assert_eq!(s.step, Step::Address, "nothing is waiting: the answer is ignored");
    press(&mut s, KeyCode::Enter);
    s.probed(Err("nothing answers at http://localhost:9: connection refused".into()));
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert_eq!(s.step, Step::Provider);
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
}

#[test]
fn a_model_the_list_lacks_can_be_typed() {
    let mut s = setup();
    s.step = Step::Model;
    s.probe = Some(ServerProbe { kind: "llama.cpp".into(), ..Default::default() });
    s.list = Picker::new(PickKind::Model, "", Vec::new(), None);
    typing(&mut s, "my-model");
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::CheckModel { ref model, .. }) if model == "my-model"));
}

fn subscription() -> Setup {
    let mut cx = ProviderConfig::new("openai-responses", "https://chatgpt.example/backend-api/codex", None);
    cx.login = Some("codex".into());
    cx.models = vec!["gpt-x".into()];
    Setup::new(vec![("codex".into(), cx)], Vec::new(), "")
}

#[test]
fn a_subscription_provider_asks_how_to_sign_in_before_its_address() {
    let mut s = subscription();
    // the first choices offer the sign-in; the row is on the cursor
    assert_eq!((s.step, s.list.current().unwrap().value.as_str()), (Step::Start, "new:codex"));
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
    assert_eq!(s.step, Step::Login);
    assert_eq!(s.cfg.auth.as_deref(), Some("codex"), "the login is kept under the provider's name");
    let values: Vec<&str> = s.list.items.iter().map(|i| i.value.as_str()).collect();
    assert!(values.contains(&"codex-browser") && values.contains(&"codex-device"), "{values:?}");

    // the browser way: the command goes out, the page shown, Esc drops it
    typing(&mut s, "browser");
    let out = press(&mut s, KeyCode::Enter);
    assert_eq!(out, SetupOut::Send(AgentCommand::Login { name: "codex".into(), flow: "codex-browser".into() }));
    assert!(s.busy.is_some());
    s.login_prompt("https://auth.example/oauth/authorize?state=abc".into(), String::new());
    let (lines, _) = s.lines(80, &Theme::nexo(), 0, false);
    let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string())).collect();
    assert!(text.contains("auth.example") && !text.contains("oauth/authorize"), "{text}");
    assert_eq!(
        press(&mut s, KeyCode::Char('c')),
        SetupOut::Copy("https://auth.example/oauth/authorize?state=abc".into())
    );
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelLogin));
    assert!(s.busy.is_none() && s.prompt.is_none() && s.step == Step::Login);

    // a refused sign-in stays on the list with its reason
    s.list.query.clear();
    s.list.refilter();
    typing(&mut s, "browser");
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.logged_in(Err("sign-in was declined".into())), SetupOut::Nothing);
    assert_eq!((s.step, s.error.as_deref()), (Step::Login, Some("sign-in was declined")));

    // a good one asks the provider what it serves, and Esc from the models goes back to the sign-in
    s.list.query.clear();
    s.list.refilter();
    typing(&mut s, "browser");
    press(&mut s, KeyCode::Enter);
    let out = s.logged_in(Ok(()));
    assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, .. }) if name == "codex"), "{out:?}");
    s.probed(Ok(ServerProbe {
        kind: "subscription".into(),
        protocol: "openai-responses".into(),
        base_url: "https://chatgpt.example/backend-api/codex".into(),
        models: vec![ModelInfo { id: "gpt-x".into(), ..Default::default() }],
        ..Default::default()
    }));
    assert_eq!(s.step, Step::Model);
    press(&mut s, KeyCode::Esc);
    assert_eq!(s.step, Step::Login);
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert_eq!(s.step, Step::Start);
}

#[test]
fn a_login_that_arrives_after_the_setup_moved_on_is_ignored() {
    let mut s = subscription();
    assert_eq!(s.logged_in(Ok(())), SetupOut::Nothing);
    s.login_prompt("https://x".into(), String::new());
    assert!(s.prompt.is_none());
    assert_eq!(s.step, Step::Start);
}

// ---- the first run: a few choices, a pasted key tested before it is kept ----

/// A first run: nothing set up, a keyed provider for each of two services, a subscription, a local server.
fn first_run() -> Setup {
    no_auth();
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_FIRST_DS"));
    ds.default_model = Some("deepseek-chat".into());
    let an = ProviderConfig::new("anthropic", "https://api.anthropic.com", Some("NEXO_T_FIRST_AN"));
    let ol = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
    let mut cx = ProviderConfig::new("openai-responses", "https://chatgpt.example/backend-api/codex", None);
    cx.login = Some("codex".into());
    Setup::new(
        vec![("deepseek".into(), ds), ("anthropic".into(), an), ("codex".into(), cx), ("ollama".into(), ol)],
        Vec::new(),
        "",
    )
}

fn row_text(s: &Setup, w: usize) -> String {
    s.lines(w, &Theme::nexo(), 0, false)
        .0
        .iter()
        .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>() + "\n")
        .collect()
}

fn deepseek_probe() -> ServerProbe {
    ServerProbe {
        kind: "openai-compatible".into(),
        protocol: "openai-chat".into(),
        base_url: "https://api.deepseek.com".into(),
        models: vec![ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() }],
        ..Default::default()
    }
}

#[test]
fn the_first_screen_has_a_few_choices_and_one_is_recommended() {
    let s = first_run();
    assert_eq!(s.step, Step::Start);
    let labels: Vec<&str> = s.list.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Use deepseek", "Sign in with your ChatGPT plan", "More providers"], "{labels:?}");
    assert_eq!(s.list.items.iter().filter(|i| i.hint.contains("recommended")).count(), 1);
    assert!(s.list.items[0].hint.starts_with("recommended"), "{}", s.list.items[0].hint);
    // nothing a newcomer has not met
    let t = row_text(&s, 80);
    for word in ["endpoint", "token", "slug", "base URL"] {
        assert!(!t.contains(word), "{word}: {t}");
    }
    assert!(t.lines().filter(|l| !l.trim().is_empty()).count() <= 5, "one sentence, the scan note, three choices: {t}");
}

#[test]
fn models_on_this_computer_come_first_and_ask_for_no_key() {
    let mut s = first_run();
    s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    assert_eq!(s.step, Step::Start);
    let first = s.list.current().unwrap();
    assert_eq!(
        (first.label.as_str(), first.value.as_str()),
        ("Use the models on this computer (free)", "local:ollama")
    );
    assert!(first.hint.starts_with("recommended"), "{}", first.hint);
    assert!(s.list.items.len() <= 4);
    let out = press(&mut s, KeyCode::Enter);
    assert!(
        matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: None, .. }) if name == "ollama"),
        "{out:?}"
    );
    assert_ne!(s.step, Step::Key);
}

#[test]
fn a_key_found_in_the_environment_is_offered_and_used_as_it_is() {
    no_auth();
    // its own variable, so no other test sees it
    std::env::set_var("NEXO_T_ENV_OFFER_AN", "made-up-key-for-the-test-0000");
    let an = ProviderConfig::new("anthropic", "https://api.anthropic.com", Some("NEXO_T_ENV_OFFER_AN"));
    let ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_ENV_OFFER_DS"));
    let mut s = Setup::new(vec![("deepseek".into(), ds), ("anthropic".into(), an)], Vec::new(), "");
    let first = s.list.current().unwrap();
    assert_eq!(first.label, "Found a key for anthropic, use it?");
    assert_eq!(first.value, "env:anthropic");
    assert!(s.list.items.len() <= 4);
    // the offered row is the one that is recommended; deepseek is still one row down
    assert!(first.hint.starts_with("recommended"), "{}", first.hint);
    assert!(s.list.items.iter().any(|i| i.label == "Use deepseek" && !i.hint.contains("recommended")));
    let out = press(&mut s, KeyCode::Enter);
    std::env::remove_var("NEXO_T_ENV_OFFER_AN");
    assert!(
        matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: None, .. }) if name == "anthropic"),
        "the key is read from where it is, not copied: {out:?}"
    );
    assert_eq!(s.name, "anthropic");
}

#[test]
fn a_pasted_key_names_its_service_when_only_that_service_starts_so() {
    let mut s = first_run();
    // quotes, spaces and a line break come with a paste; none of them is the key
    let out = s.paste("  \"sk-ant-api03-madeupmadeupmade\"  \r\n");
    assert_eq!(s.name, "anthropic");
    assert!(
        matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == "sk-ant-api03-madeupmadeupmade"),
        "{out:?}"
    );
    assert_eq!(s.busy.as_deref(), Some("testing the key"));
}

#[test]
fn a_pasted_key_with_a_start_two_services_share_asks_which_one() {
    let mut s = first_run();
    let out = s.paste("sk-made-up-0123-made-up-4567-abcd");
    assert_eq!(out, SetupOut::Nothing);
    assert_eq!(s.step, Step::Provider, "the person is asked");
    assert!(row_text(&s, 80).contains("deepseek"));
    typing(&mut s, "set up deep");
    let out = press(&mut s, KeyCode::Enter);
    assert!(
        matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: Some(ref k), .. }) if name == "deepseek" && k == "sk-made-up-0123-made-up-4567-abcd"),
        "the pasted key goes with the choice: {out:?}"
    );
}

#[test]
fn a_key_on_the_key_screen_shows_dots_and_its_last_four_and_the_page_to_get_one() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    assert_eq!((s.step, s.name.as_str()), (Step::Key, "deepseek"));
    s.paste(" 'abcdefgh12345678' ");
    assert_eq!(s.field, "abcdefgh12345678");
    let t = row_text(&s, 80);
    assert!(
        t.contains(
            "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}5678"
        ),
        "{t}"
    );
    assert!(!t.contains("abcdefgh"), "{t}");
    assert!(t.contains("https://platform.deepseek.com/api_keys"), "{t}");
    for word in ["endpoint", "token", "slug", "base URL"] {
        assert!(!t.contains(word), "{word}: {t}");
    }
    let _ = crate::links::take_opened();
    press_ctrl(&mut s, 'o');
    assert_eq!(crate::links::take_opened(), ["https://platform.deepseek.com/api_keys"]);
    // a provider whose key page is not known shows no link
    s.name = "ollama".into();
    assert!(!row_text(&s, 80).contains("Get one at"));
}

fn press_ctrl(s: &mut Setup, c: char) -> SetupOut {
    s.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

#[test]
fn a_refused_key_is_a_card_with_a_way_back_and_nothing_is_kept() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-wrong-key-0000");
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(_), .. })), "{out:?}");
    s.probed(Ok(ServerProbe { needs_key: true, ..Default::default() }));
    assert_eq!(s.step, Step::Trouble);
    let t = row_text(&s, 80);
    assert!(t.contains("401") || t.to_lowercase().contains("key"), "{t}");
    assert!(t.contains("paste the key again"), "{t}");
    assert!(t.lines().filter(|l| !l.trim().is_empty()).count() <= 4, "a card of 3 lines and a hint: {t}");
    assert!(t.lines().all(|l| l.chars().count() < 72), "{t}");
    assert!(s.key.is_some(), "the wrong key is in memory only, never written");
    // Enter: the key is asked again, empty
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
    assert_eq!((s.step, s.field.as_str(), s.key.is_none()), (Step::Key, "", true));
    // Esc: back to the first choices, with the key forgotten
    s.probed(Ok(ServerProbe { needs_key: true, ..Default::default() }));
    typing(&mut s, "x");
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert_eq!(s.step, Step::Start);
    assert!(s.pending_key.is_none() && s.field.is_empty());
}

#[test]
fn a_server_that_does_not_answer_is_a_card_with_retry() {
    let mut s = first_run();
    s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    press(&mut s, KeyCode::Enter);
    s.probed(Err("nothing answers at http://localhost:11434/v1: connection refused".into()));
    assert_eq!(s.step, Step::Trouble);
    let out = press(&mut s, KeyCode::Enter);
    assert!(
        matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, .. }) if name == "ollama"),
        "retry asks again: {out:?}"
    );
    s.probed(Err("nothing answers".into()));
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert_eq!(s.step, Step::Start, "or another way in");
}

#[test]
fn a_local_server_with_no_models_says_what_to_do_instead_of_an_empty_list() {
    let mut s = first_run();
    s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(ServerProbe {
        kind: "ollama".into(),
        protocol: "openai-chat".into(),
        base_url: "http://localhost:11434/v1".into(),
        ..Default::default()
    }));
    assert_eq!(s.step, Step::Model);
    let t = row_text(&s, 80);
    assert!(t.contains("ollama pull") && !t.contains("no match"), "{t}");
}

#[test]
fn a_default_model_is_chosen_with_a_line_on_what_it_is_for() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    assert_eq!(s.step, Step::Model);
    assert_eq!(s.list.current().unwrap().value, "deepseek-chat");
    assert!(row_text(&s, 80).contains("suggests for everyday work"), "{}", row_text(&s, 80));
}

#[test]
fn the_key_is_tested_with_a_model_then_saved_then_the_last_screen_says_where() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::CheckModel { .. })), "{out:?}");
    assert_eq!(s.busy.as_deref(), Some("testing deepseek-chat"));
    s.checked(ModelCheck {
        info: ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() },
        latency_ms: Some(640),
        ..Default::default()
    });
    assert_eq!(s.step, Step::Review);
    let t = row_text(&s, 80);
    assert!(t.contains("Works") && t.contains("deepseek-chat"), "{t}");
    // saving is asked of the agent; the last screen waits for its word
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::SaveModel { .. })), "{out:?}");
    assert_eq!(s.step, Step::Review);
    s.saved("other", "model");
    assert_eq!(s.step, Step::Review, "a model changed that is not this one is not our save");
    s.saved("deepseek", "deepseek-chat");
    assert_eq!(s.step, Step::Done);
    let t = row_text(&s, 80);
    assert!(t.contains("deepseek/deepseek-chat is ready"), "{t}");
    assert!(t.contains("Saved in") && t.contains("The key is in"), "{t}");
    assert!(t.contains("/model") && t.contains("/help") && t.contains("Enter"), "three things to try: {t}");
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
}

#[test]
fn a_failed_check_is_a_card_and_esc_returns_to_the_models() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck { error: Some("HTTP 402: insufficient balance".into()), ..Default::default() });
    assert_eq!(s.step, Step::Trouble);
    assert!(s.problem.is_some());
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert_eq!(s.step, Step::Model);
}
