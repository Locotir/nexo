//! The working window: a view aims at `target` tokens (the priced cap) and is rewritten down to it when keeping
//! the larger view costs more than the rewrite (`fire_point`; a fixed `cap` is only for a spec that sets one),
//! and never passes the ceiling: 95 % of the model's own window, or the spec's `hard` when the window is not
//! known. What the agent is working from (the sticky set) stays below the ceiling; no model and no directive
//! decide it, only rules over the history.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hindsight::UKind;
use crate::ledger::ERROR_MARKS;
use crate::tools::Kind;

use super::files::file_key;
use super::fixes::TEST_FAILED;
use super::{Engine, RECALL_TOOL};

/// A result this small saves next to nothing when it leaves: its stub is as long.
const LEAVE_LEAST: i64 = 40;
/// The tokens of failing runs (a command whose output shows a failure) held below `cap`, newest first: what the
/// agent fixed is what it reports, and the lines of the failure are what it quotes.
const FAILURE_BUDGET: i64 = 2000;
/// The share of the model's window a view may take before the sticky results leave too.
const CEILING: f64 = 0.95;
/// A ceiling cut lands on this share of the ceiling, so one rewrite buys a stretch of requests.
const CEILING_LAND: f64 = 0.9;
/// Context length past which a longer view is worse for the model, not only dearer: the quality loss of each
/// token past it is counted per call as this share of an uncached token (a heuristic, not a measured price).
const LONG_KNEE: i64 = 100_000;
const LONG_LOSS: f64 = 0.05;
/// The batch a view may pass what the agent works from by before a refused rewrite lands whatever it sends again
/// (`Engine::past_working_set`): the 8k the window lets a session that only reads keep past its working set.
const BATCH: i64 = 8_000;

/// The sticky set as of the last request, by unit.
#[derive(Clone, Default)]
pub(super) struct Sticky {
    /// Results that never leave by the window: the latest `file now` of each path, the last `check:`, and
    /// what the latest step returned.
    pub keep: FxHashSet<usize>,
    /// Results that leave only past `hard`: the latest whole read of each file the agent changed or the person
    /// named.
    pub reads: FxHashSet<usize>,
    /// Tokens of everything sticky: the request, the agent's own messages and the results in `keep`.
    pub tokens: i64,
    /// Result units a window cut took parts of, oldest first (for measurement).
    pub cut: Vec<usize>,
    /// Whether the output of each command result shows a failure, once read.
    failing: FxHashMap<usize, bool>,
}

