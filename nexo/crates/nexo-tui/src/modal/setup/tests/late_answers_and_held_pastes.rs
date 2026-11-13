use super::*;

#[test]
fn esc_goes_back_one_step_at_every_step_and_closes_from_the_first() {
    let mut s = first_run();
    // Start -> More providers -> Key -> (typed) back -> Provider -> Start -> closed
    press(&mut s, KeyCode::Down);
    press(&mut s, KeyCode::Down);
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.step, Step::Provider);
    typing(&mut s, "set up deep");
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.step, Step::Key);
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert_eq!(s.step, Step::Provider);
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert_eq!(s.step, Step::Start);
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
    // Key -> Model -> Review: each Esc is one step
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck {
        info: ModelInfo { id: "deepseek-chat".into(), ..Default::default() },
        ..Default::default()
    });
    assert_eq!(s.step, Step::Review);
    press(&mut s, KeyCode::Esc);
    assert_eq!(s.step, Step::Model);
    press(&mut s, KeyCode::Esc);
    assert_eq!(s.step, Step::Key, "the key typed is asked for again");
    press(&mut s, KeyCode::Esc);
    assert_eq!(s.step, Step::Start);
    // while a request is out, Esc cancels it and stays
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    assert!(s.busy.is_some());
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelCheck));
    assert!(s.busy.is_none() && s.step == Step::Key);
}

#[test]
fn the_list_of_providers_says_what_to_do_not_where_they_live() {
    let mut s = first_run();
    s.show_providers();
    let hint =
        |v: &str| s.list.items.iter().find(|i| i.value == v).map(|i| i.hint.clone()).unwrap_or_else(|| panic!("{v}"));
    assert_eq!(hint("new:deepseek"), "needs a key");
    assert_eq!(hint("new:codex"), "sign in");
    assert_eq!(hint("new:ollama"), "on this computer");
    assert!(s.list.items.iter().all(|i| !i.hint.contains("://")), "no address in a row");
}

#[test]
fn the_key_line_of_every_step_says_what_esc_does() {
    let mut s = first_run();
    for step in [
        Step::Start,
        Step::Provider,
        Step::Login,
        Step::Model,
        Step::Address,
        Step::Key,
        Step::Review,
        Step::Trouble,
        Step::Done,
    ] {
        s.step = step;
        s.from_start = false;
        let h = s.hints();
        assert!(h.contains("Esc") || step == Step::Done, "{step:?}: {h}");
        if !matches!(step, Step::Start | Step::Provider | Step::Done) {
            assert!(h.contains("Esc back") || h.contains("Esc pick another"), "{step:?}: {h}");
        }
    }
    s.step = Step::Provider;
    s.from_start = true;
    assert!(s.hints().contains("Esc back to the choices"));
    s.busy = Some("testing the key".into());
    assert_eq!(s.hints(), " Esc cancel");
}

#[test]
fn esc_from_a_provider_screen_lands_on_the_row_it_came_from() {
    let mut s = first_run();
    s.fill_start();
    press(&mut s, KeyCode::Down);
    let want = s.list.current().map(|i| i.value.clone()).unwrap_or_default();
    press(&mut s, KeyCode::Enter);
    press(&mut s, KeyCode::Esc);
    assert_eq!(s.step, Step::Start);
    assert_eq!(s.list.current().map(|i| i.value.clone()).unwrap_or_default(), want);
    // and from the whole list of providers it lands on "More providers"
    press(&mut s, KeyCode::Down);
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.step, Step::Provider);
    press(&mut s, KeyCode::Esc);
    assert_eq!(s.step, Step::Start);
    assert_eq!(s.list.current().map(|i| i.value.as_str()), Some("more"));
}

// ---- the key leaves no residue, and the screens claim only what happened ----

const HELD: &str = "sk-made-up-0123-made-up-4567-abcd";

/// A paste of unknown kind, the person on the list of providers.
fn holding() -> Setup {
    let mut s = first_run();
    assert_eq!(s.paste(HELD), SetupOut::Nothing);
    assert_eq!(s.step, Step::Provider);
    s
}

