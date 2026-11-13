use super::*;

// ------------------------------------------------------------ turns and threads

/// An additive logistic model: each feature is binned and every bin has a weight.
pub(super) struct Head {
    pub(super) edges: Vec<Vec<f64>>,
    pub(super) w: Vec<Vec<f64>>,
    pub(super) b: f64,
}

/// The link head (trained balanced, so corrected by the share of links it was trained on).
pub(super) struct LinkModel {
    pub(super) head: Head,
    pub(super) share: f64,
}

pub(super) const LINK_FEATURES: [&str; 13] = [
    "share",
    "overlap",
    "rarest",
    "asked",
    "names",
    "prev",
    "active",
    "since",
    "novelty",
    "words",
    "short_prev",
    "size",
    "anchor",
];

pub(super) fn link_model() -> Option<&'static LinkModel> {
    static M: OnceLock<Option<LinkModel>> = OnceLock::new();
    M.get_or_init(|| {
        let d: Value = serde_json::from_str(MODEL).ok()?;
        let names: Vec<&str> = d.get("link_features")?.as_array()?.iter().filter_map(Value::as_str).collect();
        if names != LINK_FEATURES {
            return None;
        }
        let link = d.get("link")?;
        let floats = |v: &Value| -> Option<Vec<Vec<f64>>> {
            v.as_array()?.iter().map(|r| r.as_array()?.iter().map(Value::as_f64).collect()).collect()
        };
        let head = Head { edges: floats(link.get("edges")?)?, w: floats(link.get("w")?)?, b: link.get("b")?.as_f64()? };
        if head.edges.len() != LINK_FEATURES.len() || head.w.len() != LINK_FEATURES.len() {
            return None;
        }
        let share = d.get("trained_on").and_then(|t| t.get("link_share")).and_then(Value::as_f64).unwrap_or(0.0);
        Some(LinkModel { head, share: if share != 0.0 { share } else { 0.5 } })
    })
    .as_ref()
}

pub(super) fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z.clamp(-30.0, 30.0)).exp())
}

pub(super) fn link_prob(x: &[f64; 13]) -> f64 {
    match link_model() {
        Some(m) => {
            let mut s = 0.0;
            for ((edges, w), &v) in m.head.edges.iter().zip(&m.head.w).zip(x) {
                s += w.get(edges.partition_point(|&e| e <= v)).copied().unwrap_or(0.0);
            }
            let (p, pi) = (sigmoid(m.head.b + s), m.share);
            p * pi / (p * pi + (1.0 - p) * (1.0 - pi))
        }
        None => {
            // the curator's stand-in for a trained head
            const W: [f64; 13] = [7.0, 0.0, 0.0, 1.5, 4.0, 1.5, 0.6, -0.4, -6.0, 0.0, 6.0, 0.0, -1.0];
            let mut s = 0.0;
            for (w, v) in W.iter().zip(x) {
                s += w * v;
            }
            sigmoid(-2.0 + s)
        }
    }
}

pub(super) struct Turn {
    pub(super) start: usize,
    pub(super) request: Set,
    pub(super) names: Set,
    pub(super) terms: HashSet<u32>,
    pub(super) seen: HashSet<u32>,
    pub(super) tokens: usize,
    pub(super) thread: usize,
}

/// Turns and threads, built message by message.
#[derive(Default)]
pub(super) struct Threads {
    pub(super) turns: Vec<Turn>,
    /// Thread -> its latest turn.
    pub(super) last: Vec<usize>,
    pub(super) df: HashMap<u32, usize>,
    pub(super) n: usize,
    pub(super) names: HashSet<u32>,
    /// IDF of the words of the latest request, as it opened its turn.
    pub(super) weights: Vec<(u32, f64)>,
}

impl Threads {
    pub(super) fn build(tr: &Trace) -> Threads {
        let mut th = Threads::default();
        for mi in 0..tr.msgs.len() {
            if tr.starts_turn(mi) {
                th.open(tr, mi);
            }
            let (lo, hi) = tr.by_msg[mi];
            let Some(t) = th.turns.last_mut() else { continue };
            for u in &tr.units[lo..hi] {
                t.terms.extend(&u.terms);
                t.seen.extend(&u.words);
                t.tokens += u.tokens;
                for &w in &u.terms {
                    *th.df.entry(w).or_default() += 1;
                }
                th.names.extend(&u.words);
            }
            th.n += hi - lo;
        }
        th
    }

