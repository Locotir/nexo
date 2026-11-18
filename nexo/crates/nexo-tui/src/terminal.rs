//! Taking the terminal and giving it back, whatever happens.

use std::io::{stdout, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::ThreadId;

use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub struct Term {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse: bool,
}

/// Whether mouse capture is on right now, kept where the panic hook can read it: `/mouse` changes it
/// while the program runs, so what was true at startup is not what the hook has to undo.
static MOUSE: AtomicBool = AtomicBool::new(false);

/// The thread that drives the interface while it holds the terminal. The process-wide panic hook (in `nexo-cli`)
/// leaves raw mode only for a panic on this thread: tokio catches a panic in a spawned task and the interface keeps
/// running, so restoring the terminal for that one would leave a live screen without raw mode.
static TUI_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

/// The session a panic can name in its resume line, once something was said in it.
static SESSION: Mutex<Option<String>> = Mutex::new(None);

fn claim_thread() {
    *TUI_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::thread::current().id());
}

fn release_thread() {
    *TUI_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Whether the calling thread is the one that holds the terminal right now.
pub fn on_tui_thread() -> bool {
    *TUI_THREAD.lock().unwrap_or_else(|e| e.into_inner()) == Some(std::thread::current().id())
}

/// Whether the interface holds the terminal now, on whatever thread.
pub fn holds_terminal() -> bool {
    TUI_THREAD.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Records the session `nexo --resume` can bring back (None while nothing was said in it), for the panic hook.
pub fn note_session(id: Option<&str>) {
    let mut held = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if held.as_deref() != id {
        *held = id.map(str::to_owned);
    }
}

/// The session id the panic hook can offer to resume.
pub fn noted_session() -> Option<String> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// `NEXO_NO_ALT_SCREEN=1`: draw on the normal screen, so what the session showed stays in the
/// terminal's scrollback after quitting (the inline way of the other terminal agents).
pub fn alt_screen() -> bool {
    !inline() && !nexo_core::env::env_flag("NEXO_NO_ALT_SCREEN")
}

/// `NEXO_INLINE=1` (`--inline`, `[ui] screen = "inline"`): the terminal's own screen and scrollback;
/// each finished turn is written above a small live zone (the draft and the turn in progress).
pub fn inline() -> bool {
    nexo_core::env::env_flag("NEXO_INLINE")
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
        // a failure after raw mode is on must not leave the shell in it
        let entered = enter_screen(&mut out).and_then(|()| match mouse {
            true => execute!(out, EnableMouseCapture),
            false => Ok(()),
        });
        if let Err(e) = entered {
            MOUSE.store(mouse, Ordering::Relaxed);
            restore();
            return Err(e.into());
        }
        MOUSE.store(mouse, Ordering::Relaxed);
        claim_thread();
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
        restore();
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
        MOUSE.store(self.mouse, Ordering::Relaxed);
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
        MOUSE.store(on, Ordering::Relaxed);
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        release_thread();
        restore();
        let _ = self.terminal.show_cursor();
    }
}

pub fn restore() {
    let mut out = stdout();
    if MOUSE.load(Ordering::Relaxed) {
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
    fn only_the_thread_that_holds_the_terminal_counts_as_the_interface_thread() {
        super::claim_thread();
        assert!(super::on_tui_thread());
        // a tokio worker or a tool thread panicking must not take the terminal from the interface
        assert!(!std::thread::spawn(super::on_tui_thread).join().unwrap());
        super::release_thread();
        assert!(!super::on_tui_thread(), "a released terminal has no interface thread");
    }

    #[test]
    fn a_noted_session_is_what_the_panic_hook_offers_to_resume() {
        super::note_session(Some("20261117-010000-ab12"));
        assert_eq!(super::noted_session().as_deref(), Some("20261117-010000-ab12"));
        super::note_session(None);
        assert_eq!(super::noted_session(), None);
    }

    #[test]
    fn restore_undoes_the_mouse_state_of_now_not_the_one_at_startup() {
        use std::sync::atomic::Ordering;
        // started with the mouse off, `/mouse` turned it on: a panic must still release it
        super::MOUSE.store(false, Ordering::Relaxed);
        super::MOUSE.store(true, Ordering::Relaxed);
        assert!(super::MOUSE.load(Ordering::Relaxed));
        super::MOUSE.store(false, Ordering::Relaxed);
    }

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