#[test]
fn an_answer_of_a_check_that_was_left_changes_nothing() {
    let mut s = first_run();
    let SetupOut::Send(AgentCommand::Probe { name: a, seq: seq_a, .. }) = s.paste("sk-ant-api03-madeupmadeupmade")
    else {
        panic!("a probe")
    };
    assert_eq!(a, "anthropic");
    // Esc stops the check; the agent is told
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelCheck));
    assert!(s.busy.is_none());
    // a check on another provider
    assert_eq!(s.paste(HELD), SetupOut::Nothing);
    typing(&mut s, "set up deep");
    let SetupOut::Send(AgentCommand::Probe { name: b, seq: seq_b, .. }) = press(&mut s, KeyCode::Enter) else {
        panic!("a probe")
    };
    assert_eq!(b, "deepseek");
    assert_ne!(seq_a, seq_b);
    // the first one answers late: wrong name, then the right name with the old counter
    s.probed_from("anthropic", seq_a, Ok(deepseek_probe()));
    s.probed_from("deepseek", seq_a, Ok(deepseek_probe()));
    assert!(s.busy.is_some() && s.probe.is_none(), "the screen is still the second check's");
    s.probed_from("deepseek", seq_b, Ok(deepseek_probe()));
    assert!(s.busy.is_none() && s.probe.is_some());
}

#[test]
fn a_model_check_answer_of_another_check_is_ignored() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    let SetupOut::Send(AgentCommand::CheckModel { seq, .. }) = press(&mut s, KeyCode::Enter) else { panic!("a check") };
    let ok = ModelCheck { info: ModelInfo { id: "deepseek-chat".into(), ..Default::default() }, ..Default::default() };
    s.checked_from("deepseek", "deepseek-chat", seq + 1, ok.clone());
    s.checked_from("deepseek", "other", seq, ok.clone());
    assert!(s.busy.is_some(), "not ours");
    s.checked_from("deepseek", "deepseek-chat", seq, ok);
    assert!(s.busy.is_none());
}

#[test]
fn a_save_that_failed_keeps_the_step_says_why_and_never_reaches_the_end_screen() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck {
        info: ModelInfo { id: "deepseek-chat".into(), ..Default::default() },
        ..Default::default()
    });
    assert!(matches!(press(&mut s, KeyCode::Enter), SetupOut::Send(AgentCommand::SaveModel { .. })));
    s.save_failed(
        "deepseek",
        "deepseek-chat",
        "Not saved: config.toml could not be written. Make it writable, then press Enter.".into(),
    );
    assert_eq!(s.step, Step::Review);
    assert!(s.busy.is_none() && s.done.is_none());
    let t = row_text(&s, 100);
    assert!(t.contains("Not saved") && !t.contains("is ready"), "{t}");
    // a failure that is not ours changes nothing; a later success of the same save still works
    assert!(!s.saved("deepseek", "deepseek-chat"), "nothing was waiting");
    assert_eq!(s.step, Step::Review);
}

#[test]
fn the_key_works_only_when_a_request_that_needs_the_key_passed() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    let t = row_text(&s, 100);
    assert!(!t.contains("The key works") && t.contains("Models found"), "a public list proves nothing: {t}");
    let mut s2 = first_run();
    press(&mut s2, KeyCode::Enter);
    typing(&mut s2, "sk-good-key-0000");
    press(&mut s2, KeyCode::Enter);
    s2.probed(Ok(ServerProbe { key_proved: true, ..deepseek_probe() }));
    assert!(row_text(&s2, 100).contains("The key works"));
}

#[test]
fn a_held_paste_is_dropped_with_a_line_when_a_local_server_is_picked() {
    let mut s = holding();
    typing(&mut s, "set up oll");
    s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    let out = press(&mut s, KeyCode::Enter);
    assert!(
        matches!(out, SetupOut::Send(AgentCommand::Probe { ref name, key: None, .. }) if name == "ollama"),
        "{out:?}"
    );
    assert!(s.key.is_none() && s.pending_key.is_none());
    assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
}

#[test]
fn a_held_paste_is_dropped_with_a_line_when_the_custom_server_is_picked_and_when_esc_closes() {
    let mut s = holding();
    typing(&mut s, "your own");
    press(&mut s, KeyCode::Enter);
    assert_eq!((s.step, s.name.as_str()), (Step::Address, "custom"));
    assert!(s.key.is_none() && s.pending_key.is_none());
    assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
    // Esc on the list with a held paste drops it first
    let mut s = holding();
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert!(s.pending_key.is_none());
    assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
}

#[test]
fn a_held_paste_still_goes_to_the_cloud_provider_the_person_picks() {
    let mut s = holding();
    typing(&mut s, "set up deep");
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == HELD), "{out:?}");
    assert!(s.error.is_none());
}

/// A server that records the head of every request it gets.
fn recording_server() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", l.local_addr().unwrap().port());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { break };
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap_or(0);
            log.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (addr, seen)
}