impl Engine {
    /// The sticky set of the view as it stands (see [`Sticky`]). Rules over the history only: the request
    /// and the agent's own words are never results, so they never leave by the window.
    pub(super) fn refresh_sticky(&mut self) {
        let vis = self.visible();
        self.index_units();
        let t = self.tr.msgs.len();
        let shown: FxHashSet<usize> = vis.iter().copied().collect();
        // newest first, the units of the messages the view sends (not every unit of the history)
        let mut by_age: Vec<usize> = vis.to_vec();
        if !by_age.windows(2).all(|w| w[0] < w[1]) {
            by_age.sort_unstable();
            by_age.dedup();
        }
        let units: Vec<usize> = by_age.iter().rev().flat_map(|&mi| self.unit_range(mi).rev()).collect();
        let (mut keep, mut reads) = (FxHashSet::default(), FxHashSet::default());
        let (mut now_seen, mut read_seen) = (FxHashSet::<String>::default(), FxHashSet::<String>::default());
        let mut check_seen = false;
        let mut tokens = 0;
        let mut failing = std::mem::take(&mut self.stk.failing);
        let mut room = FAILURE_BUDGET;
        for i in units {
            let u = &self.tr.units[i];
            match u.kind {
                UKind::User | UKind::Text | UKind::Call => tokens += u.tokens,
                UKind::Thinking => {}
                UKind::Result => {
                    let info = self.fz.info.get(&i);
                    let (kind, path, error) =
                        info.map_or((Kind::Other, None, false), |x| (x.kind, x.path.clone(), x.error));
                    // what the latest step returned, and what a `recall` brought back until the step after
                    // it is answered: the agent asked for it because it works from it
                    let mut sticky = u.msg + 1 >= t || (u.tool == RECALL_TOOL && u.msg + 3 >= t);
                    if !error && matches!(kind, Kind::Write | Kind::Exec) && (!check_seen || kind == Kind::Write) {
                        let text = self.tr.unit_text(i);
                        if !check_seen && text.contains("\ncheck: ") {
                            check_seen = true;
                            sticky = true;
                        }
                        if let (Kind::Write, Some(p)) = (kind, &path) {
                            if text.contains("file now") && now_seen.insert(p.clone()) {
                                sticky = true;
                            }
                        }
                    }
                    if !error {
                        let files: Vec<String> = match (kind, &path) {
                            (Kind::Read, Some(p)) => vec![p.clone()],
                            _ => self.shell_reads.get(&i).cloned().unwrap_or_default(),
                        };
                        // the whole body of a file the agent changed or the person named is what it works from
                        let mut latest = false;
                        for f in files {
                            let key = file_key(&f);
                            let work = self.written.contains_key(&key) || self.pins.contains_key(&key);
                            latest |= read_seen.insert(f) && work;
                        }
                        if latest && !sticky {
                            reads.insert(i);
                        }
                    }
                    // the output of a failing run is held below `cap` while the budget lasts
                    if !sticky && kind == Kind::Exec && u.tokens <= room {
                        let shows = *failing.entry(i).or_insert_with(|| {
                            let text = self.tr.unit_text(i);
                            error || ERROR_MARKS.is_match(&text) || TEST_FAILED.is_match(&text)
                        });
                        if shows {
                            reads.insert(i);
                            room -= u.tokens;
                        }
                    }
                    if sticky {
                        keep.insert(i);
                        tokens += u.tokens;
                    }
                }
            }
        }
        // the reads a request names are what it asks about
        for u in self.named_results() {
            if !keep.contains(&u) && shown.contains(&self.tr.units[u].msg) {
                reads.insert(u);
            }
        }
        let cut = std::mem::take(&mut self.stk.cut);
        self.stk = Sticky { keep, reads, tokens, cut, failing };
    }

    /// The tokens a view may take, head included: 95 % of what the model's window leaves for the request (the host
    /// gives it, `set_budget`), or the spec's `hard` while the window is not known (0: no ceiling).
    pub(super) fn ceiling(&self) -> i64 {
        if self.budget > 0 {
            ((self.budget + self.head.max(0)) as f64 * CEILING) as i64
        } else {
            self.p.hard
        }
    }

    /// Where a cut made because the view passed the ceiling lands, head included (`CEILING_LAND`).
    pub(super) fn ceiling_land(&self) -> i64 {
        (self.ceiling() as f64 * CEILING_LAND) as i64
    }

    /// The size past which a rewrite down to `land` tokens pays, head included: keeping the extra tokens costs
    /// `ratio` of an uncached token each (the provider's cache price when the host gave it, `Curator::prices`) on
    /// every call still to come, plus the quality a view past `LONG_KNEE` loses; the rewrite sends its new
    /// prefix (nearly the whole view it lands on: the oldest results go first) without the cache. It pays once the
    /// first is `margin` times the second, the margin the priced cut (`rent`) asks too. The calls to come are
    /// `ahead` of the calls so far (at least 4), as for `horizon`. `i64::MAX` when it never pays (a free cache). With no
    /// estimate of the calls to come (`ahead=0`) it is twice the target, the fixed rule the cost replaces.
    pub(super) fn fire_point(&self, land: i64) -> i64 {
        if self.p.ahead <= 0.0 {
            return 2 * land;
        }
        let (r, l) = (self.p.ratio, land as f64);
        let calls = self.calls_ahead(self.tr.steps.len());
        if r <= 0.0 {
            return i64::MAX;
        }
        let rewrite = self.p.margin * l * (1.0 - r).max(0.01);
        let (knee, loss) = (LONG_KNEE as f64, LONG_LOSS);
        let at = l + rewrite / (r * calls);
        let fire = if at <= knee.max(l) {
            at
        } else if l >= knee {
            l + rewrite / ((r + loss) * calls)
        } else {
            (rewrite / calls + r * l + loss * knee) / (r + loss)
        };
        fire.min(i64::MAX as f64 / 2.0) as i64
    }

