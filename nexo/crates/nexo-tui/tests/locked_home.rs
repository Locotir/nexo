//! The home stays locked until a model works, and the sign-in card of the setup.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::{AgentCommand, AgentEvent};
use nexo_core::ProviderConfig;
use nexo_tui::modal::setup::{Setup, SetupOut, Step};
use nexo_tui::modal::Modal;
use nexo_tui::theme::Theme;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

/// A provider that is signed in with a subscription login, and none is saved.
fn codex() -> ProviderConfig {
    let mut c = ProviderConfig::new("openai-chat", "https://codex.example", None);
    c.login = Some("codex".into());
    c
}

fn app(provider: &str, cfg: ProviderConfig) -> App {
    // no credential store of the machine is read
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: provider.into(),
        model: "m".into(),
        providers: vec![(provider.to_string(), cfg)],
        theme: "dark".into(),
        animations: false,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn screen(a: &mut App) -> String {
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer()).join("\n")
}

fn press(a: &mut App, code: KeyCode) {
    event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
}

fn limits(provider: &str, source: &str) -> AgentEvent {
    AgentEvent::ModelLimits {
        provider: provider.into(),
        model: "m".into(),
        pricing: None,
        context_window: None,
        max_output: None,
        source: source.into(),
    }
}

#[test]
fn a_provider_that_is_not_ready_shows_the_card_and_typing_opens_the_setup() {
    let mut a = app("codex", codex());
    assert!(a.locked);
    let s = screen(&mut a);
    assert!(s.contains("Set up a model to start") && s.contains("n e x o"), "{s}");
    press(&mut a, KeyCode::Char('h'));
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "typing opens the setup");
    assert!(a.input.is_empty(), "the draft stays empty");
    a.modal = None;
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "Enter opens it too");
}

#[test]
fn leaving_the_setup_with_esc_keeps_the_card() {
    let mut a = app("codex", codex());
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none() && a.locked);
    assert!(screen(&mut a).contains("Set up a model to start"));
}

#[test]
fn the_home_opens_when_a_model_works() {
    let mut a = app("codex", codex());
    // the agent starting up on the provider is no choice; a model chosen in the setup is
    a.handle_agent(limits("codex", "catalogue"));
    assert!(a.locked);
    a.handle_agent(AgentEvent::ModelChanged { provider: "codex".into(), model: "m".into() });
    assert!(!a.locked);
    assert!(!screen(&mut a).contains("Set up a model to start"));
    press(&mut a, KeyCode::Char('h'));
    assert!(a.modal.is_none() && !a.input.is_empty(), "the draft takes the text now");
}

#[test]
fn a_ready_provider_shows_the_normal_home_at_once() {
    std::env::set_var("NEXO_T_LOCK_KEY", "k");
    let a = app("deepseek", ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_LOCK_KEY")));
    assert!(!a.locked);
    let mut a = a;
    assert!(!screen(&mut a).contains("Set up a model to start"));
}

fn login_setup() -> Setup {
    let mut s = Setup::new(vec![("codex".into(), codex())], vec![], "codex/m");
    s.name = "codex".into();
    s.cfg = codex();
    s.step = Step::Login;
    s
}

fn text(s: &Setup) -> String {
    let (lines, _) = s.lines(80, &Theme::nexo(), 0, false);
    lines
        .iter()
        .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_sign_in_card_shows_the_host_and_the_spaced_code_not_the_raw_link() {
    let mut s = login_setup();
    let url = "https://auth.example.com/oauth/device?user_code=ABCD1234&state=0123456789abcdef0123456789abcdef";
    s.busy = Some("waiting for the browser".into());
    s.login_prompt(url.into(), "ABCD-1234".into());
    let t = text(&s);
    assert!(t.contains("auth.example.com") && t.contains("in your browser"), "{t}");
    assert!(!t.contains("user_code") && !t.contains("0123456789abcdef"), "{t}");
    assert!(t.contains("A B C D - 1 2 3 4"), "{t}");
    assert!(t.contains("c copies the link"), "{t}");
    let out = s.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    assert_eq!(out, SetupOut::Copy(url.into()));
}

#[test]
fn a_failed_sign_in_is_one_line_with_what_the_keys_do() {
    let mut s = login_setup();
    s.busy = Some("waiting for the browser".into());
    let out = s.logged_in(Err("the sign-in was refused\nstack of details".into()));
    assert_eq!(out, SetupOut::Nothing);
    let t = text(&s);
    assert!(t.contains("✗ the sign-in was refused") && !t.contains("stack of details"), "{t}");
    assert!(t.contains("Enter tries again, Esc goes back"), "{t}");
}

#[test]
fn a_sign_in_that_worked_says_so_before_moving_on() {
    let mut s = login_setup();
    s.busy = Some("waiting for the browser".into());
    let out = s.logged_in(Ok(()));
    assert!(matches!(out, SetupOut::Send(AgentCommand::Probe { .. })), "{out:?}");
    assert_eq!(s.step, Step::Address);
    assert!(text(&s).contains("✓ signed in to codex"));
}

#[test]
fn no_provider_chosen_opens_on_the_card_without_naming_one() {
    let mut a = app("codex", codex());
    a.opts.provider = String::new();
    a.opts.model = String::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut a = App::new(a.opts.clone(), tx);
    assert!(a.locked);
    let s = screen(&mut a);
    assert!(s.contains("Set up a model to start") && !s.to_lowercase().contains("anthropic"), "{s}");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Some(Modal::Setup(_))));
}

