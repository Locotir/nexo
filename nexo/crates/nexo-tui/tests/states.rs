//! States told plainly: flash levels, dialogs that do not fit, the mode's words, the keys line.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::Level;
use nexo_core::permission::PermissionMode;
use nexo_tui::app::{status_life, STATUS_ERR_MS, STATUS_MS};
use nexo_tui::input::VimMode;
use nexo_tui::modal::Modal;
use nexo_tui::toast::Kind;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        theme: "dark".into(),
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn draw(a: &mut App, w: u16, h: u16) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t.backend().buffer().clone()
}

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

/// The colour of the first cell of `needle` on the keys row (above the title bar).
fn fg_of(buf: &Buffer, needle: &str) -> Color {
    let rows = rows_of(buf);
    let y = rows.len() - 2;
    let col = rows[y].find(needle).map(|b| rows[y][..b].chars().count()).expect("text on the bottom row");
    buf[(col as u16, y as u16)].fg
}

#[test]
fn an_error_flash_is_red_and_stays_longer_than_a_confirmation() {
    let t0 = Instant::now();
    let mut a = app();
    a.fx.clock = Some(t0);
    a.flash_level(Level::Error, "theme not saved");
    let buf = draw(&mut a, 100, 24);
    assert_eq!(fg_of(&buf, "theme not saved"), a.theme.error);
    a.flash_level(Level::Warn, "nothing to undo");
    assert_eq!(fg_of(&draw(&mut a, 100, 24), "nothing to undo"), a.theme.warn);
    a.flash("copied");
    assert_eq!(fg_of(&draw(&mut a, 100, 24), "copied"), a.theme.bar_fg);
    assert!(status_life(Level::Error) == STATUS_ERR_MS && STATUS_ERR_MS > STATUS_MS);
    // past a confirmation's life the error is still up; past its own it goes
    a.flash_level(Level::Error, "theme not saved");
    a.fx.clock = Some(ms(t0, STATUS_MS + 500));
    a.tick();
    assert!(a.status.is_some());
    a.fx.clock = Some(ms(t0, STATUS_ERR_MS + 500));
    a.tick();
    assert!(a.status.is_none());
    a.flash("copied");
    a.fx.clock = Some(ms(t0, STATUS_ERR_MS + STATUS_MS + 600));
    a.tick();
    assert!(a.status.is_none());
}

#[test]
fn a_long_status_gets_the_room_the_bar_has_and_the_whole_text_goes_to_a_toast() {
    let mut a = app();
    let msg = "x".repeat(70) + " the end";
    a.flash_level(Level::Error, &msg);
    let rows = rows_of(&draw(&mut a, 160, 24));
    assert!(rows[22].contains("the end"), "wide bar shows it whole: {}", rows[22]);
    assert!(a.toasts.iter().next().is_none(), "nothing clipped, no toast");
    let rows = rows_of(&draw(&mut a, 60, 24));
    assert!(!rows[22].contains("the end"));
    let t = a.toasts.iter().next().expect("clipped text reaches a toast");
    assert!(t.kind == Kind::Error && t.text == msg);
    // once, not every frame
    draw(&mut a, 60, 24);
    assert_eq!(a.toasts.iter().count(), 1);
}

#[test]
fn a_dialog_that_does_not_fit_says_so() {
    let mut a = app();
    a.modal = Some(Modal::Info(Box::new(nexo_tui::overlay::Info::new(0).still(true))));
    let rows = rows_of(&draw(&mut a, 26, 12));
    assert!(rows.iter().any(|r| r.contains("make the window wider")), "{rows:?}");
}

#[test]
fn the_title_bar_names_the_mode_like_the_rule_over_the_draft() {
    let mut a = app();
    for (mode, word) in [
        (PermissionMode::AcceptEdits, "✎ accept edits"),
        (PermissionMode::ReadOnly, "◉ read-only"),
        (PermissionMode::Auto, "▲ YOLO"),
        (PermissionMode::Plan, "◇ plan"),
    ] {
        a.permission_mode = mode;
        let rows = rows_of(&draw(&mut a, 120, 24));
        assert!(rows[23].contains(word), "title {word}: {}", rows[23]);
        assert!(rows[..22].iter().any(|r| r.starts_with(&format!("─ {word}"))), "rule {word}: {rows:?}");
    }
    // ask is told on the rule too, dim
    a.permission_mode = PermissionMode::Ask;
    let rows = rows_of(&draw(&mut a, 120, 24));
    assert!(rows.iter().skip(1).any(|r| r.starts_with("─ ask ─")), "{rows:?}");
}

#[test]
fn the_vim_normal_mark_wears_no_permission_colour() {
    let mut a = app();
    a.permission_mode = PermissionMode::AcceptEdits;
    a.input.vim = Some(VimMode::Normal);
    let buf = draw(&mut a, 80, 24);
    let rows = rows_of(&buf);
    let y = rows.iter().position(|r| r.starts_with("▮")).expect("normal mark") as u16;
    let c = buf[(0, y)].fg;
    for t in [a.theme.warn, a.theme.error, a.theme.info, a.theme.accent2, a.theme.accent] {
        assert_ne!(c, t);
    }
}

#[test]
fn the_keys_line_keeps_the_send_key_and_help_when_it_is_narrow() {
    let mut a = app();
    let rows = rows_of(&draw(&mut a, 120, 24));
    assert!(rows[22].contains("Enter send") && rows[22].contains("? help"));
    for w in [45u16, 40, 36] {
        let rows = rows_of(&draw(&mut a, w, 24));
        let r = &rows[22];
        assert!(r.contains("Enter send"), "{w}: {r}");
        assert!(r.contains("? help"), "{w}: {r}");
        // every hint is whole: no half word at the end
        assert!(!r.contains("Shift+↑ b") || r.contains("Shift+↑ blocks"), "{w}: {r}");
    }
}

#[test]
fn toast_icons_are_single_width_and_differ_in_shape() {
    let mut a = app();
    a.toast(Kind::Info, "one");
    a.toast(Kind::Success, "two");
    a.toast(Kind::Error, "three");
    let rows = rows_of(&draw(&mut a, 80, 24));
    assert!(rows[0].trim_end().ends_with("· one"), "{}", rows[0]);
    assert!(rows[1].trim_end().ends_with("✓ two"), "{}", rows[1]);
    assert!(rows[2].trim_end().ends_with("✗ three"), "{}", rows[2]);
}
