//! Status lights, the setup save, and what counts as news when scrolled up.

use super::*;

#[test]
fn a_new_model_or_mode_in_the_status_bar_lights_up_once_and_the_first_look_does_not() {
    let (mut a, _rx) = app();
    let t0 = std::time::Instant::now();
    a.fx.clock = Some(t0);
    assert!(!a.tick_fx(), "the first look at the bar is not a change");
    let model_fg = |a: &App| {
        let segs = crate::ui::title_segments(a, 200);
        segs.iter().flatten().find(|s| s.text.contains("mock")).and_then(|s| s.style.fg).unwrap()
    };
    let own = model_fg(&a);
    a.model = "n".into();
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(10));
    assert!(a.tick_fx(), "a change asks for frames while it fades");
    assert_ne!(model_fg(&a), own, "the new model is lit");
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(crate::motion::CUT_MS + 300));
    assert!(!a.tick_fx(), "and the frames stop when it has settled");
    assert_eq!(model_fg(&a), own);
    // with the motion off nothing lights
    a.opts.animations = false;
    a.permission_mode = nexo_core::permission::PermissionMode::Plan;
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(crate::motion::CUT_MS + 400));
    assert!(!a.tick_fx());
}

#[test]
fn a_model_saved_from_the_setup_leaves_it_open_on_its_last_screen_until_the_agent_confirms() {
    use crate::modal::setup::{Setup, Step};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nexo_core::{ModelCheck, ModelInfo, ProviderConfig, ServerProbe};
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (mut a, mut rx) = app();
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_APP_SETUP_DS"));
    ds.default_model = Some("m1".into());
    let mut s = Setup::new(vec![("deepseek".into(), ds)], Vec::new(), "");
    let press = |s: &mut Setup, code: KeyCode| s.key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut s, KeyCode::Enter);
    for c in "sk-made-up-key-0000".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(ServerProbe {
        protocol: "openai-chat".into(),
        base_url: "https://api.deepseek.com".into(),
        models: vec![ModelInfo { id: "m1".into(), ..Default::default() }],
        ..Default::default()
    }));
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck { info: ModelInfo { id: "m1".into(), ..Default::default() }, ..Default::default() });
    a.modal = Some(Modal::Setup(Box::new(s)));
    let out = a.modal.as_mut().unwrap().key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    a.modal_out(out);
    // sent, not yet offered by the switcher (only a save that went through is), and the setup is still there
    assert!(
        matches!(rx.try_recv(), Ok(AgentCommand::SaveModel { ref name, ref model, .. }) if name == "deepseek" && model == "m1")
    );
    assert!(!a.opts.models.contains(&("deepseek".to_string(), "m1".to_string())));
    let Some(Modal::Setup(st)) = &a.modal else { panic!("the setup stays open") };
    assert_eq!(st.step, Step::Review);
    // another model changing is not our save
    a.handle_agent(AgentEvent::ModelChanged { provider: "other".into(), model: "x".into() });
    let Some(Modal::Setup(st)) = &a.modal else { panic!("still open") };
    assert_eq!(st.step, Step::Review);
    // the agent's word: the last screen
    a.handle_agent(AgentEvent::ModelChanged { provider: "deepseek".into(), model: "m1".into() });
    let Some(Modal::Setup(st)) = &a.modal else { panic!("the last screen") };
    assert_eq!(st.step, Step::Done);
    assert_eq!((a.provider.as_str(), a.model.as_str()), ("deepseek", "m1"));
    assert!(a.opts.models.contains(&("deepseek".to_string(), "m1".to_string())), "offered once saved");
}

/// The setup walked to the point where Enter on the review has sent `SaveModel` (the setup still open).
fn saving_setup() -> (App, tokio::sync::mpsc::UnboundedReceiver<AgentCommand>) {
    use crate::modal::setup::Setup;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nexo_core::{ModelCheck, ModelInfo, ProviderConfig, ServerProbe};
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (mut a, mut rx) = app();
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_APP_SETUP_DS"));
    ds.default_model = Some("m1".into());
    let mut s = Setup::new(vec![("deepseek".into(), ds)], Vec::new(), "");
    let press = |s: &mut Setup, code: KeyCode| s.key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut s, KeyCode::Enter);
    for c in "sk-made-up-key-0000".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(ServerProbe {
        protocol: "openai-chat".into(),
        base_url: "https://api.deepseek.com".into(),
        models: vec![ModelInfo { id: "m1".into(), ..Default::default() }],
        ..Default::default()
    }));
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck { info: ModelInfo { id: "m1".into(), ..Default::default() }, ..Default::default() });
    a.modal = Some(Modal::Setup(Box::new(s)));
    let out = a.modal.as_mut().unwrap().key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    a.modal_out(out);
    assert!(matches!(rx.try_recv(), Ok(AgentCommand::SaveModel { .. })));
    (a, rx)
}

