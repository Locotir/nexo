//! The provider's cache as the engine models it: the pocket of each topic and how long the provider keeps a view.

use serde_json::Value;

use super::Engine;

/// The last view sent on a topic (`Params.pockets`): (message, hash of it as sent), and when.
#[derive(Clone)]
pub(super) struct Pocket {
    sent: Vec<(usize, u64)>,
    at: f64,
    /// How many messages the conversation had then.
    end: usize,
}

/// The view just decided, weighed against the pockets: the tokens the cache gives it if it keeps the views
/// up to each age, (age, tokens) youngest first, each giving more than the one before; and its tokens.
#[derive(Clone)]
pub(super) struct Probe {
    steps: Vec<(f64, i64)>,
    total: i64,
}

/// The pockets kept: a session of many topics drops the oldest.
const POCKETS_KEPT: usize = 16;

/// What the provider says it read tells a pocket's age apart only when the views it would and would not
/// give differ by this share of the request at least; the provider counts in blocks and our tokens are
/// estimates.
const LIFE_GAP: f64 = 0.1;

/// A view the provider did not give at this age or younger tells nothing of its lifetime: a cache keeps
/// views at least this long, and a miss sooner is the provider's own load.
const LIFE_LEAST: f64 = 60.0;

/// A hash of a message as sent, never 0 (0 marks a message sent as it came).
pub(super) fn fingerprint(v: &Value) -> u64 {
    use std::hash::Hasher;
    struct H(rustc_hash::FxHasher);
    impl std::io::Write for H {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.write(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut h = H(rustc_hash::FxHasher::default());
    // writing to a hasher does not fail
    let _ = serde_json::to_writer(&mut h, v);
    h.0.finish() | 1
}

impl Engine {
    /// The view `vis` as sent, message by message: (message, the hash of it as it goes), so that two views
    /// hash alike as far as the provider reads them alike.
    pub(super) fn prints(&mut self, vis: &[usize]) -> Vec<(usize, u64)> {
        if self.raw_fp.len() < self.tr.msgs.len() {
            self.raw_fp.resize(self.tr.msgs.len(), 0);
        }
        vis.iter()
            .map(|&mi| {
                if self.message(mi).0.is_some() {
                    return (mi, self.built[&mi].fp);
                }
                if self.raw_fp[mi] == 0 {
                    self.raw_fp[mi] = fingerprint(&self.tr.msgs[mi]);
                }
                (mi, self.raw_fp[mi])
            })
            .collect()
    }

    /// The topic whose pocket the view is (`Params.pockets`): the one the person is on when the other topics
    /// leave the view, else the one whole view.
    fn pocket_key(&self) -> usize {
        if !self.goal.turns.is_empty() && self.apart() {
            self.goal.active()
        } else {
            usize::MAX
        }
    }

    /// Seconds a step takes on average, from the `now` the host gave (0 until two views were seen).
    fn secs_per_step(&self) -> f64 {
        match self.t0 {
            Some((t, k)) if self.tr.steps.len() > k => ((self.now - t) / (self.tr.steps.len() - k) as f64).max(0.0),
            _ => 0.0,
        }
    }

    /// Whether the pocket of topic `k` (`Params.pockets`) will still live when the person is back on it, in
    /// `hz` steps: the lifetime learned (or set) covers the time left, and the topic has had no message
    /// since the pocket was kept, so a return finds the view it left.
    pub(super) fn pocket_lives(&self, k: usize, hz: f64) -> bool {
        if self.p.pockets < 1 {
            return false;
        }
        let Some(p) = self.pockets.get(&k) else { return false };
        let life = self.p.life.min(self.life_hi);
        let last = self.goal.members[k].last().map_or(0, |&j| self.goal.span(j, self.tr.msgs.len()).1);
        life > 0.0 && (self.now - p.at) + hz * self.secs_per_step() <= life && p.end + 1 >= last
    }

    /// Where the provider's cache stops in the view as it stands, by the pockets (`Params.pockets`): the first
    /// message no pocket that lives holds, the same by every pocket however old, and whether nothing past the
    /// anchor is held (`usize::MAX`: all of it is held, or the cache has seen none of it yet). Counts what a
    /// pocket older than the latest view brings, and a topic come back to after its pocket is gone.
    pub(super) fn warm_now(&mut self) -> (usize, usize, bool) {
        if self.pockets.is_empty() {
            return (usize::MAX, usize::MAX, false);
        }
        let vis = self.visible();
        let cur = self.prints(&vis);
        let life = self.p.life.min(self.life_hi);
        let young = self.pockets.values().map(|p| p.at).fold(f64::NEG_INFINITY, f64::max);
        let (now, own) = (self.now, self.pocket_key());
        // each pocket: whether it lives, whether it is the latest, whether it is this topic's, and the first
        // place of the view it does not hold (`usize::MAX`: none)
        let held: Vec<(bool, bool, bool, usize)> = self
            .pockets
            .iter()
            .map(|(&k, p)| {
                let n = cur.iter().zip(&p.sent).take_while(|(a, b)| a == b).count();
                let lives = if life > 0.0 { now - p.at <= life } else { p.at >= young };
                (lives, p.at >= young, k == own, if n == p.sent.len().min(cur.len()) { usize::MAX } else { n })
            })
            .collect();
        let live = held.iter().filter(|h| h.0).map(|h| h.3).max();
        let all = held.iter().map(|h| h.3).max().unwrap_or(0);
        let latest = held.iter().filter(|h| h.1).map(|h| h.3).max().unwrap_or(0);
        if let Some(c) = live.filter(|&c| c > latest) {
            let tokens: i64 = (latest..c.min(cur.len())).map(|j| self.message(cur[j].0).1).sum();
            self.bump("pocket_back", 1);
            self.bump("pocket_hit", tokens);
        }
        if held.iter().any(|h| h.2 && !h.0) {
            self.bump("pocket_dead", 1);
        }
        let place = |c: usize| if c == usize::MAX { usize::MAX } else { cur[c].0 };
        let warm = live.map_or(0, place);
        (warm, place(all), warm != usize::MAX && live.is_none_or(|c| c <= 1))
    }

    /// The view just decided is the pocket of its topic (`Params.pockets`); what the cache gives it by how
    /// long the cache keeps views is kept for `learn`.
    pub(super) fn keep_pocket(&mut self) {
        let vis = self.visible();
        let fin = self.prints(&vis);
        let tokens: Vec<i64> = vis.iter().map(|&mi| self.message(mi).1).collect();
        let now = self.now;
        let mut by_age: Vec<(f64, i64)> = self
            .pockets
            .values()
            .map(|p| {
                let n = fin.iter().zip(&p.sent).take_while(|(a, b)| a == b).count();
                (now - p.at, tokens[..n].iter().sum())
            })
            .collect();
        by_age.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut steps: Vec<(f64, i64)> = Vec::new();
        for (age, hit) in by_age {
            if steps.last().is_none_or(|s| hit > s.1) {
                steps.push((age, hit));
            }
        }
        self.probe = Some(Probe { steps, total: tokens.iter().sum() });
        let key = self.pocket_key();
        self.pockets.insert(key, Pocket { sent: fin, at: now, end: self.tr.msgs.len() });
        if self.pockets.len() > POCKETS_KEPT {
            let oldest = self.pockets.iter().min_by(|a, b| a.1.at.total_cmp(&b.1.at)).map(|(k, _)| *k);
            if let Some(k) = oldest {
                self.pockets.remove(&k);
            }
        }
    }

    /// What the provider said of the call made with the view last decided (`Curator::cached`): the input
    /// tokens it read from its cache, and all of them. The share it read tells which pockets it still kept,
    /// so how long it keeps a view: a pocket it gave sets the least lifetime, one it did not give the most,
    /// unless that age is too young to tell (`LIFE_LEAST`) or below one it gave. A provider that never says
    /// it read from its cache teaches nothing.
    /// What a cached input token costs against an uncached one, from the provider's prices.
    pub fn set_ratio(&mut self, ratio: f64) {
        self.p.ratio = ratio;
    }

    #[cfg(test)]
    pub fn ratio(&self) -> f64 {
        self.p.ratio
    }

    pub fn learn(&mut self, sys: u64, read: u64, input: u64) {
        let Some(probe) = self.probe.take() else { return };
        self.read_seen |= read > 0;
        if input == 0 || !self.read_seen {
            return;
        }
        let all = (sys as i64 + probe.total).max(1) as f64;
        // what it read, in the estimated tokens of the view
        let got = read.min(input) as f64 / input as f64 * all;
        let mut below = 0.0;
        for &(age, hit) in &probe.steps {
            let level = sys as f64 + hit as f64;
            let gap = level - below;
            if gap >= LIFE_GAP * all {
                if got >= below + 0.7 * gap {
                    self.life_lo = self.life_lo.max(age);
                } else {
                    if got <= below + 0.3 * gap && age >= LIFE_LEAST && age > self.life_lo && age < self.life_hi {
                        self.life_hi = age;
                        self.bump("life_learned", 1);
                    }
                    break;
                }
            }
            below = level;
        }
        // it gave a view older than the lifetime learned: that was the provider's load, not its lifetime
        if self.life_lo >= self.life_hi {
            self.life_hi = f64::INFINITY;
            self.bump("life_reset", 1);
        }
    }
}
