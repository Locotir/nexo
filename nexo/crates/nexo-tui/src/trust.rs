//! The trust question asked before the interface and the agent start.

use std::time::Duration;

use crate::terminal_io::{Hangup, Keys};
use crate::{event, frame, modal, terminal, App, TuiOptions};

/// What the trust question shows (see [`ask_trust`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustAsk {
    pub path: String,
    /// One short line on what a yes enables.
    pub enables: String,
    /// Set apart when trusting the folder means more than a project (the home folder).
    pub warning: Option<String>,
}

/// Asks "Do you trust the files in this folder?" on a screen of its own, before the interface and
/// the agent start, so a yes can still shape the configuration they start from. `opts` gives the
/// theme, the mouse and the motion. True only for a yes; No, Esc, Ctrl+C, a closed window or a
/// terminal that stops answering are a no.
pub async fn ask_trust(ask: TrustAsk, opts: TuiOptions) -> anyhow::Result<bool> {
    let mut term = terminal::Term::enter(opts.mouse)?;
    let (tx, _commands) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(opts, tx);
    app.modal = Some(modal::Modal::Trust(modal::Trust::new(ask.path, ask.enables, ask.warning)));
    let mut events = Keys::spawn();
    let mut hangup = Hangup::new();
    let answer = loop {
        frame::draw_synced(&mut term.terminal, &mut app)?;
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) => event::handle(&mut app, e),
                Some(Err(_)) | None => break false,
            },
            _ = hangup.wait() => break false,
            // the dialog pops open over a few frames
            _ = tokio::time::sleep(Duration::from_millis(33)) => {}
        }
        if let Some(yes) = app.trust_answer {
            break yes;
        }
        // closed by a click outside it: not a yes
        if app.modal.is_none() {
            break false;
        }
    };
    drop(term);
    Ok(answer)
}
