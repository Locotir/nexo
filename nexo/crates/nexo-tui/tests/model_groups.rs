//! The first step of the model setup: one fold per ready provider, the others folded away,
//! the row for a server of one's own last.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentCommand;
use nexo_core::ProviderConfig;
use nexo_tui::modal::setup::{Setup, SetupOut};
use nexo_tui::theme::Theme;

fn setup() -> Setup {
    let cfg = |url: &str, env: &str| ProviderConfig::new("openai-chat", url, Some(env));
    let providers = vec![
        ("deepseek".to_string(), cfg("https://api.deepseek.com", "NEXO_T_NO_KEY_1")),
        ("codex".to_string(), cfg("https://codex.example", "NEXO_T_NO_KEY_2")),
        ("anthropic".to_string(), cfg("https://api.anthropic.com", "NEXO_T_NO_KEY_3")),
        ("cerebras".to_string(), cfg("https://api.cerebras.ai", "NEXO_T_NO_KEY_4")),
        ("xai".to_string(), cfg("https://api.x.ai", "NEXO_T_NO_KEY_5")),
    ];
    let mut models: Vec<(String, String)> = (0..2).map(|i| ("deepseek".to_string(), format!("chat-{i}"))).collect();
    models.extend((0..20).map(|i| ("codex".to_string(), format!("gpt-{i}"))));
    Setup::new(providers, models, "deepseek/chat-0")
}

fn press(s: &mut Setup, code: KeyCode) -> SetupOut {
    s.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typing(s: &mut Setup, text: &str) {
    for c in text.chars() {
        press(s, KeyCode::Char(c));
    }
}

fn values(s: &Setup) -> Vec<String> {
    s.list.items.iter().map(|i| i.value.clone()).collect()
}

fn text(s: &Setup) -> String {
    let (lines, _) = s.lines(100, &Theme::nexo(), 0, false);
    lines
        .iter()
        .map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn select(s: &mut Setup, value: &str) {
    s.list.selected = s.list.items.iter().position(|i| i.value == value).unwrap_or_else(|| panic!("no row {value}"));
}

#[test]
fn the_flat_list_is_gone() {
    let s = setup();
    let t = text(&s);
    assert!(!t.contains("codex/gpt-"), "{t}");
    assert!(!t.contains("set up anthropic"), "{t}");
    assert!(t.contains("▸ codex") && t.contains("20 models"), "{t}");
    assert!(t.lines().count() < 16, "{t}");
}

#[test]
fn only_the_group_in_use_is_open_at_first() {
    let s = setup();
    let v = values(&s);
    assert!(v.contains(&"use:deepseek/chat-0".to_string()));
    assert!(!v.iter().any(|x| x.starts_with("use:codex/")), "{v:?}");
    let t = text(&s);
    assert!(t.contains("▾ deepseek") && t.contains("● in use"), "{t}");
    assert_eq!(s.list.current().unwrap().value, "use:deepseek/chat-0");
}

#[test]
fn a_header_opens_and_closes_by_key() {
    let mut s = setup();
    select(&mut s, "group:codex");
    assert_eq!(press(&mut s, KeyCode::Enter), SetupOut::Nothing);
    assert_eq!(values(&s).iter().filter(|x| x.starts_with("use:codex/")).count(), 20);
    assert_eq!(s.list.current().unwrap().value, "group:codex", "the cursor stays on the header");
    assert!(text(&s).contains("▾ codex"));
    // the sign-in again row is the last one in the group
    let v = values(&s);
    let at = v.iter().position(|x| x == "use:codex/gpt-19").unwrap();
    assert_eq!(v[at + 1], "new:codex");
    press(&mut s, KeyCode::Left);
    assert!(!values(&s).iter().any(|x| x.starts_with("use:codex/")));
    press(&mut s, KeyCode::Right);
    assert!(values(&s).iter().any(|x| x.starts_with("use:codex/")));
    press(&mut s, KeyCode::Char(' '));
    assert!(!values(&s).iter().any(|x| x.starts_with("use:codex/")), "space toggles too");
}

#[test]
fn the_providers_not_ready_are_folded_with_their_count() {
    let mut s = setup();
    let t = text(&s);
    assert!(t.contains("▸ others") && t.contains("3 providers"), "{t}");
    assert!(!values(&s).contains(&"new:anthropic".to_string()));
    select(&mut s, "group:@others");
    press(&mut s, KeyCode::Enter);
    let t = text(&s);
    assert!(t.contains("set up anthropic") && t.contains("https://api.anthropic.com"), "{t}");
    assert!(t.contains("needs NEXO_T_NO_KEY_3"), "{t}");
}

#[test]
fn the_row_for_a_server_of_ones_own_is_last() {
    let mut s = setup();
    assert_eq!(values(&s).last().unwrap(), "new:");
    select(&mut s, "group:@others");
    press(&mut s, KeyCode::Enter);
    assert_eq!(values(&s).last().unwrap(), "new:");
    let t = text(&s);
    let last = t.lines().rfind(|l| !l.trim().is_empty()).unwrap();
    assert!(last.contains("set up your own server") && last.contains("OpenAI-compatible address"), "{last}");
    press(&mut s, KeyCode::End);
    assert_eq!(s.list.current().unwrap().value, "new:");
    press(&mut s, KeyCode::Home);
    assert_eq!(s.list.selected, 0);
}

#[test]
fn filtering_reaches_a_model_in_a_folded_group() {
    let mut s = setup();
    assert!(!values(&s).iter().any(|x| x.starts_with("use:codex/")));
    typing(&mut s, "gpt-17");
    assert_eq!(s.list.current().unwrap().value, "use:codex/gpt-17");
    assert!(text(&s).contains("▾ codex"));
    assert!(!values(&s).iter().any(|x| x.starts_with("use:deepseek/")));
    // clearing the filter folds it back
    s.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(!values(&s).iter().any(|x| x.starts_with("use:codex/")));
}

#[test]
fn a_model_in_an_open_group_switches_as_before() {
    let mut s = setup();
    select(&mut s, "group:codex");
    press(&mut s, KeyCode::Enter);
    select(&mut s, "use:codex/gpt-3");
    assert_eq!(
        press(&mut s, KeyCode::Enter),
        SetupOut::SendAndClose(AgentCommand::SetModel { provider: Some("codex".into()), model: "gpt-3".into() })
    );
}

#[test]
fn a_narrow_window_keeps_what_to_do_and_cuts_the_address() {
    let mut s = setup();
    select(&mut s, "group:@others");
    press(&mut s, KeyCode::Enter);
    let (lines, _) = s.lines(46, &Theme::nexo(), 0, false);
    let t: String =
        lines.iter().map(|l| l.spans.iter().map(|x| x.content.to_string()).collect::<String>() + "\n").collect();
    assert!(t.contains("needs NEXO_T_NO_KEY_3"), "{t}");
    let own = s.list.items.iter().find(|i| i.value == "new:").unwrap();
    assert!(own.hint.starts_with("any OpenAI-compatible"), "{}", own.hint);
}
