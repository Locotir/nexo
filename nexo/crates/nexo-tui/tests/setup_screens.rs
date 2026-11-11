//! The screens of the first run, drawn on the default (quiet) interface at 110x34 and 80x24, with the
//! mouse on, and again with the ASCII fallback: each one whole, in the frame, with its one question.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::{ModelCheck, ModelInfo, ProviderConfig, ServerProbe};
use nexo_tui::modal::setup::{Setup, SetupOut};
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

const SIZES: [(u16, u16); 2] = [(110, 34), (80, 24)];

/// Nothing is chosen yet: a keyed provider with a default model, a subscription and a local server.
fn first_run() -> Setup {
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let mut ds = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("NEXO_T_SCREENS_DS"));
    ds.default_model = Some("deepseek-chat".into());
    let mut cx = ProviderConfig::new("openai-responses", "https://chatgpt.example/backend-api/codex", None);
    cx.login = Some("codex".into());
    let ol = ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None);
    Setup::new(vec![("deepseek".into(), ds), ("codex".into(), cx), ("ollama".into(), ol)], Vec::new(), "")
}

fn app_with(s: Setup) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    // the interface as it comes: the quiet detail, the mouse on
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        theme: "dark".into(),
        mouse: true,
        animations: false,
        ..TuiOptions::default()
    };
    assert_eq!(opts.detail, nexo_core::config::Detail::Quiet);
    let mut a = App::new(opts, tx);
    a.modal = Some(Modal::Setup(Box::new(s)));
    a
}

fn press(s: &mut Setup, code: KeyCode) -> SetupOut {
    s.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typing(s: &mut Setup, text: &str) {
    for c in text.chars() {
        press(s, KeyCode::Char(c));
    }
}

fn probe() -> ServerProbe {
    ServerProbe {
        kind: "openai-compatible".into(),
        protocol: "openai-chat".into(),
        base_url: "https://api.deepseek.com".into(),
        models: vec![
            ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() },
            ModelInfo { id: "deepseek-reasoner".into(), reasoning: true, ..Default::default() },
        ],
        ..Default::default()
    }
}

fn at_key() -> Setup {
    let mut s = first_run();
    press(&mut s, KeyCode::Enter);
    s
}

fn at_model() -> Setup {
    let mut s = at_key();
    typing(&mut s, "sk-made-up-key-0000");
    press(&mut s, KeyCode::Enter);
    s.probed(Ok(probe()));
    s
}

fn at_review() -> Setup {
    let mut s = at_model();
    press(&mut s, KeyCode::Enter);
    s.checked(ModelCheck {
        info: ModelInfo { id: "deepseek-chat".into(), context_window: Some(128_000), ..Default::default() },
        latency_ms: Some(640),
        called_tool: true,
        ..Default::default()
    });
    s
}

fn screens() -> Vec<(&'static str, Setup)> {
    let mut start_local = first_run();
    start_local.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    let mut more = first_run();
    press(&mut more, KeyCode::Down);
    press(&mut more, KeyCode::Down);
    press(&mut more, KeyCode::Enter);
    let mut trouble = at_key();
    typing(&mut trouble, "sk-wrong-key-0000");
    press(&mut trouble, KeyCode::Enter);
    trouble.probed(Ok(ServerProbe { needs_key: true, ..Default::default() }));
    let mut check_failed = at_model();
    press(&mut check_failed, KeyCode::Enter);
    check_failed.checked(ModelCheck { error: Some("HTTP 402: insufficient balance".into()), ..Default::default() });
    let mut done = at_review();
    press(&mut done, KeyCode::Enter);
    done.saved("deepseek", "deepseek-chat");
    let mut unreachable = first_run();
    unreachable.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    press(&mut unreachable, KeyCode::Enter);
    unreachable.probed(Err("nothing answers at http://localhost:11434/v1: connection refused".into()));
    let mut empty = first_run();
    empty.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    press(&mut empty, KeyCode::Enter);
    empty.probed(Ok(ServerProbe {
        kind: "ollama".into(),
        protocol: "openai-chat".into(),
        base_url: "http://localhost:11434/v1".into(),
        ..Default::default()
    }));
    vec![
        ("start", first_run()),
        ("start with a local server", start_local),
        ("more providers", more),
        ("key", at_key()),
        ("key refused", trouble),
        ("local server down", unreachable),
        ("local server with no models", empty),
        ("models", at_model()),
        ("works", at_review()),
        ("check failed", check_failed),
        ("done", done),
    ]
}

