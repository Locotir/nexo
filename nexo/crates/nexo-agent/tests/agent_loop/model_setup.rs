use super::*;

#[tokio::test]
async fn the_model_setup_checks_saves_switches_and_sets_the_effort() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        // the first try asks for a reasoning level; this server does not take one
        Reply::Err(ProviderError::new(ErrorKind::BadRequest, "Unsupported parameter: 'reasoning_effort'")),
        Reply::Events(call("c1", "ping", json!({}))),
        Reply::Events(text("on the new model")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    let probe = nexo_core::ServerProbe {
        kind: "openai-compatible".into(),
        protocol: "openai-chat".into(),
        base_url: "https://gw.example/v1".into(),
        models: vec![nexo_core::ModelInfo {
            id: "think-1".into(),
            context_window: Some(64_000),
            reasoning: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let cfg = ProviderConfig::new("openai-chat", "https://gw.example/v1", Some("GW_KEY"));
    r.send(AgentCommand::CheckModel {
        name: "gw".into(),
        probe,
        cfg: cfg.clone(),
        model: "think-1".into(),
        key: None,
        seq: 0,
    });
    let evs = r.until(|e| matches!(e, AgentEvent::ModelChecked { .. })).await;
    let AgentEvent::ModelChecked { check, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(check.error.is_none(), "{check:?}");
    assert!(check.called_tool, "the model called the test tool");
    assert!(check.info.efforts.is_empty(), "the refused level leaves no levels: {check:?}");
    assert!(check.sources.iter().any(|s| s.contains("refused the reasoning level")), "{:?}", check.sources);
    let reqs = client.requests();
    assert!(reqs[0].reasoning.is_some() && reqs[1].reasoning.is_none());
    assert_eq!(reqs[1].tools[0].name, "ping");

    // save it with what the setup found, plus levels to choose from
    let mut save = cfg;
    save.context_window.insert("think-1".into(), 64_000);
    save.efforts.insert("think-1".into(), vec!["low".into(), "high".into()]);
    r.send(AgentCommand::SaveModel { name: "gw".into(), cfg: save, model: "think-1".into(), key: None });
    let evs = r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelChanged { provider, model } if provider == "gw" && model == "think-1")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ModelLimits { context_window: Some(64_000), .. })));
    let saved = user_config();
    assert_eq!(saved.provider.as_deref(), Some("gw"));
    assert_eq!(saved.providers["gw"].api_key_env.as_deref(), Some("GW_KEY"));

    r.send(AgentCommand::SetEffort { effort: Some("medium".into()) });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("low, high"))));
    r.send(AgentCommand::SetEffort { effort: Some("high".into()) });
    let evs = r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Effort { current: Some(c), .. }) if c == "high"));
    assert_eq!(user_config().providers["gw"].effort["think-1"], "high");
    r.send(AgentCommand::Submit { text: "x".into(), attachments: vec![] });
    r.turn().await;
    let last = client.requests().pop().unwrap();
    assert_eq!(last.model, "think-1");
    assert!(matches!(last.reasoning, Some(nexo_core::ReasoningConfig::Effort(nexo_core::Effort::High))));
    r.stop().await;
}

#[tokio::test]
async fn a_save_that_cannot_write_says_so_keeps_no_key_and_never_switches() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    let data = data_dir();
    // the configuration file cannot be written: a folder stands where it should be
    let conf = data.join("config.toml");
    let before = std::fs::read(&conf).ok();
    let _ = std::fs::remove_file(&conf);
    std::fs::create_dir(&conf).unwrap();
    let secret = "sk-never-left-on-disk-0123456789";
    let cfg = ProviderConfig::new("openai-chat", "https://gw.example/v1", Some("GW_KEY"));
    r.send(AgentCommand::SaveModel { name: "gw-fail".into(), cfg, model: "m".into(), key: Some(secret.into()) });
    let evs = r.until(|e| matches!(e, AgentEvent::SaveFailed { .. } | AgentEvent::ModelChanged { .. })).await;
    std::fs::remove_dir(&conf).unwrap();
    if let Some(b) = before {
        std::fs::write(&conf, b).unwrap();
    }
    assert!(
        matches!(evs.last(), Some(AgentEvent::SaveFailed { name, model, reason }) if name == "gw-fail" && model == "m" && reason.starts_with("Not saved")),
        "{evs:?}"
    );
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { .. })));
    fn holds(dir: &std::path::Path, needle: &str) -> bool {
        std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
            let p = e.path();
            if p.is_dir() {
                holds(&p, needle)
            } else {
                std::fs::read_to_string(&p).is_ok_and(|t| t.contains(needle))
            }
        })
    }
    assert!(!holds(&data, secret), "a key of a save that failed is not left on disk");
    r.stop().await;
}