#[test]
fn a_held_paste_never_reaches_a_server_at_an_address_the_person_typed() {
    let (addr, seen) = recording_server();
    let mut s = holding();
    typing(&mut s, "your own");
    press(&mut s, KeyCode::Enter);
    s.field.clear();
    typing(&mut s, &addr);
    let SetupOut::Send(AgentCommand::Probe { cfg, key, .. }) = press(&mut s, KeyCode::Enter) else { panic!("a probe") };
    assert!(key.is_none(), "the command carries no key");
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let _ = rt.block_on(nexo_providers::probe::server(&cfg, key.as_deref()));
    let heads = seen.lock().unwrap().clone();
    assert!(!heads.is_empty(), "the server was asked");
    for h in heads {
        let h = h.to_ascii_lowercase();
        assert!(!h.contains("authorization") && !h.contains("x-api-key") && !h.contains(&HELD[..12]), "{h}");
    }
}

#[test]
fn a_key_typed_in_the_search_box_is_masked_and_taken_as_a_key() {
    let mut s = first_run();
    s.paste("a");
    s.step = Step::Provider;
    s.fill_providers();
    let typed = "sk-ant-api03-madeupmadeupmade";
    typing(&mut s, typed);
    assert_eq!(s.list.query, typed, "all of it is kept until Enter");
    assert!(!row_text(&s, 100).contains(&typed[..14]), "never drawn in clear");
    let out = press(&mut s, KeyCode::Enter);
    assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(ref k), .. }) if k == typed), "{out:?}");
    assert!(s.list.query.is_empty());
}

#[test]
fn text_typed_at_the_first_step_is_held_as_a_key_only_with_a_known_start() {
    // a model id, a password, a word: long enough to pass for a key by length alone
    for typed in ["deepseek-reasoner", "correct-horse-battery-staple", "0123456789abcdef0123456789abcdef"] {
        let mut s = first_run();
        assert_eq!(s.step, Step::Start);
        typing(&mut s, typed);
        let out = press(&mut s, KeyCode::Enter);
        assert!(matches!(out, SetupOut::Nothing), "{typed}: nothing is sent");
        assert_eq!((s.step, s.pending_key.as_deref()), (Step::Start, None), "{typed}: not held");
        assert!(s.field.is_empty(), "{typed}: dropped");
        assert!(s.error.as_deref().is_some_and(|e| e.contains("not a key nexo knows")), "{typed}: said so");
    }
    let mut s = first_run();
    typing(&mut s, "sk-made-up-0123-made-up-4567-abcd");
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.pending_key.as_deref(), Some("sk-made-up-0123-made-up-4567-abcd"), "a known start is held");
}

#[test]
fn typed_text_is_a_key_only_with_a_known_start_and_is_masked_from_the_first_character_after_it() {
    let mut s = first_run();
    s.paste("a");
    s.step = Step::Provider;
    s.fill_providers();
    // a model id of 17 characters is a model id: shown as typed, and Enter does not hold it as a key
    typing(&mut s, "deepseek-reasoner");
    assert!(!looks_like_secret("deepseek-reasoner"), "shown as typed");
    press(&mut s, KeyCode::Enter);
    assert!(s.pending_key.is_none(), "not a key");
    assert_eq!(s.list.query, "deepseek-reasoner", "the search stays what it was");
    s.list.query.clear();
    s.refilter();
    // a known start: the start may be drawn, nothing after it ever is
    typing(&mut s, "gsk_ab");
    assert!(looks_like_secret(&s.list.query), "masked as soon as one character follows the known start");
    assert!(!looks_like_secret("gsk_"), "a start alone is only a start");
    typing(&mut s, "cdefghijklmnop");
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.pending_key.as_deref(), Some("gsk_abcdefghijklmnop"), "a known start and long enough: a key");
    // the start several services share (`sk-`): a terminal that hands a paste over as typed keys must
    // not draw such a key in clear, and Enter keeps it like a pasted one
    let mut s = first_run();
    s.paste("a");
    s.step = Step::Provider;
    s.fill_providers();
    typing(&mut s, "sk-m");
    assert!(looks_like_secret(&s.list.query), "masked from the first character after sk-");
    assert!(!looks_like_secret("sk-") && !looks_like_secret("sky-model"), "a start alone, or another word");
    typing(&mut s, "0-made-up-0123-made-up-4567-abcd");
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.pending_key.as_deref(), Some("sk-m0-made-up-0123-made-up-4567-abcd"));
}

#[test]
fn an_answer_with_the_open_counter_but_another_name_is_ignored() {
    let mut s = first_run();
    let SetupOut::Send(AgentCommand::Probe { seq, .. }) = s.paste("sk-ant-api03-madeupmadeupmade") else {
        panic!("a probe")
    };
    s.probed_from("deepseek", seq, Ok(deepseek_probe()));
    assert!(s.busy.is_some() && s.probe.is_none(), "only the name told it apart");
}