fn draw(s: Setup, w: u16, h: u16, ascii: bool) -> Vec<String> {
    let mut a = app_with(s);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let mut buf = t.backend().buffer().clone();
    if ascii {
        nexo_tui::ascii::fold(&mut buf);
    }
    rows_of(&buf)
}

fn dialog(rows: &[String]) -> &[String] {
    let top = rows.iter().position(|r| r.contains("set up a model")).expect("the title");
    let bottom = rows.iter().rposition(|r| r.contains('╰')).expect("the bottom");
    &rows[top..=bottom.max(top)]
}

#[test]
fn every_screen_is_whole_inside_the_frame_at_both_sizes_and_in_ascii() {
    for (w, h) in SIZES {
        for ascii in [false, true] {
            for (name, s) in screens() {
                let rows = draw(s, w, h, ascii);
                let all = rows.join("\n");
                assert!(rows.iter().any(|r| r.contains("set up a model")), "{name} {w}x{h}: no title\n{all}");
                for r in &rows {
                    assert!(r.chars().count() <= w as usize, "{name} {w}x{h}: {r:?}");
                    if ascii {
                        assert!(
                            r.is_ascii() || r.chars().all(|c| c.is_ascii() || ('\u{2800}'..='\u{28ff}').contains(&c)),
                            "{name} ascii: {r:?}"
                        );
                    }
                }
                // the draft line below is still there
                assert!(
                    rows.iter().any(|r| r.starts_with('›') || r.starts_with('>')),
                    "{name} {w}x{h}: no draft\n{all}"
                );
            }
        }
    }
}

#[test]
fn the_first_screen_is_one_sentence_a_few_choices_and_one_recommended() {
    for (w, h) in SIZES {
        let rows = draw(first_run(), w, h, false);
        let d = dialog(&rows);
        let all = d.join("\n");
        assert_eq!(all.matches("recommended").count(), 1, "{w}x{h}\n{all}");
        for (i, r) in d.iter().enumerate() {
            if r.contains("recommended") {
                assert!(r.contains("Use deepseek"), "{w}x{h}: {r}");
                assert!(i <= 5, "the recommended choice is near the top: {all}");
            }
        }
        assert!(all.contains("More providers"), "{all}");
        // the title, one sentence, the scan note, a gap and three choices, and the frame's bottom
        assert!(d.len() <= 9, "{w}x{h}: {} rows\n{all}", d.len());
        for word in ["endpoint", "token", "slug", "base URL", "OAuth"] {
            assert!(!all.contains(word), "{word}\n{all}");
        }
    }
}

#[test]
fn models_on_this_computer_lead_the_first_screen_and_say_they_are_free() {
    for (w, h) in SIZES {
        let rows = draw(s_clone_local(), w, h, false);
        let d = dialog(&rows);
        let first = d.iter().find(|r| r.contains("Use ")).expect("a choice");
        assert!(first.contains("Use the models on this computer (free)"), "{w}x{h}: {first}\n{}", d.join("\n"));
    }
}

fn s_clone_local() -> Setup {
    let mut s = first_run();
    s.found_local(vec![("ollama".into(), "Ollama 0.12.6".into())]);
    s
}

#[test]
fn the_key_screen_shows_the_page_dots_and_the_last_four_and_no_key() {
    for (w, h) in SIZES {
        let mut s = at_key();
        s.paste("  \"sk-made-up-key-ab12\"  ");
        let rows = draw(s, w, h, false);
        let all = dialog(&rows).join("\n");
        assert!(all.contains("https://platform.deepseek.com/api_keys"), "{w}x{h}\n{all}");
        assert!(all.contains("\u{2022}ab12") && !all.contains("made-up"), "{w}x{h}\n{all}");
        assert!(!all.contains('"'), "the quotes of a paste are gone: {all}");
    }
}

