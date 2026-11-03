//! Taking the terminal and giving it back, whatever happens.

use std::io::{stdout, Stdout, Write};

use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub struct Term {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse: bool,
}

/// `NEXO_NO_ALT_SCREEN=1`: draw on the normal screen, so what the session showed stays in the
/// terminal's scrollback after quitting (the inline way of the other terminal agents).
pub fn alt_screen() -> bool {
    !inline() && !std::env::var("NEXO_NO_ALT_SCREEN").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// `NEXO_INLINE=1` (`--inline`, `[ui] screen = "inline"`): the terminal's own screen and scrollback;
/// each finished turn is written above a small live zone (the draft and the turn in progress).
pub fn inline() -> bool {
    std::env::var("NEXO_INLINE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Rows of the live zone of the inline mode for a terminal of `rows` rows: half of it, at least 8
/// and at most 24, never more than the terminal has.
pub fn live_rows(rows: u16) -> u16 {
    (rows / 2).clamp(8, 24).min(rows.max(1))
}

fn enter_screen(out: &mut Stdout) -> std::io::Result<()> {
    if alt_screen() {
        execute!(out, EnterAlternateScreen)?;
    }
    execute!(out, EnableBracketedPaste)
}

impl Term {
    pub fn enter(mouse: bool) -> anyhow::Result<Term> {
        enable_raw_mode()?;
        let mut out = stdout();
        enter_screen(&mut out)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore(mouse);
            hook(info);
        }));
        let backend = CrosstermBackend::new(out);
        let terminal = if inline() {
            let rows = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
            let viewport = ratatui::Viewport::Inline(live_rows(rows));
            Terminal::with_options(backend, ratatui::TerminalOptions { viewport })?
        } else {
            Terminal::new(backend)?
        };
        Ok(Term { terminal, mouse })
    }
}

impl Term {
    /// Leaves the alternate screen and raw mode so another program (an editor) can use the terminal.
    pub fn suspend(&mut self) {
        restore(self.mouse);
        let _ = self.terminal.show_cursor();
    }

    /// Takes the terminal back after `suspend`.
    pub fn resume(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut out = stdout();
        enter_screen(&mut out)?;
        if self.mouse {
            execute!(out, EnableMouseCapture)?;
        }
        self.terminal.clear()?;
        Ok(())
    }

    /// Mouse capture on or off at runtime.
    pub fn set_mouse(&mut self, on: bool) {
        if on == self.mouse {
            return;
        }
        let mut out = stdout();
        if on {
            let _ = execute!(out, EnableMouseCapture);
        } else {
            let _ = execute!(out, DisableMouseCapture);
        }
        self.mouse = on;
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        restore(self.mouse);
        let _ = self.terminal.show_cursor();
    }
}

pub fn restore(mouse: bool) {
    let mut out = stdout();
    if mouse {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, DisableBracketedPaste);
    if alt_screen() {
        let _ = execute!(out, LeaveAlternateScreen);
    } else {
        // the session was written to the normal screen: the shell prompt goes under it
        let _ = out.flush();
    }
    let _ = disable_raw_mode();
}

#[cfg(test)]
mod tests {
    #[test]
    fn nexo_no_alt_screen_keeps_the_session_in_the_scrollback() {
        std::env::remove_var("NEXO_NO_ALT_SCREEN");
        assert!(super::alt_screen());
        for off in ["1", "yes"] {
            std::env::set_var("NEXO_NO_ALT_SCREEN", off);
            assert!(!super::alt_screen(), "{off}");
        }
        std::env::set_var("NEXO_NO_ALT_SCREEN", "0");
        assert!(super::alt_screen());
        std::env::remove_var("NEXO_NO_ALT_SCREEN");
    }
}