#[test]
fn an_agent_on_a_placeholder_neither_names_it_nor_opens_the_home() {
    let mut a = app("codex", codex());
    a.opts.provider = String::new();
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut a = App::new(a.opts.clone(), tx);
    a.handle_agent(limits("anthropic", "catalogue"));
    assert!(a.locked && a.provider.is_empty());
    assert!(!screen(&mut a).to_lowercase().contains("anthropic"));
}

#[test]
fn a_scan_that_answered_before_the_dialog_opened_leaves_no_note() {
    let mut a = app("codex", codex());
    a.handle_agent(AgentEvent::LocalServers { found: vec![] });
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Some(Modal::Setup(_))));
    assert!(!screen(&mut a).contains("Looking for Ollama"));
}

fn ollama() -> ProviderConfig {
    ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None)
}

fn type_line(a: &mut App, line: &str) {
    for c in line.chars() {
        press(a, KeyCode::Char(c));
    }
}

#[test]
fn the_first_screen_answers_help_and_esc_comes_back() {
    let mut a = app("codex", codex());
    let s = screen(&mut a);
    assert!(s.contains("/help lists the commands") && s.contains("Ctrl+D"), "{s}");
    type_line(&mut a, "/help");
    assert!(a.modal.is_none(), "typing a command opens nothing yet");
    assert!(screen(&mut a).contains("/help"), "the draft is shown");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Some(Modal::Help { .. })), "help opens, not the setup");
    press(&mut a, KeyCode::Esc);
    assert!(a.modal.is_none() && a.locked);
    assert!(screen(&mut a).contains("Set up a model to start"));
}

#[test]
fn the_question_mark_opens_help_on_the_first_screen() {
    let mut a = app("codex", codex());
    press(&mut a, KeyCode::Char('?'));
    assert!(matches!(a.modal, Some(Modal::Help { .. })));
}

#[test]
fn a_command_that_needs_a_model_and_a_plain_prompt_open_the_setup() {
    let mut a = app("codex", codex());
    type_line(&mut a, "/compact");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "/compact needs a model");
    a.modal = None;
    assert!(a.input.is_empty());
    type_line(&mut a, "fix the bug");
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "a plain prompt opens the setup");
}

#[test]
fn quit_works_on_the_first_screen() {
    let mut a = app("codex", codex());
    type_line(&mut a, "/quit");
    press(&mut a, KeyCode::Enter);
    assert!(a.should_quit);
}

#[test]
fn a_chosen_provider_that_did_not_answer_is_named_with_its_address() {
    let mut a = app("ollama", ollama());
    assert!(a.locked);
    a.handle_agent(AgentEvent::LocalServers { found: vec![] });
    let s = screen(&mut a);
    assert!(s.contains("ollama did not answer at http://localhost:11434/v1"), "{s}");
    assert!(s.contains("ollama serve") && s.contains("Enter opens the setup"), "{s}");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "the setup is one key away");
}

#[test]
fn a_command_that_arrives_as_a_paste_stays_a_command_on_the_first_screen() {
    let mut a = app("codex", codex());
    event::handle(&mut a, crossterm::event::Event::Paste("/help".into()));
    assert!(a.modal.is_none() && a.input.full_text() == "/help");
    press(&mut a, KeyCode::Enter);
    assert!(matches!(a.modal, Some(Modal::Help { .. })));
    press(&mut a, KeyCode::Esc);
    event::handle(&mut a, crossterm::event::Event::Paste("fix it".into()));
    assert!(matches!(a.modal, Some(Modal::Setup(_))), "a pasted prompt opens the setup");
}

#[test]
fn the_first_ctrl_c_on_the_first_screen_says_to_press_it_again() {
    let mut a = app("codex", codex());
    event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(screen(&mut a).contains("Ctrl+C again to quit"), "{}", screen(&mut a));
}