    /// Where the working window's rewrite starts (head included) and where it lands: the cost of keeping against
    /// rewriting (`fire_point`), or the fixed `cap` a spec sets (with a `target`, the cap or twice the target,
    /// whichever comes first); never past the ceiling.
    pub(super) fn window_span(&self) -> (i64, i64) {
        let (start, land) = match (self.p.target, self.p.cap) {
            (t, c) if t > 0 && c > 0 => (c.min(2 * t), t),
            (t, _) if t > 0 => (self.fire_point(t), t),
            (_, c) => (c, c),
        };
        match self.ceiling() {
            0 => (start, land),
            c => (start.min(c), land),
        }
    }

    /// Whether the view runs by epochs (`Params.epoch`, with a `target`).
    pub(super) fn epochs(&self) -> bool {
        self.p.epoch != 0 && self.p.target > 0
    }

    /// Whether this request is an epoch: the view is rewritten now. Always when the view does not run by epochs.
    /// Otherwise when it has passed the ceiling or the model's window, or when it has grown past where the last
    /// epoch that rewrote a message already sent left it by the room the cost rule gives (`fire_point` less the size it lands on): what keeping
    /// that growth costs for the calls still to come is more than sending the new prefix once. With a fixed `cap`, as soon as
    /// the view is past it.
    pub(super) fn epoch_due(&mut self) -> bool {
        self.resent_from = usize::MAX;
        self.turn_only = false;
        if !self.epochs() {
            return true;
        }
        let turned = self.turn_due();
        let view = self.sizes_now().iter().sum::<i64>();
        let total = view + self.head;
        let (start, land) = self.window_span();
        // the first view of an engine (also after the host rewrote the history) is laid down whole
        if self.landed == 0 || (self.ceiling() > 0 && total > self.ceiling()) || (self.budget > 0 && view > self.budget)
        {
            return true;
        }
        // a fixed `cap` is the person's own trigger: past it the cut runs at every request, as it did before epochs
        // (an epoch that changed nothing sent leaves `landed` where it was: the next look waits for the view to
        // have grown by `check` since that one, not for the whole span again)
        // (with `kept` at 4 the room past the last landing is not asked: the epoch is tried each `check` tokens of
        // growth and lands by what it really does, `landing_pays`)
        let grown = (self.p.kept >= 4 && self.tries_landings()) || total - self.landed > start - land;
        let sized = total > start && (self.p.cap > 0 || (grown && total - self.looked >= self.p.check));
        self.turn_only = turned && !sized;
        sized || turned
    }

