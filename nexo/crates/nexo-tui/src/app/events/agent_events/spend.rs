use super::*;

impl App {
    pub(super) fn on_usage(&mut self, turn: Usage, call: Usage, session: Usage, cost_usd: f64) {
        // one event per model call: its input is what that call sent (the attempt that
        // answered), while the turn counts all it spent
        if call.total_input() > 0 {
            self.last_call_input = call.total_input();
            self.last_call_cached = call.cache_read;
            self.call_inputs.push(call.total_input());
            // the reply joins the history now, but the curator reports its count only before the next call:
            // the bar's session figure grows by what the reply cost, so it moves with the turn
            if let Some(c) = &mut self.context {
                c.history_tokens += call.output;
            }
        }
        self.turn_tally.steps += 1;
        self.turn_usage += turn;
        self.session_usage = session;
        self.cost = cost_usd;
    }

    // one parameter per field of the event, so the arm body moves as it was
    #[allow(clippy::too_many_arguments)]
    pub(super) fn on_context(
        &mut self,
        view_tokens: u64,
        history_tokens: u64,
        history_bytes: u64,
        view_bytes: u64,
        messages_in_view: usize,
        messages_total: usize,
        policy: String,
        note: Option<String>,
        label: String,
        detail: Option<String>,
    ) {
        // the view shrank while the history did not: the curator let something go
        if let Some(prev) = &self.context {
            let dropped = prev.view_tokens.saturating_sub(view_tokens);
            if dropped >= crate::motion::CUT_LEAST && history_tokens >= prev.history_tokens {
                self.fx.cut = Some((dropped, self.now()));
                // the map of this call arrives right after: what it was until now is what its cells condense from
                self.fx.cut_from = self.map.iter().map(|c| c.state).collect();
                self.fx.cut_shown = false;
            }
        }
        self.context_wait = false;
        self.context = Some(ContextInfo {
            view_tokens,
            history_tokens,
            history_bytes,
            view_bytes,
            messages_in_view,
            messages_total,
            policy: crate::render::wrap::one_line(&policy),
            note: note.map(|n| crate::render::wrap::one_line(&n)),
            label: crate::render::wrap::one_line(&label),
            detail,
        });
    }

    pub(super) fn on_context_map(
        &mut self,
        cells: Vec<ContextCell>,
        zones: Vec<ContextZone>,
        calls: Vec<(String, CallMark)>,
        call_tokens: Vec<(String, u64)>,
    ) {
        let marks: std::collections::HashMap<String, CallMark> = calls.into_iter().collect();
        let call_tokens: std::collections::HashMap<String, u64> = call_tokens.into_iter().collect();
        // a card whose word changed is drawn again
        for i in 0..self.blocks.len() {
            if let Block::Tool { id, .. } = &self.blocks[i] {
                if marks.get(id) != self.marks.get(id) || call_tokens.get(id) != self.call_tokens.get(id) {
                    self.cache[i] = None;
                    self.dirty = true;
                }
            }
        }
        self.marks = marks;
        self.call_tokens = call_tokens;
        self.map_changed =
            cells.iter().enumerate().map(|(i, c)| self.map.get(i).is_none_or(|old| old.state != c.state)).collect();
        self.map = cells;
        self.map_zones = zones;
        self.map_at = Some(self.now());
    }
}
