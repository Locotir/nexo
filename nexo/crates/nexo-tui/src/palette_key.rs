//! The commands window on its own key (Ctrl+Space, or Alt+P where the terminal
//! keeps Ctrl+Space): it opens over any draft. The draft waits aside, whole,
//! and comes back when the command has run, on Esc, or when the `/` is erased.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::palette::PaletteKind;

/// Whether `k` is the key that opens the commands window.
pub fn is_palette_key(k: &KeyEvent) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    matches!((k.code, ctrl, alt), (KeyCode::Char(' '), true, false) | (KeyCode::Char('p' | 'P'), false, true))
}

impl App {
    /// The key: opens the commands window, or closes it and gives the draft back.
    pub fn palette_key(&mut self) {
        self.dirty = true;
        if self.kept_draft.is_some() {
            self.give_draft_back();
            return;
        }
        // a `/` window typed by hand: the key closes it, like Esc
        if self.palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Commands) {
            self.input.clear();
            self.palette = None;
            return;
        }
        if !self.input.is_empty() {
            self.kept_draft = Some(self.input.take_draft());
        } else {
            self.input.clear();
        }
        self.input.editor.insert_char('/');
        self.refresh_palette();
    }

    /// The set-aside draft, back in the box as it was; the window closes.
    pub fn give_draft_back(&mut self) {
        if let Some(d) = self.kept_draft.take() {
            self.input.put_draft(d);
            self.palette = None;
            self.dirty = true;
        }
    }

    /// After every event: once the command has gone (or its `/` was erased), the draft returns.
    pub fn draft_check(&mut self) {
        if self.kept_draft.is_some() && self.palette.is_none() && self.input.is_empty() {
            self.give_draft_back();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use nexo_core::agent::AgentCommand;
    use tokio::sync::mpsc;

    use crate::app::App;
    use crate::event::handle;
    use crate::palette::PaletteKind;
    use crate::TuiOptions;

    fn app() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let opts = TuiOptions {
            cwd: PathBuf::from("."),
            provider: "mock".into(),
            model: "m".into(),
            models: vec![("mock".into(), "m".into()), ("mock".into(), "n".into())],
            providers: vec![],
            theme: "dark".into(),
            mouse: true,
            show_reasoning: true,
            timestamps: false,
            fold_after: 3,
            group_tools: true,
            complete_words: true,
            suggest: false,
            notify_after: 15,
            animations: false,
            vim: false,
            session_id: None,
            sessions: vec![],
            skills: vec![],
            keys: Default::default(),
            status_line: vec![],
            status_command: String::new(),
        };
        let mut a = App::new(opts, tx);
        a.root = false;
        (a, rx)
    }

    fn key(a: &mut App, code: KeyCode, m: KeyModifiers) {
        handle(a, Event::Key(KeyEvent::new(code, m)));
    }

    fn typed(a: &mut App, s: &str) {
        for c in s.chars() {
            key(a, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    fn open(a: &mut App) {
        key(a, KeyCode::Char(' '), KeyModifiers::CONTROL);
    }

    #[test]
    fn the_key_opens_the_commands_over_a_draft_and_esc_gives_it_back() {
        let (mut a, _rx) = app();
        typed(&mut a, "fix the login bug");
        key(&mut a, KeyCode::Left, KeyModifiers::NONE);
        let cursor = a.input.cursor_offset();
        open(&mut a);
        assert_eq!(a.input.text(), "/");
        assert!(a.palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Commands));
        typed(&mut a, "mo");
        assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("model"));
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(a.palette.is_none());
        assert_eq!(a.input.text(), "fix the login bug");
        assert_eq!(a.input.cursor_offset(), cursor, "the cursor stays where it was");
    }

    #[test]
    fn a_command_run_from_the_key_leaves_the_draft_in_place() {
        let (mut a, _rx) = app();
        typed(&mut a, "half a thought");
        open(&mut a);
        typed(&mut a, "help");
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        assert!(a.kept_draft.is_none());
        assert_eq!(a.input.text(), "half a thought");
    }

    #[test]
    fn erasing_the_slash_or_pressing_the_key_again_gives_the_draft_back() {
        let (mut a, _rx) = app();
        typed(&mut a, "draft");
        key(&mut a, KeyCode::Char('p'), KeyModifiers::ALT);
        assert_eq!(a.input.text(), "/");
        key(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(a.input.text(), "draft");
        open(&mut a);
        typed(&mut a, "mod");
        open(&mut a);
        assert_eq!(a.input.text(), "draft");
        assert!(a.palette.is_none());
    }

    #[test]
    fn the_key_closes_a_slash_window_typed_by_hand() {
        let (mut a, _rx) = app();
        typed(&mut a, "/mo");
        assert!(a.palette.is_some());
        open(&mut a);
        assert!(a.palette.is_none());
        assert_eq!(a.input.text(), "");
    }

    #[test]
    fn on_an_empty_box_the_key_is_the_same_as_typing_a_slash() {
        let (mut a, _rx) = app();
        open(&mut a);
        assert_eq!(a.input.text(), "/");
        assert!(a.kept_draft.is_none());
        assert!(a.palette.is_some());
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert!(a.palette.is_none());
    }

    #[test]
    fn a_long_paste_in_the_draft_survives_the_trip() {
        let (mut a, mut rx) = app();
        let long = (0..10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        a.input.insert_paste(&long);
        let shown = a.input.text();
        open(&mut a);
        key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(a.input.text(), shown);
        key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
        let mut sent = String::new();
        while let Ok(c) = rx.try_recv() {
            sent.push_str(&format!("{c:?}"));
        }
        assert!(sent.contains("line 9"), "the folded paste is still sent whole: {sent}");
    }
}