    /// Whether the person's latest request makes an epoch due (`Params.close`): it has made two calls of its
    /// own, none of them nor its text drew on the steps of the requests before it that went idle (a distinctive
    /// word of their outputs the person's requests do not hold; a step it names stays too), and those still
    /// hold `check` tokens of the view. They leave by the rent rule (`rent_close`), which runs only at an
    /// epoch; waiting for the view to grow past the room the cost rule gives keeps what an earlier task read
    /// for tens of calls of a new one that never uses it (a session that went from reading a project's docs
    /// to configuring a model carried 25k tokens of those docs for 19 calls). A request that goes on from them
    /// ("run it again", then an edit of the file read before) draws on them, and they wait as before. A
    /// session in which the person has already gone back to a topic left earlier is one that goes back: the
    /// idle steps of the requests before wait for the cost rule (on the fixtures that return to old topics
    /// the rule alone forgot 17 more words). Once per request. It only weighs: what leaves is the epoch's to
    /// decide, and the epoch lands only when it pays (`Engine::decide`).
    fn turn_due(&mut self) -> bool {
        let nt = self.goal.turns.len();
        if self.p.close == 0 || nt < 2 || nt <= self.turn_seen || self.goal.switches().1 > 0 {
            return false;
        }
        let (start, n) = (self.goal.turns[nt - 1].start, self.tr.msgs.len());
        let has = |e: &Engine, mi: usize, k: UKind| e.fz.by_msg(mi).iter().any(|&i| e.tr.units[i].kind == k);
        if (start..n).filter(|&mi| has(self, mi, UKind::Call)).count() < 2 {
            return false;
        }
        self.turn_seen = nt;
        let asked = self.user_words();
        let said: FxHashSet<_> =
            self.words_in(start..n, &[UKind::Text, UKind::Call]).into_iter().filter(|w| !asked.contains(w)).collect();
        let sizes = self.sizes_now();
        let shown: FxHashSet<usize> = self.visible().iter().copied().collect();
        let need = self.asks(nt - 1);
        let named: FxHashSet<usize> =
            self.pick(&need, &FxHashSet::default(), |s| !s.out && s.idle.is_some()).into_iter().collect();
        let (mut idle, mut keep) = (0, Vec::new());
        for (i, s) in self.steps.iter().enumerate().filter(|(_, s)| !s.out && s.idle.is_some()) {
            if named.contains(&i) || s.read.iter().any(|w| said.contains(w)) {
                keep.push(i);
            } else {
                idle += s
                    .msgs
                    .iter()
                    .filter(|m| shown.contains(m))
                    .map(|&m| sizes.get(m).copied().unwrap_or(0))
                    .sum::<i64>();
            }
        }
        self.turn_keep = keep;
        idle >= self.p.check
    }

    /// The tokens of the latest outputs the agent has not written about that no cut may take (`still`): what the
    /// agent is working from is held whatever the system prompt, the request and the sticky set take, so a big
    /// head does not leave it the outputs of the newest step alone. Past the ceiling they leave all the same.
    pub(super) fn still_now(&self) -> i64 {
        self.p.still.max(0)
    }

    /// Whether the sticky set holds result `i` at the given tier: 0 below the target, 1 below the ceiling.
    fn holds(&self, i: usize, tier: usize, unwritten: usize) -> bool {
        self.stk.keep.contains(&i) || (tier == 0 && (self.stk.reads.contains(&i) || self.tr.units[i].msg >= unwritten))
    }

    /// Whether the view is past the ceiling: there a cut is made whatever it costs.
    fn over_ceiling(&self, total: i64) -> bool {
        self.ceiling() > 0 && total > self.ceiling()
    }

    /// Whether a landing is weighed from a plan before it is made (`Params.kept` 1 and 2): only the priced window
    /// (`target`) is; a `cap` set by hand is a limit the view never passes, whatever the cut sends again.
    pub(super) fn weighs_landings(&self) -> bool {
        matches!(self.p.kept, 1 | 2) && self.p.target > 0
    }

    /// Whether an epoch is tried on a copy of the engine and lands only when the view it really makes pays
    /// (`Params.kept` 3 and 4, `landing_pays`).
    pub(super) fn tries_landings(&self) -> bool {
        self.p.kept >= 3 && self.epochs()
    }

    /// Whether the view as it stands (head included) is past the working-set bound: the sticky set, the latest
    /// `still` tokens of what the agent has not written about, `BATCH`, and the `target` the cost rule lets a view
    /// keep before its next rewrite. A landing is refused only below it (`Params.kept` 3 and 4): past it the
    /// view holds more than the agent works from, and the next rewrite only gets larger. The sticky set is
    /// read again only when the view is past the rest of the bound.
    pub(super) fn past_working_set(&mut self) -> bool {
        let total = self.sizes_now().iter().sum::<i64>() + self.head;
        let room = self.still_now() + BATCH + self.p.target;
        if total <= room {
            return false;
        }
        self.refresh_sticky();
        total > self.stk.tokens + room
    }

