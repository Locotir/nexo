//! One decision for each request: what stays in the view and what leaves.

use std::sync::Arc;

use serde_json::Value;

use super::Engine;

impl Engine {
    pub fn decide(&mut self) {
        self.own.clock += 1;
        if self.t0.is_none() {
            self.t0 = Some((self.now, self.tr.steps.len()));
        }
        self.index_calls();
        if self.dump.is_some() {
            self.dump_rows();
        }
        let epoch = self.epoch_due();
        // while a refusal stands, the view is held to the working-set bound at every request, not only each
        // `check` tokens: the bound comes down when a large output stops being the latest one, with no growth
        let over = !epoch && self.refused && self.tries_landings() && self.past_working_set();
        if (epoch || over) && self.tries_landings() && !self.must_land() {
            // the epoch is made on a copy and this engine goes on with no epoch: the copy takes its place only
            // when the view it really made pays for the cache it breaks (`landing_pays`), or when the view with
            // no epoch goes out past the working-set bound (`past_working_set`); else nothing of the try stays,
            // and the next one waits for `check` tokens of growth or for the bound
            // (the context of this step is set before the copy, so neither of the two has to copy the featurizer
            // to set it)
            self.context_at(self.tr.msgs.len());
            let mut tried = self.clone();
            tried.decide_as(true);
            self.decide_as(false);
            let pays = epoch && self.landing_pays(&mut tried);
            if pays || self.past_working_set() {
                *self = tried;
                self.refused = false;
                if !pays {
                    self.bump("landings_bound", 1);
                }
            } else if epoch {
                self.looked = self.sizes_now().iter().sum::<i64>() + self.head;
                self.refused = true;
                self.bump("landings_refused", 1);
            }
        } else {
            self.decide_as(epoch);
            self.refused &= !epoch;
        }
        if self.tries_landings() {
            let vis = self.visible();
            self.sent_fp = Arc::new(self.prints(&vis));
        }
    }

