use super::*;

impl App {
    pub(super) fn on_usage(&mut self, turn: Usage, call: Usage, session: Usage, cost_usd: f64) {
        // one event per model call: its input is what that call sent (the attempt that
        // answered), while the turn counts all it spent
        if call.total_input() > 0 {
            let input = call.total_input();
            let cached = call.cache_read.min(input);
            let previous = self.cache_baseline.as_ref().filter(|(provider, model, _, _)| {
                self.last_call_input > 0 && *provider == self.provider && *model == self.model
            });
            let previously_cached = previous.is_some_and(|(_, _, _, cached)| *cached);
            self.cache_miss_fresh = previous.and_then(|(_, _, before, _)| {
                let before = (*before).min(input);
                let fresh = input - cached;
                (previously_cached
                    && cached < before - cached.min(before)
                    && cached < input - cached
                    && fresh >= crate::app::gauge::CACHE_MISS_FRESH)
                    .then_some(fresh)
            });
            self.cache_baseline = Some((self.provider.clone(), self.model.clone(), input, cached > 0));
            self.last_call_input = call.total_input();
            self.last_call_cached = call.cache_read;
            self.call_inputs.push(call.total_input());
            // the reply joins the history now, but the curator reports its count only before the next call:
            // the bar's session figure grows by what the reply cost, so it moves with the turn
            if let Some(c) = &mut self.context {
                c.history_tokens += call.output;
            }
            // only the main agent's calls report usage here (sub-agents, the reader, compaction and the like carry
            // `Usage::default()` as `call`), so the step count agrees with the title bar's `call_inputs`
            self.turn_tally.steps += 1;
        }
        self.turn_usage += turn;
        self.session_usage = session;
        // Usage is a session total, including replay and side calls from other providers.
        // Only GroupCost carries enough provenance to charge a particular group.
        self.cost = cost_usd;
    }

    pub(super) fn on_group_cost(&mut self, call_ids: &[String], usd: f64, plan: bool) {
        if call_ids.is_empty() || !usd.is_finite() || usd <= 0.0 {
            return;
        }
        let mut group = None;
        for id in call_ids {
            let Some(index) = self.tool_index(id) else { return };
            let Some(current) = self.group_of(index) else { return };
            if group.is_some_and(|g| g != current) {
                return; // a request spanning multiple groups has no single attributable group
            }
            group = Some(current);
        }
        let Some(group) = group else { return };
        let look = &mut self.looks[group];
        look.spent_plan =
            if look.spent == 0.0 { Some(plan) } else { look.spent_plan.filter(|previous| *previous == plan) };
        look.spent += usd;
        self.invalidate(group);
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
        crate::trace::totals("context");
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
        self.words.defer(false);
        self.relation_quotes = None;
        self.quote_heat = None;
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
        let heads = self.topic_heads_of(&cells, &zones);
        for i in heads.keys().chain(self.topic_heads.keys()) {
            if heads.get(i) != self.topic_heads.get(i) && *i < self.cache.len() {
                self.cache[*i] = None;
                self.dirty = true;
            }
        }
        self.topic_heads = heads;
        self.map = cells;
        self.map_zones = zones;
        self.map_at = Some(self.now());
    }

    /// The messages of the person that go back to a topic, with the words of their line: the
    /// requests of the map, matched to the person's messages from the end (the transcript may hold fewer of
    /// the oldest). A way back says how much of the earlier topic the model carries again. None while the
    /// curator sees a single topic.
    fn topic_heads_of(&self, cells: &[ContextCell], zones: &[ContextZone]) -> std::collections::HashMap<usize, String> {
        let mut out = std::collections::HashMap::new();
        if zones.len() < 3 {
            return out;
        }
        let asks: Vec<usize> =
            (0..cells.len()).filter(|&c| matches!(cells[c].kind, nexo_core::CellKind::Request)).collect();
        let users: Vec<usize> =
            (0..self.blocks.len()).filter(|&i| matches!(self.blocks[i], Block::User { .. })).collect();
        let n = asks.len().min(users.len());
        let mut prev = None;
        let mut seen = std::collections::HashSet::new();
        for (&bi, &ci) in users[users.len() - n..].iter().zip(&asks[asks.len() - n..]) {
            let z = cells[ci].zone;
            if z == 0 {
                continue;
            }
            if prev != Some(z) && seen.contains(&z) && zones.get(z as usize).is_some() {
                out.insert(bi, format!("↺ earlier topic{}", carried(&cells[..ci], z)));
            }
            seen.insert(z);
            prev = Some(z);
        }
        out
    }
}

/// What the model carries of topic `zone` from the messages before a way back to it:
/// ` · 2 msgs · 5 calls · 5 outputs`, the kinds it has none of left out.
fn carried(before: &[ContextCell], zone: u32) -> String {
    use nexo_core::{CellKind, CellState};
    let kept = |kind: CellKind| {
        before
            .iter()
            .filter(|c| c.zone == zone && c.kind == kind && !matches!(c.state, CellState::Dropped | CellState::Unknown))
            .count()
    };
    let parts = [(kept(CellKind::Request), "msg"), (kept(CellKind::Call), "call"), (kept(CellKind::Output), "output")];
    let mut s = String::new();
    for (n, what) in parts {
        if n > 0 {
            s.push_str(&format!(" · {n} {what}{}", if n == 1 { "" } else { "s" }));
        }
    }
    if s.is_empty() {
        s.push_str(" · nothing kept");
    }
    s
}