    /// Whether the epoch of this request lands whatever it sends again: the first view of an engine (also after
    /// the host rewrote the history), a view past the ceiling or the model's window, and a `cap` set by hand.
    /// So does an epoch whose refusal would leave the view past the working-set bound (`past_working_set`,
    /// asked in `decide` of the view as it goes out with no epoch).
    pub(super) fn must_land(&mut self) -> bool {
        let view = self.sizes_now().iter().sum::<i64>();
        self.p.cap > 0
            || self.landed == 0
            || self.over_ceiling(view + self.head)
            || (self.budget > 0 && view > self.budget)
    }

    /// Whether the epoch made on `tried`, a copy of this engine as it was before the request was decided, lands
    /// (`Params.kept` 3 and 4). This engine decided the same request with no epoch: its view is the one sent last with
    /// the new messages at the end. Both views are measured as they go out, message by message, against the
    /// view sent last (`sent_fp`): the provider reads a view again without its cache from the first message that
    /// differs. The epoch lands when what it saves on the calls still to come (`calls_left`) is more than
    /// `lmargin` times what it sends again now: each token the view loses saves `ratio` of itself on every call,
    /// and each token less past `LONG_KNEE` the quality a long view loses (`LONG_LOSS`, as `fire_point` counts
    /// it); what it sends again is its view from the first message it changed, less what the view with no epoch
    /// reads without the cache anyway (the new messages, and what a recall brought back). An epoch that changes
    /// no message before that point costs the cache nothing and lands; so does any epoch when the view with no
    /// epoch is past the ceiling.
    pub(super) fn landing_pays(&mut self, tried: &mut Engine) -> bool {
        let (calls, margin) = (self.calls_left(), self.p.lmargin);
        self.landing_pays_over(tried, calls, margin)
    }

    /// Whether the epoch made on `tried` pays (`landing_pays`) when what it saves lasts `calls` calls and it has to
    /// save `margin` times what it sends again.
    pub(super) fn landing_pays_over(&mut self, tried: &mut Engine, calls: f64, margin: f64) -> bool {
        let (v0, v1) = (self.visible(), tried.visible());
        let (p0, p1) = (self.prints(&v0), tried.prints(&v1));
        let (s0, s1) = (self.sizes_now(), tried.sizes_now());
        let sent = self.sent_fp.clone();
        let same = |p: &[(usize, u64)]| p.iter().zip(sent.iter()).take_while(|(a, b)| a == b).count();
        let (a0, a1) = (same(&p0), same(&p1));
        let tokens = |p: &[(usize, u64)], s: &[i64]| p.iter().map(|&(mi, _)| s[mi]).sum::<i64>();
        let (t0, t1) = (tokens(&p0, &s0) + self.head, tokens(&p1, &s1) + self.head);
        if a1 >= a0 || self.over_ceiling(t0) {
            return true;
        }
        let resent = (tokens(&p1[a1..], &s1) - tokens(&p0[a0..], &s0)).max(0) as f64;
        let long = ((t0 - LONG_KNEE).max(0) - (t1 - LONG_KNEE).max(0)) as f64;
        let r = self.p.ratio;
        let saved = (r * (t0 - t1) as f64 + LONG_LOSS * long) * calls;
        saved > margin * (1.0 - r).max(0.01) * resent
    }

    /// The calls until the view has grown to where its size makes an epoch due (`epoch_due`), at the pace the history
    /// has grown so far (at least 1): what an epoch made before that saves lasts until then, since that one
    /// rewrites the view anyway.
    pub(super) fn calls_to_size_epoch(&mut self) -> f64 {
        let total = self.sizes_now().iter().sum::<i64>() + self.head;
        let (start, land) = self.window_span();
        let at = start.max(self.landed + start - land).max(self.looked + self.p.check);
        let pace = (self.raw_tokens / self.tr.steps.len().max(1) as i64).max(1);
        ((at - total).max(0) as f64 / pace as f64).max(1.0)
    }