    /// The decision of one request, as an epoch (the view is rewritten now) or not.
    fn decide_as(&mut self, epoch: bool) {
        // by epochs (`Params.epoch`) the view is rewritten only at an epoch: between two, it only grows at the end
        let by_epoch = self.epochs();
        // what the view sent before: a message of it that a step below changes without weighing the cache
        // (it changes what the view sends, not what is weighed) still counts in `miss`
        let sent = self.sent_vis.clone();
        let was_sent = |mi: usize| sent.contains(&mi);
        let mut quiet: Vec<(&'static str, usize)> = Vec::new();
        // whether the other topics leave the view: decided when the topic changes, and again at each request of that
        // turn while it is declined (what a cut costs grows with the turn, so a declined split mostly stays so)
        let nt = self.goal.turns.len();
        let switch = !by_epoch && nt > 1 && self.goal.turns[nt - 1].thread != self.goal.turns[nt - 2].thread;
        let fresh = switch && self.split_turn != nt - 1;
        if fresh {
            self.split = self.split_now();
            self.split_turn = nt - 1;
            self.hidden_recalls = 0;
        } else if switch && !self.split && self.unsplit_turn != nt - 1 {
            self.split = self.split_now();
        }
        // the agent asked twice for what the split left out: it needs the other topics, the view is whole again
        // until the next change of topic
        if self.split && self.hidden_recalls >= 2 {
            self.split = false;
            self.hidden_recalls = 0;
            self.unsplit_turn = nt - 1;
            self.bump("unsplit", 1);
            self.frozen.clear();
        }
        if let Some(mi) = self.unnudged.take().filter(|&mi| was_sent(mi)) {
            quiet.push(("miss_note", mi));
        }
        // the output that went whole as the latest message (`Params.fresh`) points up from now on
        if let Some(&mi) = sent.last().filter(|&&mi| self.p.fresh != 0 && self.dups.contains_key(&mi)) {
            if mi + 1 < self.tr.msgs.len() && self.dups_on(mi).iter().any(Option::is_some) {
                if self.swap_pays(mi) {
                    quiet.push(("miss_fresh", mi));
                } else {
                    // it keeps going as it was sent until a request rewrites the view from it or earlier
                    self.fresh_wait.insert(mi);
                }
            }
        }
        // the provider's cache holds the last view of each topic for a while (`Params.pockets`): where it
        // stops in the view as it stands, which cuts past it do not move
        let (warm, warm_all, cold) =
            if self.p.pockets != 0 { self.warm_now() } else { (usize::MAX, usize::MAX, false) };
        self.warm = warm;
        // the oldest message closing steps changed in this request: the cache misses from there on
        let mut closed: Option<usize> = None;
        // a history with no request from the person yet has nothing to close, idle or bring back; by epochs what a
        // request needs of the steps that left comes back at once, but nothing more leaves before the epoch
        if self.p.close != 0 && nt > 0 {
            let (now, mut turned) = (self.tr.steps.len(), false);
            while self.shut_done + 1 < nt {
                if let Some(unused) = self.close_unused(self.shut_done, nt - 1) {
                    for (i, s) in self.steps.iter_mut().enumerate().filter(|(_, s)| !s.out) {
                        s.idle = if unused.contains(&i) { Some(s.idle.unwrap_or(now)) } else { None };
                    }
                }
                self.shut_done += 1;
                turned = true;
            }
            if epoch && self.p.live != 0 {
                if let Some(mi) = self.live_drop(now) {
                    quiet.push(("miss_live", mi));
                }
            }
            // what the latest request asks is weighed against every step, those of the request just closed too
            let need = self.asks(nt - 1);
            let named_back = self.p.named != 0 && self.named_keep(turned);
            if (turned && self.recall(&need)) || named_back {
                closed = self.shut_lines();
            }
            let idle: Vec<usize> = (0..self.steps.len()).filter(|&i| self.steps[i].idle.is_some()).collect();
            if epoch && !idle.is_empty() && self.rent_close(idle) {
                if let Some(mi) = self.shut_lines() {
                    closed = Some(closed.map_or(mi, |c| c.min(mi)));
                }
            }
        }
        let mut switched = false;
        if !by_epoch && nt > 1 && self.goal.turns[nt - 1].thread != self.goal.turns[nt - 2].thread {
            let start = self.goal.turns[nt - 1].start;
            if !self.frozen.contains_key(&start) && self.unsplit_turn != nt - 1 {
                let text = self.cards_text();
                if !text.is_empty() {
                    self.frozen.insert(start, text);
                    self.bump("cards", 1);
                } else if fresh && !self.split {
                    self.bump("split_kept", 1);
                }
                self.bump("switches", 1);
                if fresh && self.goal.members[self.goal.turns[nt - 1].thread].len() > 1 {
                    self.bump("returns", 1);
                }
                switched = true;
                // the view sends the messages of another topic: the cache misses from the first that differs
                // from what it holds (with pockets, the last view of that topic, if it lives)
                if self.p.pockets != 0 {
                    if warm != usize::MAX {
                        quiet.push(("miss_switch", warm));
                    }
                } else {
                    let vis = self.visible();
                    // (the message before it joins it when they are of the same role)
                    if let Some(p) = (0..sent.len()).find(|&p| vis.get(p) != Some(&sent[p])) {
                        let role = |m: usize| self.tr.msgs[m].get("role").and_then(Value::as_str);
                        let joined = p > 0
                            && (vis.get(p).is_some_and(|&m| role(m) == role(vis[p - 1]))
                                || role(sent[p]) == role(sent[p - 1]));
                        quiet.push((
                            "miss_switch",
                            if joined { vis[p - 1] } else { vis.get(p).map_or(sent[p], |&m| m.min(sent[p])) },
                        ));
                    }
                }
            }
        }
        // the same topic, but its last view outlived the cache: what a cache that kept it would hold is read
        // again
        if warm < warm_all {
            self.bump("pocket_cold", 1);
            if !switched {
                quiet.push(("miss_cold", warm));
            }
        }
        let t = self.tr.msgs.len();
        if t == 0 {
            self.warm = usize::MAX;
            return;
        }
        let k = self.tr.steps.len();
        let mut sizes = self.sizes_now();
        let total: i64 = sizes.iter().sum();
        // where the cache stops, the view is read again whatever is cut: what is worth cutting is weighed now
        let due = epoch
            && (warm != usize::MAX
                || total - self.checked >= self.p.check
                || k as i64 - self.checked_at as i64 >= self.p.every);
        if self.p.target > 0 && epoch {
            self.refresh_sticky();
        }
        let held = if self.p.hold != 0 && self.p.eoq != 0 && epoch { self.held(&sizes) } else { 0 };
        self.guess = true;
        let mut fire = 0;
        if self.p.eoq != 0 && epoch {
            // what no cut takes: the first message and the protected tail
            let tail = (t as i64 - self.p.protect).clamp(1, t as i64) as usize;
            let base = (sizes[0] + sizes[tail..].iter().sum::<i64>() - held).max(0);
            let stuck;
            (self.cap, self.floor, stuck) = self.eoq_cap(k, base);
            fire = self.cap;
            self.fire = fire;
            if stuck {
                self.bump("cap_stuck", 1);
            }
            // the priced cap runs with the working window's target: it aims at the same cut, lower
            let g = self.raw_tokens / k.max(1) as i64;
            if self.p.target > 0 && !stuck && self.cap > self.p.target && base + g <= self.p.target {
                let cut_to = self.floor * self.cap as f64;
                // below the cap the budget set, only what the history shows dead leaves (copies, out of date
                // reads); what the model only guesses unlikely to be needed again stays
                self.guess = total - held > self.cap;
                // it fires where rewriting pays (`fire_point`; with a fixed `cap`, at twice the target or the priced
                // cap if lower) and lands on the target: one batch
                fire = if self.p.cap > 0 { fire.min(2 * self.p.target) } else { self.window_span().0 };
                self.fire = fire;
                self.cap = self.p.target;
                self.floor = (cut_to / self.cap as f64).min(0.8);
            }
        }
        let mut first: Option<usize> = None;
        let capped = epoch && self.cap != 0 && total - held > fire.max(self.cap);
        // a view the cache holds no more of than its anchor is read again whole: it is cut down to what the
        // cap leaves now, for free, rather than later at the price of breaking the cache (`Params.pockets` 2)
        let squeeze = epoch
            && self.p.pockets >= 2
            && !capped
            && cold
            && self.cap != 0
            && (total - held) as f64 > self.floor * self.cap as f64;
        let mut over = capped || squeeze;
        if over && self.p.cut_copies {
            let target = self.floor * self.cap as f64 + held as f64;
            if target != 0.0 {
                first = self.need(t, &sizes, target, true);
                sizes = self.sizes_now();
            }
            let left = sizes.iter().sum::<i64>();
            over = left - held > 2 * self.cap || (squeeze && left as f64 > target);
        }
        if over {
            if let Some(e) = self.evict(t, k, &sizes, true) {
                first = Some(first.map_or(e, |f| f.min(e)));
            }
            sizes = self.sizes_now();
            let target = self.floor * self.cap as f64 + held as f64;
            if self.p.cut_cover && target != 0.0 && sizes.iter().sum::<i64>() as f64 > target {
                if let Some(mi) = self.cover(t, &sizes, target) {
                    first = Some(first.map_or(mi, |f| f.min(mi)));
                    sizes = self.sizes_now();
                }
            }
            if self.p.cut_need && target != 0.0 && sizes.iter().sum::<i64>() as f64 > target {
                if let Some(mi) = self.need(t, &sizes, target, false) {
                    first = Some(first.map_or(mi, |f| f.min(mi)));
                    sizes = self.sizes_now();
                }
            }
            // `leave` writes the anchor again: the first message changes with it, and with it every topic's
            // pocket, so a squeeze leaves only when no pocket that lives holds the anchor as it is
            if target != 0.0
                && sizes.iter().sum::<i64>() as f64 > target
                && (!squeeze || warm == 0)
                && self.leave(&sizes, target)
            {
                first = Some(0);
            }
            // (what `leave` wrote is counted as it is now, the anchor it writes again included)
            sizes = self.sizes_now();
            if squeeze {
                self.bump("pocket_squeeze", first.is_some() as i64);
                self.miss("miss_squeeze", first, &sizes, t);
            } else {
                self.bump("cap_rewrites", first.is_some() as i64);
                self.miss("miss_cap", first, &sizes, t);
            }
        } else if due {
            if let Some(e) = self.evict(t, k, &sizes, false) {
                self.miss("miss_due", Some(e), &sizes, t);
                first = Some(first.map_or(e, |f| f.min(e)));
            }
        }
        // the working window: past `cap` the oldest results the agent is not working from leave, price ignored
        if let Some(mi) = epoch.then(|| self.window_cut()).flatten() {
            first = Some(first.map_or(mi, |f| f.min(mi)));
            sizes = self.sizes_now();
            self.miss("miss_cap", Some(mi), &sizes, t);
        }
        // what settles and what fits is weighed as a view rewritten from message 1 on (`first`); the cache
        // misses from the oldest result settled, and from the anchor that `leave` writes again
        let mut missed = first;
        if self.fit() {
            first.get_or_insert(1);
            self.miss("miss_fit", Some(0), &sizes, t);
            missed = Some(0);
        }
        if first.is_some() || due {
            self.checked = self.sizes_now().iter().sum();
            self.checked_at = k;
        }
        self.miss("miss_close", closed, &sizes, t);
        if let Some(c) = closed {
            first = Some(first.map_or(c, |f| f.min(c)));
            missed = Some(missed.map_or(c, |f| f.min(c)));
        }
        if let Some(mi) = self.nudge(t) {
            self.miss("miss_nudge", Some(mi), &sizes, t);
            first = Some(first.map_or(mi, |f| f.min(mi)));
            missed = Some(missed.map_or(mi, |f| f.min(mi)));
        }
        // a view changed from message 1 on is read again nearly whole: the anchor can change with it, as it can
        // when no pocket that lives holds the anchor as it is (`Params.pockets`)
        if epoch && self.refresh_rules(first.is_some_and(|f| f <= 1) || warm == 0) {
            self.miss("miss_anchor", Some(0), &sizes, t);
            missed = Some(0);
        }
        for (k, mi) in quiet {
            self.miss(k, Some(mi), &sizes, t);
            missed = Some(missed.map_or(mi, |f| f.min(mi)));
        }
        // a pointer swap held back goes out once the view is rewritten from its message or an earlier one
        if let Some(m) = missed {
            self.fresh_wait.retain(|&w| w < m);
        }
        self.miss("miss", missed, &sizes, t);
        if by_epoch && epoch {
            // the epoch lands the view only when it changed a message already sent; one that changed nothing
            // leaves `landed`, and the next look waits for `check` tokens of growth (`epoch_due`)
            let now = self.sizes_now().iter().sum::<i64>() + self.head;
            self.looked = now;
            if self.landed == 0 || missed.is_some_and(was_sent) {
                self.landed = now;
            }
            self.bump("epochs", 1);
        }
        self.sent_vis = self.visible();
        if self.p.pockets != 0 {
            self.keep_pocket();
        }
        self.warm = usize::MAX;
    }

    /// Counts under `k` the tokens of the view from message `from` on: what the provider's cache misses
    /// when that message is the first one the view changed (`None`: no change, nothing counted).
    fn miss(&mut self, k: &'static str, from: Option<usize>, sizes: &[i64], t: usize) {
        if let Some(f) = from {
            let n = sizes.len().min(t);
            let tokens = sizes[f.min(n)..n].iter().sum();
            self.bump(k, tokens);
        }
    }
}
