//! The context gauge: what the title bar and the status bar measure.

use super::*;

impl App {
    /// The context gauge: (tokens the last call sent, the window), when known.
    /// The last call's real input against the model's window; None when no
    /// window is known (a share of the history is not a limit and would alarm
    /// for nothing: the bar then shows `ctx_tokens` instead).
    pub fn gauge(&self) -> Option<(u64, u64)> {
        let den = self.limits.as_ref().and_then(|l| l.context_window).map(u64::from).filter(|d| *d > 0)?;
        let num = if self.last_call_input > 0 { self.last_call_input } else { self.context.as_ref()?.view_tokens };
        Some((num.min(den), den))
    }

    /// What the title bar's gauge draws: tokens in use, the window, and whether the
    /// window is a guess (the model never said it; most take at least this much).
    pub fn gauge_view(&self) -> (u64, u64, bool) {
        match self.gauge() {
            Some((n, d)) => (n, d, false),
            None => (self.ctx_tokens().unwrap_or(0).min(GUESS_WINDOW), GUESS_WINDOW, true),
        }
    }

    /// The last call by where its tokens stood (cached prefix, sent fresh) and what the view leaves
    /// out; None until a call reported its input.
    pub fn gauge_split(&self) -> Option<crate::render::gauge::Split> {
        if self.last_call_input == 0 {
            return None;
        }
        let cached = self.last_call_cached.min(self.last_call_input);
        let out = self.context.as_ref().map_or(0, |c| c.history_tokens.saturating_sub(c.view_tokens));
        Some(crate::render::gauge::Split { cached, fresh: self.last_call_input - cached, out })
    }

    /// Tokens the next call sends, for the bar when no window is known.
    pub fn ctx_tokens(&self) -> Option<u64> {
        if self.last_call_input > 0 {
            return Some(self.last_call_input);
        }
        self.context.as_ref().map(|c| c.view_tokens)
    }
}