    /// The calls a landing is paid back over (`Params.kept`): `lcalls`, and `lahead` for each step so far.
    pub(super) fn calls_left(&self) -> f64 {
        (self.p.lcalls + self.p.lahead * self.tr.steps.len() as f64).max(0.0)
    }

    /// Whether a landing pays (`Params.kept`): `drop` tokens leave the view for the calls still to come, each
    /// saving `ratio` of itself per call, and `resent` tokens that stay are read once without the cache.
    pub(super) fn lands(&self, drop: i64, resent: i64) -> bool {
        let r = self.p.ratio;
        r * drop.max(0) as f64 * self.calls_left() > self.p.lmargin * (1.0 - r) * resent.max(0) as f64
    }

    /// The window cut of this epoch before it is made (`Params.kept`): the first message it would change and
    /// the tokens it would take, by what each result saves when it leaves, oldest first down to where the cut
    /// lands. `None` when the view is not past where the cut starts or nothing can leave.
    pub(super) fn cut_plan(&mut self) -> Option<(usize, i64)> {
        let sizes = self.sizes_now();
        let total = sizes.iter().sum::<i64>() + self.head;
        let (start, land) = self.window_span();
        if total <= start {
            return None;
        }
        self.refresh_sticky();
        let unwritten = self.still_from(&sizes);
        let vis = self.visible();
        let (mut first, mut cut) = (None, 0i64);
        for &mi in vis.iter() {
            if total - cut <= land {
                break;
            }
            let pictured = has_picture(&self.tr.msgs[mi]);
            let take: i64 = self
                .fz
                .by_msg(mi)
                .iter()
                .filter(|&&i| {
                    let u = &self.tr.units[i];
                    u.kind == UKind::Result
                        && (u.saving >= LEAVE_LEAST || pictured)
                        && !self.holds(i, 0, unwritten)
                        && (0..self.fz.chunks[i].len()).any(|j| {
                            !self.dropped.get(&i).is_some_and(|d| d.contains(&j)) && !self.carrying.contains(&(i, j))
                        })
                })
                .map(|&i| self.tr.units[i].saving.max(0))
                .sum();
            let take = if pictured && take > 0 { sizes[mi] } else { take.min(sizes[mi]) };
            if take > 0 {
                first.get_or_insert(mi);
                cut += take;
            }
        }
        first.map(|f| (f, cut))
    }

    /// The first message the window cut of this epoch changes: the oldest result the sticky set does not hold that has a
    /// part to drop. `None` when the view does not run by epochs or is not past where the cut starts.
    pub(super) fn cut_start(&mut self) -> Option<usize> {
        if !self.epochs() || self.turn_only {
            return None;
        }
        if self.weighs_landings() {
            // the cut is made only when it pays for the view it sends again: until then nothing behind it is free
            let (first, cut) = self.cut_plan()?;
            let sizes = self.sizes_now();
            let behind: i64 = sizes[first..].iter().sum();
            if !self.over_ceiling(sizes.iter().sum::<i64>() + self.head) && !self.lands(cut, behind - cut) {
                return None;
            }
            self.resent_from = self.resent_from.min(first);
            return Some(first);
        }
        let sizes = self.sizes_now();
        if sizes.iter().sum::<i64>() + self.head <= self.window_span().0 {
            return None;
        }
        self.refresh_sticky();
        let unwritten = self.still_from(&sizes);
        self.visible().iter().copied().find(|&mi| {
            let pictured = has_picture(&self.tr.msgs[mi]);
            self.fz.by_msg(mi).iter().any(|&i| {
                let u = &self.tr.units[i];
                u.kind == UKind::Result
                    && (u.saving >= LEAVE_LEAST || pictured)
                    && !self.holds(i, 0, unwritten)
                    && (0..self.fz.chunks[i].len()).any(|j| {
                        !self.dropped.get(&i).is_some_and(|d| d.contains(&j)) && !self.carrying.contains(&(i, j))
                    })
            })
        })
    }