#[test]
fn esc_makes_the_answer_of_the_stopped_check_late_even_for_a_wait_that_began_without_a_new_check() {
    let mut s = first_run();
    let SetupOut::Send(AgentCommand::Probe { seq, .. }) = s.paste("sk-ant-api03-madeupmadeupmade") else {
        panic!("a probe")
    };
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Send(AgentCommand::CancelCheck));
    // another wait begins on the same provider without a check of its own (the screen is waiting again)
    s.busy = Some("waiting".into());
    s.probed_from("anthropic", seq, Ok(deepseek_probe()));
    assert!(s.busy.is_some() && s.probe.is_none(), "the stopped check's answer is late");
}

#[test]
fn picking_a_model_already_set_up_with_a_paste_held_says_the_one_line_and_waits() {
    let mut s = setup();
    s.step = Step::Provider;
    s.fill_providers();
    s.pending_key = Some(HELD.into());
    typing(&mut s, "deepseek-chat");
    for _ in 0..4 {
        if s.list.current().is_some_and(|i| i.value.starts_with("use:")) {
            break;
        }
        press(&mut s, KeyCode::Down);
    }
    assert!(s.list.current().is_some_and(|i| i.value.starts_with("use:")), "{:?}", s.list.current());
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing, "the wizard does not close on a dropped paste");
    assert!(s.pending_key.is_none());
    assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
    assert!(matches!(press(&mut s, KeyCode::Enter), SetupOut::SendAndClose(AgentCommand::SetModel { .. })));
}

#[test]
fn a_held_paste_goes_only_to_the_services_of_nexos_own_list_by_the_entry_picked() {
    // a provider of the person's own on a host name of the local network: not in the list
    let lan = ProviderConfig::new("openai-chat", "https://gpu-box.corp.example/v1", Some("NEXO_T_LAN"));
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_LAN_DS"));
    ds.default_model = Some("deepseek-chat".into());
    no_auth();
    for (name, cfg, goes) in [("gpu-box", lan.clone(), false), ("deepseek", ds, true)] {
        let mut s = Setup::new(vec![(name.into(), cfg)], Vec::new(), "");
        s.paste("a");
        s.step = Step::Provider;
        s.fill_providers();
        s.pending_key = Some(HELD.into());
        let out = s.choose(&format!("new:{name}"));
        let sent = matches!(out, SetupOut::Send(AgentCommand::Probe { key: Some(_), .. })) || s.key.is_some();
        assert_eq!(sent, goes, "{name}: {out:?}");
        if !goes {
            assert_eq!(s.error.as_deref(), Some(HELD_DROPPED));
        }
    }
    // a built-in name pointed at a host of the person's own is not the built-in service either
    let moved = ProviderConfig::new("openai-chat", "http://nas.local:8000/v1", Some("NEXO_T_LAN"));
    let mut s = Setup::new(vec![("deepseek".into(), moved)], Vec::new(), "");
    s.pending_key = Some(HELD.into());
    s.choose("new:deepseek");
    assert!(s.key.is_none(), "kept nothing for an address that is not the service's");
}

#[test]
fn a_key_typed_where_a_model_name_goes_is_masked_and_not_sent_as_a_name() {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    typing(&mut s, "sk-good-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(deepseek_probe()));
    assert_eq!(s.step, Step::Model);
    let k = "sk-ant-0123456789abcdef0123456789abcdef";
    s.list.query = k.into();
    assert!(!row_text(&s, 100).contains(k), "masked on screen");
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing, "not sent as a model");
    assert!(s.list.query.is_empty() && s.error.is_some());
}

#[test]
fn the_last_screen_says_only_you_can_read_it_only_where_the_mode_is_set() {
    let mut s = first_run();
    s.done =
        Some(Done { model: "deepseek/m".into(), config: "/c.toml".into(), key: Some("/k".into()), private: false });
    s.step = Step::Done;
    let t = row_text(&s, 100);
    assert!(t.contains("The key is in /k") && !t.contains("only you can read it"), "{t}");
    s.done.as_mut().unwrap().private = true;
    assert!(row_text(&s, 100).contains("only you can read it"));
}

#[test]
fn esc_on_the_first_screen_clears_hidden_text_first_and_a_second_esc_closes() {
    let mut s = first_run();
    s.paste("some words");
    assert!(!s.field.is_empty());
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Nothing);
    assert!(s.field.is_empty());
    assert_eq!(press(&mut s, KeyCode::Esc), SetupOut::Close);
}

#[test]
fn nothing_is_busy_when_the_probe_to_check_with_does_not_exist() {
    let mut s = first_run();
    s.step = Step::Model;
    s.probe = None;
    s.list.query = "some-model".into();
    let _ = press(&mut s, KeyCode::Enter);
    assert!(s.busy.is_none(), "{:?}", s.busy);
}