#[tokio::test]
async fn fast_switches_to_the_twin_and_back_or_sets_the_request_option() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("a")), Reply::Events(text("b"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.providers.get_mut("mock").unwrap().models = vec!["m1".into(), "m1-Fast".into(), "m2".into()];
    let mut sub = ProviderConfig::new("mock", "http://sub", None);
    sub.models = vec!["c1".into()];
    sub.login = Some("codex".into());
    cfg.providers.insert("sub".into(), sub);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;

    // m1 -> m1-Fast (the provider's spelling) and back
    r.send(AgentCommand::SetFast { on: None });
    let evs = r.until(|e| matches!(e, AgentEvent::Fast { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { model, .. } if model == "m1-Fast")));
    assert!(matches!(evs.last(), Some(AgentEvent::Fast { on: true })));
    r.send(AgentCommand::SetFast { on: Some(true) });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("already a fast version")));
    r.send(AgentCommand::SetFast { on: Some(false) });
    let evs = r.until(|e| matches!(e, AgentEvent::Fast { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { model, .. } if model == "m1")));
    assert!(matches!(evs.last(), Some(AgentEvent::Fast { on: false })));

    // no twin: one line, nothing changes
    r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    r.send(AgentCommand::SetFast { on: None });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text == "No fast version of m2 on mock."));
    r.send(AgentCommand::SetFast { on: Some(false) });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text == "fast is already off"));

    // a provider with a request option keeps its model and sends the option while it is on
    r.send(AgentCommand::SetModel { provider: Some("sub".into()), model: "c1".into() });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    r.send(AgentCommand::SetFast { on: None });
    assert!(matches!(r.next().await, AgentEvent::Fast { on: true }));
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::SetFast { on: None });
    assert!(matches!(r.next().await, AgentEvent::Fast { on: false }));
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs[0].model, "c1");
    assert_eq!(reqs[0].extra.get("service_tier"), Some(&json!("priority")));
    assert!(!reqs[1].extra.contains_key("service_tier"));
    r.stop().await;
}

fn trial_files(data: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(data.join("keys"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".trying-")))
        .collect()
}

#[tokio::test]
async fn stopping_a_model_check_removes_the_tried_key_and_applies_no_answer() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    // the server takes the request and never answers
    let client = Scripted::new(vec![Reply::Hang(vec![])]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    let data = data_dir();
    let probe = nexo_core::ServerProbe {
        kind: "openai-compatible".into(),
        protocol: "openai-chat".into(),
        base_url: "https://gw.example/v1".into(),
        ..Default::default()
    };
    let cfg = ProviderConfig::new("openai-chat", "https://gw.example/v1", None);
    r.send(AgentCommand::CheckModel {
        name: "gw-hold".into(),
        probe,
        cfg,
        model: "m".into(),
        key: Some("sk-held-0123456789".into()),
        seq: 7,
    });
    let mut seen = false;
    for _ in 0..100 {
        if trial_files(&data).iter().any(|p| p.to_string_lossy().contains("gw-hold")) {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "the check keeps its trial file while it waits for the server");
    r.send(AgentCommand::CancelCheck);
    let mut gone = false;
    for _ in 0..100 {
        if !trial_files(&data).iter().any(|p| p.to_string_lossy().contains("gw-hold")) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(gone, "stopping the check removes the file");
    // and nothing is answered
    let late = tokio::time::timeout(Duration::from_millis(400), async {
        loop {
            if matches!(r.next().await, AgentEvent::ModelChecked { .. }) {
                return true;
            }
        }
    })
    .await;
    assert!(late.is_err(), "a stopped check applies no answer");
    r.stop().await;
}

#[tokio::test]
async fn a_failed_save_leaves_the_old_key_and_the_old_configuration_as_they_were() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    let data = data_dir();
    let old_key = data.join("keys").join("gw-keep");
    std::fs::create_dir_all(old_key.parent().unwrap()).unwrap();
    std::fs::write(&old_key, "sk-old-key-0123456789").unwrap();
    let conf = data.join("config.toml");
    let before = std::fs::read(&conf).ok();
    let _ = std::fs::remove_file(&conf);
    std::fs::create_dir(&conf).unwrap();
    let cfg = ProviderConfig::new("openai-chat", "https://gw.example/v1", None);
    r.send(AgentCommand::SaveModel {
        name: "gw-keep".into(),
        cfg,
        model: "m".into(),
        key: Some("sk-new-key-9876543210".into()),
    });
    let evs = r.until(|e| matches!(e, AgentEvent::SaveFailed { .. } | AgentEvent::ModelChanged { .. })).await;
    std::fs::remove_dir(&conf).unwrap();
    if let Some(b) = before {
        std::fs::write(&conf, b).unwrap();
    }
    assert!(matches!(evs.last(), Some(AgentEvent::SaveFailed { .. })), "{evs:?}");
    assert_eq!(std::fs::read_to_string(&old_key).unwrap(), "sk-old-key-0123456789", "the old key stays");
    assert!(trial_files(&data).is_empty(), "the new key's file is gone: {:?}", trial_files(&data));
    r.stop().await;
}

#[tokio::test]
async fn a_good_save_replaces_the_old_key_with_the_new_one() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    let data = data_dir();
    let old_key = data.join("keys").join("gw-swap");
    std::fs::create_dir_all(old_key.parent().unwrap()).unwrap();
    std::fs::write(&old_key, "sk-old-key-0123456789").unwrap();
    let cfg = ProviderConfig::new("openai-chat", "https://gw.example/v1", None);
    r.send(AgentCommand::SaveModel {
        name: "gw-swap".into(),
        cfg,
        model: "m".into(),
        key: Some("sk-new-key-9876543210".into()),
    });
    let evs = r.until(|e| matches!(e, AgentEvent::SaveFailed { .. } | AgentEvent::ModelChanged { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::ModelChanged { .. })), "{evs:?}");
    assert_eq!(std::fs::read_to_string(&old_key).unwrap(), "sk-new-key-9876543210");
    assert!(trial_files(&data).is_empty());
    r.stop().await;
}
