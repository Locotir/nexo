//! The context gauge: what the title bar and the status bar measure.

use super::*;

/// The full-price part of a call, in tokens, below which a cache miss is not worth a word in the status bar.
pub(super) const CACHE_MISS_FRESH: u64 = 8_000;

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

    /// Tokens the last call paid full price for when the cache let it down: an earlier call of the session was
    /// served from the cache, this one got under half of what the previous call sent from it (and of its own input), and
    /// at least 8k went at full price.
    /// None otherwise, so a provider that never reports caching never raises it.
    pub fn cache_miss(&self) -> Option<u64> {
        if self.last_call_input == 0 {
            return None;
        }
        self.cache_miss_fresh
    }

    /// Tokens the next call sends, for the bar when no window is known.
    pub fn ctx_tokens(&self) -> Option<u64> {
        if self.last_call_input > 0 {
            return Some(self.last_call_input);
        }
        self.context.as_ref().map(|c| c.view_tokens)
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use nexo_core::agent::AgentEvent;

    fn app() -> App {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        App::new(crate::TuiOptions { provider: "one".into(), model: "model".into(), ..Default::default() }, tx)
    }

    fn usage(a: &mut App, input: u64, cached: u64) {
        let call = Usage { input, cache_read: cached, ..Default::default() };
        let mut session = a.session_usage;
        session += call;
        a.handle_agent(AgentEvent::Usage { turn: call, call, session, cost_usd: 0.0 });
    }

    #[test]
    fn side_usage_does_not_supply_cache_evidence() {
        let mut a = app();
        usage(&mut a, 40_000, 0);
        let side = Usage { cache_read: 30_000, ..Default::default() };
        let mut session = a.session_usage;
        session += side;
        a.handle_agent(AgentEvent::Usage { turn: side, call: Usage::default(), session, cost_usd: 0.0 });
        usage(&mut a, 35_000, 0);
        assert_eq!(a.cache_miss(), None);
        usage(&mut a, 2_000, 40_000);
        usage(&mut a, 30_000, 1_000);
        assert_eq!(a.cache_miss(), Some(30_000));
    }

    #[test]
    fn a_past_cache_hit_does_not_keep_causing_miss_warnings() {
        let mut a = app();
        usage(&mut a, 0, 100_000);
        usage(&mut a, 100_000, 0);
        assert_eq!(a.cache_miss(), Some(100_000));
        usage(&mut a, 100_000, 0);
        assert_eq!(a.cache_miss(), None);
        usage(&mut a, 0, 100_000);
        usage(&mut a, 100_000, 0);
        assert_eq!(a.cache_miss(), Some(100_000));
    }

    #[test]
    fn changing_provider_or_model_starts_a_fresh_segment() {
        let mut a = app();
        usage(&mut a, 2_000, 40_000);
        a.handle_agent(AgentEvent::ModelChanged { provider: "two".into(), model: "model".into() });
        usage(&mut a, 30_000, 0);
        usage(&mut a, 30_000, 0);
        assert_eq!(a.cache_miss(), None);
        usage(&mut a, 2_000, 40_000);
        a.handle_agent(AgentEvent::ModelChanged { provider: "two".into(), model: "other".into() });
        usage(&mut a, 30_000, 0);
        assert_eq!(a.cache_miss(), None);
        usage(&mut a, 2_000, 40_000);
        usage(&mut a, 30_000, 1_000);
        assert_eq!(a.cache_miss(), Some(30_000));
    }
}
