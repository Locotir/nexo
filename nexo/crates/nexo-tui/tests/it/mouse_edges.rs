//! A mouse event at row 0, column 0 or far outside the drawn area never panics.

use std::path::PathBuf;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

#[test]
fn mouse_at_the_edges_and_outside_the_screen_is_harmless() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "p".into(),
        model: "m".into(),
        theme: "dark".into(),
        animations: false,
        ..TuiOptions::default()
    };
    let mut app = App::new(opts, tx);
    app.unlock();
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    draw_frame(&mut t, &mut app).unwrap();
    let kinds = [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Moved,
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollDown,
    ];
    let spots =
        [(0, 0), (0, 23), (79, 0), (79, 23), (80, 24), (200, 200), (u16::MAX, u16::MAX), (u16::MAX, 0), (0, u16::MAX)];
    for kind in kinds {
        for (column, row) in spots {
            nexo_tui::event::mouse(&mut app, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
            draw_frame(&mut t, &mut app).unwrap();
        }
    }
    assert!(app.status_hit(u16::MAX).is_none());
    assert!(!nexo_tui::modal::contains(ratatui::layout::Rect::new(u16::MAX - 1, u16::MAX - 1, 5, 5), 0, 0));
    assert!(nexo_tui::modal::contains(
        ratatui::layout::Rect::new(u16::MAX - 1, u16::MAX - 1, 5, 5),
        u16::MAX - 1,
        u16::MAX - 1
    ));
}