fn setup_text(a: &App) -> String {
    let Some(Modal::Setup(s)) = &a.modal else { panic!("the setup is open") };
    s.lines(100, &crate::theme::Theme::nexo(), 0, false)
        .0
        .iter()
        .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>() + "\n")
        .collect()
}

#[test]
fn a_failed_save_told_by_the_agent_keeps_the_open_setup_on_its_step() {
    use crate::modal::setup::Step;
    let (mut a, _rx) = saving_setup();
    a.handle_agent(AgentEvent::SaveFailed {
        name: "deepseek".into(),
        model: "m1".into(),
        reason: "Not saved: config.toml could not be written. Make it writable, then press Enter.".into(),
    });
    let Some(Modal::Setup(s)) = &a.modal else { panic!("still open") };
    assert_eq!(s.step, Step::Review);
    let t = setup_text(&a);
    assert!(t.contains("Not saved") && !t.contains("is ready"), "{t}");
    // nothing else is told: the wizard said it
    assert!(!matches!(a.blocks.last(), Some(Block::Notice { level: Level::Warn, .. })));
}

#[test]
fn a_failed_save_whose_wizard_was_closed_still_shows_one_warning() {
    let (mut a, _rx) = saving_setup();
    a.modal = None;
    let before = a.blocks.len();
    a.handle_agent(AgentEvent::SaveFailed {
        name: "deepseek".into(),
        model: "m1".into(),
        reason: "Not saved: config.toml could not be written. Make it writable, then press Enter.".into(),
    });
    assert_eq!(a.blocks.len(), before + 1);
    assert!(
        matches!(a.blocks.last(), Some(Block::Notice { level: Level::Warn, text }) if text.starts_with("Not saved")),
        "{:?}",
        a.blocks.last()
    );
}

#[test]
fn the_last_screen_says_only_you_can_read_it_where_the_file_mode_is_set() {
    use crate::modal::setup::Step;
    let (mut a, _rx) = saving_setup();
    a.handle_agent(AgentEvent::ModelChanged { provider: "deepseek".into(), model: "m1".into() });
    let Some(Modal::Setup(s)) = &a.modal else { panic!("the last screen") };
    assert_eq!(s.step, Step::Done);
    let t = setup_text(&a);
    assert_eq!(t.contains("only you can read it"), cfg!(unix), "{t}");
    assert!(t.contains("key is in") || t.contains("Key is in") || t.contains("The key is in"), "{t}");
}

#[test]
fn opening_a_block_while_scrolled_up_is_not_news() {
    let (mut a, _rx) = app();
    a.opts.animations = false;
    long_answer(&mut a, 0);
    a.push(card("c1", 30));
    long_answer(&mut a, 100);
    frame(&mut a, 60, 20);
    a.toggle_block(1);
    frame(&mut a, 60, 20);
    a.scroll_to_line(0, false);
    frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 0);
    {
        let before = a.total_lines;
        a.toggle_block(1);
        frame(&mut a, 60, 20);
        assert_ne!(a.total_lines, before, "the fold changed the height");
        assert!(!a.follow);
        assert_eq!(a.unseen(), 0, "the rows a fold shows were not sent by anyone");
    }
    long_answer(&mut a, 200);
    frame(&mut a, 60, 20);
    assert!(a.unseen() > 0, "a real arrival still counts");
}

#[test]
fn a_new_line_in_an_empty_box_takes_the_placeholder_away() {
    let (mut a, _rx) = bare();
    a.opts.animations = false;
    assert!(screen(&mut a, 80, 24).iter().any(|r| r.contains("ask anything")), "an untouched box says what it takes");
    crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    assert_eq!(a.input.line_count(), 2);
    let rows = screen(&mut a, 80, 24);
    assert!(
        !rows.iter().any(|r| r.contains("ask anything")),
        "the caret is on line 2, not on the placeholder: {rows:?}"
    );
}

#[test]
fn opening_or_selecting_a_block_never_makes_the_new_pill_count() {
    let (mut a, _rx) = app();
    long_answer(&mut a, 0);
    a.push(card("x", 30));
    frame(&mut a, 60, 20);
    assert!(a.follow);
    a.select_last();
    frame(&mut a, 60, 20);
    let i = a.selected.unwrap();
    a.toggle_block(i);
    frame(&mut a, 60, 20);
    for _ in 0..4 {
        a.scroll_by(-3);
        let rows = frame(&mut a, 60, 20);
        assert_eq!(a.unseen(), 0, "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains(" new ")), "{rows:?}");
    }
    a.select_step(false);
    frame(&mut a, 60, 20);
    assert_eq!(a.unseen(), 0);
    // output that really arrives while scrolled up still counts
    long_answer(&mut a, 100);
    let rows = frame(&mut a, 60, 20);
    assert!(a.unseen() > 0);
    assert!(rows.iter().any(|r| r.contains(" new ")), "{rows:?}");
}
