//! Modals drawn on a test backend at several sizes: whole, inside the screen, buttons reachable.

use std::path::PathBuf;

use nexo_core::agent::AgentEvent;
use nexo_core::permission::PermissionRequest;
use nexo_core::tool::{ToolDisplay, ToolKind};
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        keys: Default::default(),
        status_line: vec![],
        status_command: String::new(),
        notify_after: 15,
        animations: false,
        vim: false,
        prompt: None,
        session_id: Some("20261027-120000-ab12".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn shell_ask(command: &str) -> AgentEvent {
    AgentEvent::PermissionAsk {
        request: PermissionRequest {
            id: "p1".into(),
            tool: "bash".into(),
            kind: ToolKind::Exec,
            summary: command.into(),
            detail: ToolDisplay::Shell {
                command: command.into(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            suggested_rules: vec!["bash(cargo test*)".into(), "bash(cargo *)".into(), "bash".into()],
        },
    }
}

fn draw(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

#[test]
fn the_permission_buttons_stay_whole_and_inside_the_dialog_at_every_width() {
    for (w, h) in [(24u16, 10u16), (30, 10), (40, 12), (80, 24), (120, 40)] {
        let mut a = app();
        a.handle_agent(shell_ask("cargo test --workspace --all-features"));
        let rows = draw(&mut a, w, h);
        let row = rows
            .iter()
            .find(|r| r.contains("[y]"))
            .unwrap_or_else(|| panic!("{w}x{h}: no buttons\n{}", rows.join("\n")));
        for k in ["[y]", "[a]", "[n]"] {
            assert!(row.contains(k), "{w}x{h}: {k} missing in {row:?}");
        }
        if w >= 40 {
            assert!(row.contains("[n] deny"), "{w}x{h}: {row:?}");
        }
        assert!(!row.contains("den ") && !row.trim_end().ends_with("den"), "{w}x{h}: cut label in {row:?}");
    }
}

#[test]
fn no_dialog_runs_over_the_draft_line_at_any_size() {
    use nexo_core::permission::{Question, QuestionOption};
    use nexo_tui::modal::{Ask, Modal, PickItem, PickKind, Picker};
    for (w, h) in [(30u16, 9u16), (40, 12), (60, 16), (80, 24), (120, 40), (200, 60)] {
        for kind in ["picker", "help", "question"] {
            let mut a = app();
            a.modal = Some(match kind {
                "picker" => {
                    let items = (0..30)
                        .map(|i| PickItem {
                            label: format!("provider/model-{i}"),
                            value: i.to_string(),
                            hint: "64k ctx".into(),
                        })
                        .collect();
                    Modal::Picker(Picker::new(PickKind::Model, "Model", items, Some("3")))
                }
                "help" => Modal::Help { scroll: 0 },
                _ => Modal::Question(Box::new(Ask::new(Question {
                    id: "q".into(),
                    header: "Approach".into(),
                    question: "Which way should the change go, given the long story behind it?".into(),
                    options: (0..4)
                        .map(|i| QuestionOption { label: format!("option {i}"), description: "a description".into() })
                        .collect(),
                    multi: false,
                    plan: None,
                }))),
            });
            let rows = draw(&mut a, w, h);
            let input = rows
                .iter()
                .find(|r| r.starts_with('›'))
                .unwrap_or_else(|| panic!("{kind} {w}x{h}: no input\n{}", rows.join("\n")));
            assert!(!input.contains('╰') && !input.contains('│'), "{kind} {w}x{h}: {input:?}\n{}", rows.join("\n"));
        }
    }
}

#[test]
fn the_setup_keeps_the_line_being_typed_in_view_under_a_long_error() {
    use nexo_tui::modal::setup::{Setup, Step};
    use nexo_tui::modal::Modal;
    for (w, h) in [(40u16, 12u16), (30, 9), (80, 24)] {
        for step in [Step::Address, Step::Key] {
            let mut a = app();
            let mut s = Setup::new(vec![], vec![("deepseek".into(), "deepseek-chat".into())], "deepseek/deepseek-chat");
            s.step = step;
            s.field = "localhost".into();
            s.error = Some("could not reach http://localhost:11434: connection refused".into());
            a.modal = Some(Modal::Setup(Box::new(s)));
            let rows = draw(&mut a, w, h);
            assert!(
                rows.iter().any(|r| r.contains("› ") && r.contains("loc") || r.contains("› •")),
                "{step:?} {w}x{h}\n{}",
                rows.join("\n")
            );
            assert!(
                rows.iter().any(|r| r.contains("5") || r.contains("/5")),
                "{step:?} {w}x{h}: title\n{}",
                rows.join("\n")
            );
        }
    }
}

#[test]
fn the_model_picker_keeps_the_current_model_in_view_at_small_sizes() {
    use nexo_tui::modal::{Modal, PickItem, PickKind, Picker};
    for (w, h) in [(40u16, 12u16), (60, 16), (80, 24)] {
        let mut a = app();
        let items = (0..40)
            .map(|i| PickItem { label: format!("provider/model-{i}"), value: i.to_string(), hint: "64k ctx".into() })
            .collect();
        a.modal = Some(Modal::Picker(Picker::new(PickKind::Model, "Model", items, Some("30"))));
        let rows = draw(&mut a, w, h);
        assert!(
            rows.iter().any(|r| r.contains("model-30")),
            "{w}x{h}: current model scrolled out of view\n{}",
            rows.join("\n")
        );
    }
}

#[test]
fn a_permission_window_at_60x16_leaves_the_turn_line_alone() {
    let mut a = app();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(shell_ask("cargo test --workspace --all-features"));
    let rows = draw(&mut a, 60, 16);
    let dump = rows.join("\n");
    let turn = rows.iter().position(|r| r.contains("Esc interrupt")).unwrap_or_else(|| panic!("no turn line\n{dump}"));
    assert!(rows[turn].starts_with("─ "), "turn line: {:?}\n{dump}", rows[turn]);
    assert!(!rows[turn].contains('╯') && !rows[turn].contains('╰'), "window edge on the turn line\n{dump}");
    let input = rows.iter().find(|r| r.starts_with('›')).unwrap_or_else(|| panic!("no input\n{dump}"));
    assert!(!input.contains('╰') && !input.contains('│'), "window edge on the input\n{dump}");
    let bottom = rows.iter().rposition(|r| r.contains('╯')).unwrap_or_else(|| panic!("no window bottom\n{dump}"));
    assert!(bottom < turn, "window ends at row {bottom}, turn line at {turn}\n{dump}");
}

#[test]
fn a_short_screen_drops_the_blank_lines_so_the_rules_show() {
    let mut a = app();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(shell_ask("cargo test --workspace --all-features"));
    let rows = draw(&mut a, 60, 16);
    for k in ["1  bash(cargo test*)", "2  bash(cargo *)", "3  bash"] {
        assert!(rows.iter().any(|r| r.contains(k)), "rule {k:?} hidden\n{}", rows.join("\n"));
    }
}

#[test]
fn a_question_keeps_the_chosen_option_in_view_at_40x12() {
    use nexo_core::permission::{Question, QuestionOption};
    use nexo_tui::modal::{Ask, Modal};
    let mut a = app();
    let mut ask = Ask::new(Question {
        id: "q".into(),
        header: "Approach".into(),
        question: "Which way should the change go, given the long story behind it, and why?".into(),
        options: (0..8)
            .map(|i| QuestionOption { label: format!("option {i}"), description: "a longer description of it".into() })
            .collect(),
        multi: false,
        plan: None,
    });
    for _ in 0..6 {
        ask.key(ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Down));
    }
    a.modal = Some(Modal::Question(Box::new(ask)));
    let rows = draw(&mut a, 40, 12);
    assert!(rows.iter().any(|r| r.contains("option 6")), "chosen option out of view\n{}", rows.join("\n"));
}

#[test]
fn the_info_window_fits_every_tab_at_small_sizes() {
    use nexo_tui::modal::Modal;
    use nexo_tui::overlay::Info;
    let mut bad = Vec::new();
    for (w, h) in [(30u16, 9u16), (40, 12), (60, 16), (80, 24)] {
        for tab in 0..3 {
            let mut a = app();
            a.modal = Some(Modal::Info(Box::new(Info::new(tab).still(true))));
            let rows = draw(&mut a, w, h);
            let dump = rows.join("\n");
            let Some(input) = rows.iter().find(|r| r.starts_with('›')) else {
                bad.push(format!("tab {tab} {w}x{h}: no input\n{dump}"));
                continue;
            };
            if !(!input.contains('╰') && !input.contains('│')) {
                bad.push(format!("tab {tab} {w}x{h}: window on the input\n{dump}"));
            }
            if !(rows.iter().any(|r| r.contains('╰') && r.contains('╯'))) {
                bad.push(format!("tab {tab} {w}x{h}: window bottom missing\n{dump}"));
            }
            if !(rows.iter().any(|r| r.contains('╭') && r.contains('╮'))) {
                bad.push(format!("tab {tab} {w}x{h}: window top missing\n{dump}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

#[test]
fn a_plan_question_and_a_big_diff_keep_their_answers_reachable_when_small() {
    use nexo_core::permission::{Question, QuestionOption};
    use nexo_tui::modal::{Ask, Modal};
    let mut bad = Vec::new();
    for (w, h) in [(30u16, 9u16), (40, 12), (60, 16)] {
        // a plan with many lines and three ways to answer it
        let mut a = app();
        let plan = (0..40).map(|i| format!("{i}. step number {i} of the plan")).collect::<Vec<_>>().join("\n");
        a.modal = Some(Modal::Question(Box::new(Ask::new(Question {
            id: "q".into(),
            header: "Plan".into(),
            question: "Go ahead with this plan?".into(),
            options: ["Yes", "No", "Edit"]
                .iter()
                .map(|l| QuestionOption { label: (*l).into(), description: String::new() })
                .collect(),
            multi: false,
            plan: Some(plan),
        }))));
        let rows = draw(&mut a, w, h);
        for l in ["Yes", "No", "Edit"] {
            if !rows.iter().any(|r| r.contains(l)) {
                bad.push(format!("plan {w}x{h}: option {l} out of view\n{}", rows.join("\n")));
            }
        }
        // a diff of 60 changed lines: the three keys stay
        let mut a = app();
        let (old, new): (String, String) =
            ((0..60).map(|i| format!("old {i}\n")).collect(), (0..60).map(|i| format!("new {i}\n")).collect());
        a.handle_agent(AgentEvent::PermissionAsk {
            request: PermissionRequest {
                id: "p2".into(),
                tool: "edit".into(),
                kind: ToolKind::Write,
                summary: "edit src/lib.rs".into(),
                detail: ToolDisplay::Diff { path: "src/lib.rs".into(), old, new, start_line: 1 },
                suggested_rules: vec!["edit(src/*)".into()],
            },
        });
        let rows = draw(&mut a, w, h);
        let row = rows.iter().find(|r| r.contains("[y]"));
        match row {
            Some(r) if r.contains("[a]") && r.contains("[n]") => {}
            _ => bad.push(format!("diff {w}x{h}: buttons missing\n{}", rows.join("\n"))),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n\n"));
}

#[test]
fn the_keys_on_offer_match_what_the_permission_window_shows() {
    for (w, h) in [(30u16, 9u16), (40, 12), (60, 16), (80, 24)] {
        let mut a = app();
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.handle_agent(shell_ask("cargo test --workspace --all-features"));
        let rows = draw(&mut a, w, h);
        let dump = rows.join("\n");
        let offers = rows.iter().any(|r| r.contains("1-9 rule")); // the keys line sits under the draft now
        let shows = rows.iter().any(|r| r.contains("1  bash(cargo test*)"));
        assert_eq!(offers, shows, "{w}x{h}: the keys line and the window disagree\n{dump}");
        let header = rows.iter().any(|r| r.contains("Allow always with a rule"));
        assert!(!header || shows, "{w}x{h}: a header with no rule under it\n{dump}");
        if h >= 12 {
            assert!(shows, "{w}x{h}: no rule shown\n{dump}");
        }
        assert!(rows.iter().any(|r| r.contains("[y]")), "{w}x{h}: no buttons\n{dump}");
    }
}

#[test]
fn the_provider_list_comes_before_the_intro_on_a_short_screen() {
    use nexo_tui::modal::setup::Setup;
    use nexo_tui::modal::Modal;
    let models: Vec<(String, String)> = (1..=6).map(|i| ("mock".to_string(), format!("m{i}"))).collect();
    for (w, h) in [(40u16, 12u16), (80, 24)] {
        let mut a = app();
        let s = Setup::new(vec![], models.clone(), "mock/m1");
        a.modal = Some(Modal::Setup(Box::new(s)));
        let rows = draw(&mut a, w, h);
        let dump = rows.join("\n");
        let intro = rows.iter().position(|r| r.contains("Switch to a model"));
        let first = rows.iter().position(|r| r.contains("switch"));
        assert!(first.is_some(), "{w}x{h}: no model listed\n{dump}");
        if h <= 12 {
            assert!(intro.is_none(), "{w}x{h}: the intro takes rows from the list\n{dump}");
            let n = rows.iter().filter(|r| r.contains("switch") || r.contains("in use")).count();
            assert!(n >= 3, "{w}x{h}: only {n} models in view\n{dump}");
        } else {
            assert!(intro.is_some() && intro < first, "{w}x{h}: intro then list\n{dump}");
        }
    }
}

#[test]
fn the_review_keeps_the_chosen_field_in_view_under_a_long_error() {
    use nexo_tui::modal::setup::{Field, Setup, Step};
    use nexo_tui::modal::Modal;
    for (w, h) in [(40u16, 12u16), (30, 9), (80, 24)] {
        for sel in 0..4usize {
            let mut a = app();
            let mut s = Setup::new(vec![], vec![("deepseek".into(), "deepseek-chat".into())], "deepseek/deepseek-chat");
            s.step = Step::Review;
            s.name = "deepseek".into();
            s.model = "deepseek-chat".into();
            s.fields = ["context", "max output", "input price", "output price"]
                .iter()
                .map(|l| Field { label: l, value: "128000".into(), source: "from the server".into() })
                .collect();
            s.selected = sel;
            s.error = Some("could not save: the config file is read-only and nexo could not write to it".into());
            a.modal = Some(Modal::Setup(Box::new(s)));
            let rows = draw(&mut a, w, h);
            let label = ["context", "max output", "input price", "output price"][sel];
            assert!(
                rows.iter().any(|r| r.contains(label)),
                "{w}x{h} field {sel} ({label}) out of view\n{}",
                rows.join("\n")
            );
        }
    }
}

#[test]
fn a_long_answer_being_written_wraps_and_its_end_stays_in_view() {
    use nexo_core::permission::{Question, QuestionOption};
    use nexo_tui::modal::{Ask, Modal};
    let mut a = app();
    let mut ask = Ask::new(Question {
        id: "q".into(),
        header: "Approach".into(),
        question: "Which way?".into(),
        options: vec![QuestionOption { label: "one".into(), description: String::new() }],
        multi: false,
        plan: None,
    });
    ask.selected = 1;
    ask.typing = true;
    ask.other = format!("{} the very end", "words of a long answer ".repeat(12));
    a.modal = Some(Modal::Question(Box::new(ask)));
    let rows = draw(&mut a, 60, 24);
    let dump = rows.join("\n");
    assert!(dump.contains("the very end▏"), "the end of the answer is not in view\n{dump}");
    assert!(rows.iter().filter(|r| r.contains("words of a long")).count() >= 2, "the answer did not wrap\n{dump}");
}

#[test]
fn the_persons_own_message_sits_on_a_band_that_reaches_the_edge() {
    use nexo_tui::render::blocks::Block;
    let mut a = app();
    a.push(Block::User { text: "hello there".into(), at: None });
    let mut t = Terminal::new(TestBackend::new(60, 16)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let buf = t.backend().buffer().clone();
    let row = (0..16)
        .find(|&y| (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().contains("hello there"));
    let y = row.expect("message not drawn");
    let start = (0..60).find(|&x| buf[(x, y)].symbol() == "h").unwrap();
    let message_bg = buf[(start, y)].bg;
    let page_bg = buf[(0, 15)].bg;
    assert_ne!(message_bg, page_bg, "the message has no band");
    assert_eq!(buf[(start + 30, y)].bg, message_bg, "the band stops at the text");
}

#[test]
fn esc_on_the_session_picker_leaves_nothing_of_it_on_screen() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};
    for animations in [false, true] {
        let mut a = app();
        a.opts.animations = animations;
        a.opts.sessions = vec![("20261108-044309-7902".into(), "hello there".into())];
        // well after the banner's intro, so nothing else keeps the clock running
        let t0 = Instant::now() + Duration::from_secs(10);
        a.fx.clock = Some(t0);
        let mut t = Terminal::new(TestBackend::new(90, 28)).unwrap();
        draw_frame(&mut t, &mut a).unwrap();
        for c in "/resume".chars() {
            nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        draw_frame(&mut t, &mut a).unwrap();
        a.fx.clock = Some(t0 + Duration::from_millis(500));
        a.tick_frame(false);
        draw_frame(&mut t, &mut a).unwrap();
        let open = rows_of(t.backend().buffer()).join("\n");
        assert!(open.contains("hello there"), "{open}");
        assert_eq!(a.load(), nexo_tui::frame::Load::Idle, "the pop-in is over");
        nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.modal.is_none());
        draw_frame(&mut t, &mut a).unwrap();
        if animations {
            // the leaving dialog is drawn for a moment: the loop must keep drawing, not rest for seconds
            assert_eq!(a.load(), nexo_tui::frame::Load::Motion);
            a.fx.clock = Some(t0 + Duration::from_millis(500 + nexo_tui::anim::POP_MS + 30));
            a.tick_frame(false);
            draw_frame(&mut t, &mut a).unwrap();
        }
        let shut = rows_of(t.backend().buffer()).join("\n");
        assert!(!shut.contains("Sessions"), "animations={animations}\n{shut}");
    }
}