#[test]
fn a_refused_key_shows_the_card_in_at_most_three_short_lines() {
    for (w, h) in SIZES {
        let mut s = at_key();
        typing(&mut s, "sk-wrong-key-0000");
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(ServerProbe { needs_key: true, ..Default::default() }));
        let rows = draw(s, w, h, false);
        let d = dialog(&rows);
        let all = d.join("\n");
        assert!(all.contains("401") || all.contains("refused") || all.contains("rejected"), "{w}x{h}\n{all}");
        assert!(all.contains("paste the key again"), "{w}x{h}\n{all}");
        let body: Vec<&String> = d.iter().skip(1).filter(|r| r.chars().any(|c| c.is_alphanumeric())).collect();
        assert!(body.len() <= 4, "{w}x{h}: {} lines\n{all}", body.len());
        assert!(!all.contains("sk-wrong"), "the key is never shown: {all}");
    }
}

#[test]
fn the_models_screen_says_what_the_chosen_one_is_for() {
    for (w, h) in SIZES {
        let rows = draw(at_model(), w, h, false);
        let all = dialog(&rows).join("\n");
        assert!(all.contains("The key works"), "{w}x{h}\n{all}");
        assert!(all.contains("suggests for everyday work"), "{w}x{h}\n{all}");
        assert!(all.contains("deepseek-chat") && all.contains("deepseek-reasoner"), "{w}x{h}\n{all}");
    }
}

#[test]
fn the_review_is_one_line_of_good_news_and_the_last_screen_names_the_files() {
    for (w, h) in SIZES {
        let all = dialog(&draw(at_review(), w, h, false)).join("\n");
        assert!(all.contains("Works: deepseek-chat answered in"), "{w}x{h}\n{all}");
        assert!(!all.contains("context window"), "the facts wait behind Tab: {all}");
        let mut s = at_review();
        press(&mut s, KeyCode::Enter);
        s.saved("deepseek", "deepseek-chat");
        let all = dialog(&draw(s, w, h, false)).join("\n");
        assert!(all.contains("deepseek/deepseek-chat is ready"), "{w}x{h}\n{all}");
        assert!(all.contains("Saved in") && all.contains("The key is in"), "{w}x{h}\n{all}");
        assert!(all.contains("/model") && all.contains("/help") && all.contains("Enter"), "{w}x{h}\n{all}");
    }
}

#[test]
fn a_server_that_is_down_shows_its_card_and_one_with_nothing_installed_says_what_to_do() {
    for (w, h) in SIZES {
        let mut s = s_clone_local();
        press(&mut s, KeyCode::Enter);
        s.probed(Err("nothing answers at http://localhost:11434/v1: connection refused".into()));
        let all = dialog(&draw(s, w, h, false)).join("\n");
        assert!(all.to_lowercase().contains("localhost") || all.to_lowercase().contains("reach"), "{w}x{h}\n{all}");
        assert!(!all.contains("endpoint"), "{all}");

        let mut s = s_clone_local();
        press(&mut s, KeyCode::Enter);
        s.probed(Ok(ServerProbe {
            kind: "ollama".into(),
            protocol: "openai-chat".into(),
            base_url: "http://localhost:11434/v1".into(),
            ..Default::default()
        }));
        let all = dialog(&draw(s, w, h, false)).join("\n");
        assert!(all.contains("ollama pull") && all.contains("on this computer"), "{w}x{h}\n{all}");
        assert!(!all.contains("no match"), "{w}x{h}\n{all}");
    }
}

#[test]
fn the_default_screen_has_no_fixed_row_added_below_or_above_the_dialog() {
    // the frame outside the dialog is what it was: the draft line and one status row, nothing else
    for (w, h) in SIZES {
        let rows = draw(first_run(), w, h, false);
        let below = rows.iter().rposition(|r| r.contains('╰')).unwrap();
        let after: Vec<&String> = rows[below + 1..].iter().filter(|r| !r.trim().is_empty()).collect();
        assert!(after.len() <= 4, "{w}x{h}: {after:?}");
    }
}
