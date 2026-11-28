//! The card page of `/panel`: `c` opens it, the tree comes back, finished rows go dim, the list scrolls.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_supervise::{AgentRow, SourceKind, State};
use nexo_tui::agents_view::AgentsView;
use nexo_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use ratatui::Terminal;

const NOW: i64 = 1_800_000_000;

fn row(key: &str, state: State) -> AgentRow {
    AgentRow {
        key: format!("nexo:{key}"),
        name: key.into(),
        source: SourceKind::Nexo,
        program: "nexo".into(),
        state,
        since: NOW - 90,
        last_step: Some(format!("{key} reading the tree")),
        last_conclusion: None,
        idle_secs: Some(1),
        cost_usd: None,
        model: Some("grok".into()),
        cwd: None,
        permission_mode: None,
        parent: None,
        last_action: None,
        paused: false,
        ribbon: vec![],
        spark: vec![],
        todo: None,
        tests: None,
        subs: vec![],
        unread: 0,
        held: 0,
        retry_secs: None,
        ctx_pct: None,
        goal: None,
        lane: None,
    }
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn draw(v: &mut AgentsView, w: u16, h: u16) -> Buffer {
    let theme = Theme::by_name("dark").unwrap();
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| v.draw(f, f.area(), &theme, Instant::now(), false)).unwrap();
    t.backend().buffer().clone()
}

fn screen(b: &Buffer) -> String {
    (0..b.area.height)
        .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn golden(name: &str, w: u16, h: u16, text: &str, wrong: &mut Vec<String>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/captures").join(format!("{name}_{w}x{h}.txt"));
    if std::env::var("NEXO_UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, text).unwrap();
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(kept) if kept == text => {}
        Ok(_) => wrong.push(format!("{name} {w}x{h}: differs from {}", path.display())),
        Err(_) => wrong.push(format!("{name} {w}x{h}: no capture, run with NEXO_UPDATE_GOLDEN=1")),
    }
}

fn three() -> AgentsView {
    AgentsView::with_rows(
        vec![row("alpha", State::Working), row("bravo", State::Done), row("charlie", State::Working)],
        NOW,
        true,
    )
}

#[test]
fn c_opens_the_cards_and_opens_the_tree_again() {
    let mut v = three();
    let tree = screen(&draw(&mut v, 80, 24));
    assert!(tree.contains("NEXO SESSIONS"), "{tree}");
    v.key(key(KeyCode::Char('c')));
    let cards = screen(&draw(&mut v, 80, 24));
    assert!(cards.contains("panel · cards"), "{cards}");
    assert!(cards.contains("alpha"), "{cards}");
    assert!(!cards.contains("NEXO SESSIONS"), "the tree stays off the card page:\n{cards}");
    v.key(key(KeyCode::Char('c')));
    let back = screen(&draw(&mut v, 80, 24));
    assert!(back.contains("NEXO SESSIONS"), "{back}");
    assert!(!back.contains("panel · cards"), "{back}");
}

#[test]
fn one_column_at_80_and_two_at_140_match_their_captures() {
    let mut wrong = Vec::new();
    for (w, h) in [(80u16, 24u16), (140, 40)] {
        let mut v = three();
        v.key(key(KeyCode::Char('c')));
        let b = draw(&mut v, w, h);
        let text = screen(&b) + "\n";
        // the frame, not the step text: the step repeats the name, the corner does not
        let frames: Vec<(u16, u16)> = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .filter(|(x, y)| at(&b, *x, *y, "┌ alpha"))
            .collect();
        assert_eq!(frames.len(), 1, "{w}x{h} drew the alpha card {} times\n{text}", frames.len());
        let bravo_x = find(&b, "bravo").map(|p| p.0);
        if w < 100 {
            assert!(bravo_x.is_some_and(|x| x < 40), "one column stacks the cards\n{text}");
        } else {
            assert!(bravo_x.is_some_and(|x| x > 60), "two columns put the next card beside the first\n{text}");
        }
        golden("panel-cards", w, h, &text, &mut wrong);
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn at(b: &Buffer, x: u16, y: u16, word: &str) -> bool {
    word.chars().enumerate().all(|(i, c)| b[(x + i as u16, y)].symbol() == c.to_string())
}

fn find(b: &Buffer, word: &str) -> Option<(u16, u16)> {
    (0..b.area.height)
        .find_map(|y| (0..b.area.width.saturating_sub(word.len() as u16)).find(|x| at(b, *x, y, word)).map(|x| (x, y)))
}

#[test]
fn a_finished_row_is_dim_and_a_working_row_keeps_its_newest_step_bold() {
    let mut v = three();
    v.key(key(KeyCode::Char('c')));
    let b = draw(&mut v, 80, 24);
    let (ax, ay) = find(&b, "alpha reading").expect("the newest step is on the card");
    assert!(b[(ax, ay)].modifier.contains(Modifier::BOLD), "the newest step is bold");
    let theme = Theme::by_name("dark").unwrap();
    let (bx, by) = find(&b, "bravo").expect("bravo");
    assert_eq!(b[(bx, by)].style().fg, theme.dim().fg, "a finished card is dim");
    let (sx, sy) = find(&b, "bravo reading").expect("bravo's step");
    assert!(!b[(sx, sy)].modifier.contains(Modifier::BOLD), "a finished step is not bold");
}

#[test]
fn the_card_list_scrolls_and_stops_at_the_end() {
    let rows: Vec<AgentRow> = (0..12).map(|i| row(&format!("job{i:02}"), State::Working)).collect();
    let mut v = AgentsView::with_rows(rows, NOW, true);
    v.key(key(KeyCode::Char('c')));
    let first = screen(&draw(&mut v, 80, 24));
    assert!(first.contains("job00") && !first.contains("job08"), "{first}");
    v.key(key(KeyCode::Char('j')));
    let next = screen(&draw(&mut v, 80, 24));
    assert!(!next.contains("job00") && next.contains("job02"), "{next}");
    for _ in 0..30 {
        v.key(key(KeyCode::Char('j')));
    }
    let end = screen(&draw(&mut v, 80, 24));
    assert!(end.contains("job11") && !end.contains("job00"), "{end}");
    v.key(key(KeyCode::Char('k')));
    let back = screen(&draw(&mut v, 80, 24));
    assert!(back.contains("job09"), "{back}");
}

#[test]
fn drawing_thirty_cards_stays_under_a_frame() {
    let rows: Vec<AgentRow> = (0..30)
        .map(|i| {
            let mut r = row(&format!("agent{i:02}"), if i % 5 == 0 { State::Done } else { State::Working });
            r.model = Some("xai/grok-4".into());
            r
        })
        .collect();
    let mut v = AgentsView::with_rows(rows, NOW, true);
    v.key(key(KeyCode::Char('c')));
    let _ = draw(&mut v, 140, 40);
    let frames = 20u32;
    let start = Instant::now();
    for _ in 0..frames {
        let _ = draw(&mut v, 140, 40);
    }
    let per = start.elapsed() / frames;
    let ms = per.as_secs_f64() * 1000.0;
    eprintln!("panel cards: {ms:.3} ms per frame to draw 30 agents at 140x40");
    assert!(per < Duration::from_millis(50), "{ms:.3} ms a frame for 30 cards");
}