    /// Once rewriting pays (`fire_point`) the oldest results the sticky set does not hold leave, oldest first,
    /// until the view is down to the target, whatever the cache costs; past the ceiling the oldest the sticky set holds besides the latest `file now` of each path and
    /// the last check leave too (their note says how to bring them back). The first message changed.
    pub(super) fn window_cut(&mut self) -> Option<usize> {
        if (self.p.cap <= 0 && self.p.target <= 0) || self.turn_only {
            return None;
        }
        let sizes = self.sizes_now();
        let mut total: i64 = sizes.iter().sum::<i64>() + self.head;
        // the cut starts where rewriting pays and lands on the target: one rewrite buys a stretch of requests with
        // a view that stays small, not a trim at every request
        let (start, land) = self.window_span();
        if total <= start {
            return None;
        }
        if self.weighs_landings() && !self.over_ceiling(total) {
            // the cut waits until it pays for what it sends again; what a landing of this request already
            // sends again (`resent_from`) costs it nothing more
            let (at, cut) = self.cut_plan()?;
            let resent: i64 = sizes[at..at.max(self.resent_from).min(sizes.len())].iter().sum();
            if !self.lands(cut, resent - cut) {
                self.bump("window_waits", 1);
                return None;
            }
        }
        self.refresh_sticky();
        let unwritten = self.still_from(&sizes);
        let vis = self.visible();
        let mut first: Option<usize> = None;
        let mut cut = 0;
        for tier in 0..2 {
            let limit = if tier == 0 { land } else { self.ceiling_land() };
            if limit <= 0 {
                continue;
            }
            for &mi in vis.iter() {
                if total <= limit {
                    break;
                }
                let before = self.message(mi).1;
                let pictured = has_picture(&self.tr.msgs[mi]);
                let mut changed = false;
                for &i in self.fz.by_msg(mi).to_vec().iter() {
                    let u = &self.tr.units[i];
                    // a picture costs its nominal tokens whatever its text saves: its result leaves as a whole
                    if u.kind != UKind::Result
                        || (u.saving < LEAVE_LEAST && !pictured)
                        || self.holds(i, tier, unwritten)
                    {
                        continue;
                    }
                    let n = self.fz.chunks[i].len();
                    let mut did = false;
                    for j in 0..n {
                        if self.dropped.get(&i).is_some_and(|d| d.contains(&j)) || self.carrying.contains(&(i, j)) {
                            continue;
                        }
                        self.drop_one(i, j, if tier == 0 { "window_cap" } else { "window_hard" });
                        changed = true;
                        did = true;
                        cut += 1;
                    }
                    if did && !self.stk.cut.contains(&i) {
                        self.stk.cut.push(i);
                    }
                }
                if changed {
                    total += self.message(mi).1 - before;
                    first = Some(first.map_or(mi, |f| f.min(mi)));
                }
            }
            if tier == 1 && total > self.ceiling() {
                self.bump("window_over_hard", 1);
            }
        }
        if cut > 0 {
            self.bump("window_cuts", cut);
            self.bump("rewrites", 1);
        }
        first
    }

    /// For the replay: the tokens of the sticky set at the last decision.
    pub fn sticky_tokens(&self) -> i64 {
        self.stk.tokens
    }

    /// For the replay: the (message, block) of each result a window cut took parts of.
    pub fn cut_blocks(&self) -> Vec<(usize, usize)> {
        self.stk.cut.iter().map(|&i| (self.tr.units[i].msg, self.tr.units[i].block)).collect()
    }
}

/// Whether a message holds an image or a document, in a tool result or on its own.
fn has_picture(m: &serde_json::Value) -> bool {
    fn walk(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Array(a) => a.iter().any(walk),
            serde_json::Value::Object(o) => {
                matches!(o.get("type").and_then(|t| t.as_str()), Some("image" | "document"))
                    || o.get("content").is_some_and(walk)
            }
            _ => false,
        }
    }
    m.get("content").is_some_and(walk)
}