    pub(super) fn open(&mut self, tr: &Trace, mi: usize) {
        let asked: Vec<usize> = tr.requests(mi).collect();
        let req = union(asked.iter().map(|&i| tr.units[i].terms.clone()));
        let names = union(asked.iter().map(|&i| tr.units[i].words.clone()));
        let n = self.turns.len();
        let mut turn =
            Turn { start: mi, request: req, names, terms: HashSet::new(), seen: HashSet::new(), tokens: 0, thread: 0 };
        if n == 0 {
            self.turns.push(turn);
            self.last.push(0);
            return;
        }
        self.weights = turn.request.iter().map(|&w| (w, self.idf(w))).collect();
        let mut joint = vec![0.0f64; self.last.len()];
        for j in 0..n {
            let p = link_prob(&self.features(&turn.request, &turn.names, &self.weights, j, n));
            let k = self.turns[j].thread;
            joint[k] = joint[k].max(p);
        }
        let mut order: Vec<usize> = (0..joint.len()).collect();
        order.sort_by(|&a, &b| joint[b].total_cmp(&joint[a]).then(self.last[b].cmp(&self.last[a])));
        let best = order[0];
        let th = if joint[best] >= LINK { best } else { self.last.len() };
        if th == self.last.len() {
            self.last.push(n);
        }
        self.last[th] = n;
        turn.thread = th;
        self.turns.push(turn);
    }

    pub(super) fn idf(&self, w: u32) -> f64 {
        (1.0 + self.n as f64 / (1 + self.df.get(&w).copied().unwrap_or(0)) as f64).ln()
    }

    /// What is known, when a request arrives, about whether its turn (number `n`) draws on turn `j`.
    pub(super) fn features(&self, req: &Set, names: &Set, weights: &[(u32, f64)], j: usize, n: usize) -> [f64; 13] {
        let t = &self.turns[j];
        let total = weights.iter().map(|x| x.1).sum::<f64>();
        let total = if total == 0.0 { 1.0 } else { total };
        let (mut s, mut asked, mut top) = (0.0, 0.0, 0.0f64);
        for &(w, x) in weights {
            if t.terms.contains(&w) {
                s += x;
                top = top.max(x);
            }
            if has(&t.request, w) {
                asked += x;
            }
        }
        let prev = if j + 1 == n { 1.0 } else { 0.0 };
        let seen = names.iter().filter(|w| t.seen.contains(w)).count();
        let new = names.iter().filter(|w| !self.names.contains(w)).count();
        [
            s / total,
            s.ln_1p(),
            top,
            asked / total,
            if names.is_empty() { 0.0 } else { seen as f64 / names.len() as f64 },
            prev,
            if t.thread == self.turns[n - 1].thread { 1.0 } else { 0.0 },
            ((n - 1 - j) as f64).ln_1p(),
            new as f64 / (names.len() + 2) as f64,
            (req.len() as f64).ln_1p(),
            prev / (1 + req.len()) as f64,
            (t.tokens as f64).ln_1p(),
            if j == 0 { 1.0 } else { 0.0 },
        ]
    }

    /// Probability that the turn in progress (the last one) draws on turn `j`.
    pub(super) fn relevance(&self, j: usize) -> f64 {
        let n = self.turns.len() - 1;
        let cur = &self.turns[n];
        link_prob(&self.features(&cur.request, &cur.names, &self.weights, j, n))
    }

    /// Messages of turn `j`: `[start, end)`.
    pub(super) fn span(&self, j: usize, msgs: usize) -> (usize, usize) {
        let end = self.turns.get(j + 1).map_or(msgs, |t| t.start);
        (self.turns[j].start, end)
    }

    /// What the person asked in turn `j`, on one line.
    pub(super) fn ask(&self, tr: &Trace, j: usize) -> String {
        let text: Vec<&str> = tr.requests(self.turns[j].start).map(|i| tr.units[i].text.as_str()).collect();
        one_line(&text.join(" "), CARD_ASK)
    }
}
