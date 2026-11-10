//! The index of the session: for each key (`facts.rs`), where the lines that hold it are, as (unit, line) pairs.
//!
//! It grows as units arrive and is never read back whole: a request looks up only its own keys. A key weighs by how
//! rare it is (`ln(lines / holders)`), so a number or a name few lines hold decides, and a word every line holds
//! decides nothing. The lines come from any part of the history, whatever topic it was on. No text is stored: a
//! pair points at the unit, and the caller reads the line from it.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::facts::{key, keys};

/// A line of a unit, as it is told by the unit's number and the line's.
pub type Post = (u32, u32);

/// A key of the request that is held by more lines than this brings none of them in on its own.
const RARE: usize = 64;

/// Lines of a unit read; the rest of a huge output is not indexed.
const UNIT_LINES: usize = 20_000;

/// Keys of a request kept (the rarest in the history).
const NEED_KEYS: usize = 24;

/// Characters at each end of the request that are read.
const NEED_CHARS: usize = 400;

#[derive(Clone, Default)]
pub struct Index {
    posts: FxHashMap<u64, Vec<Post>>,
    lines: usize,
    /// The keys of the person's latest request.
    need: Vec<u64>,
    /// Those of its keys that hold a digit: a number names one thing (topic 32, port 8797) more than a word does.
    need_num: FxHashSet<u64>,
    /// The unit of that request, which is not history to itself.
    asked: u32,
}

impl Index {
    /// Indexes the lines of unit ``unit``, whose text is ``text``.
    pub fn add(&mut self, unit: usize, text: &str) {
        let mut ks = Vec::new();
        for (n, line) in text.lines().enumerate().take(UNIT_LINES) {
            keys(line, &mut ks);
            if ks.is_empty() {
                continue;
            }
            self.lines += 1;
            for &k in &ks {
                self.posts.entry(k).or_default().push((unit as u32, n as u32));
            }
        }
    }

    /// Takes ``text`` as the latest request of the person: its head and its tail make the keys a request looks up.
    pub fn want(&mut self, unit: usize, text: &str) {
        self.asked = unit as u32;
        let n = text.chars().count();
        let read: String = if n <= 2 * NEED_CHARS {
            text.to_string()
        } else {
            text.chars().take(NEED_CHARS).chain("\n".chars()).chain(text.chars().skip(n - NEED_CHARS)).collect()
        };
        self.need.clear();
        self.need_num.clear();
        let mut ks = Vec::new();
        for line in read.lines() {
            keys(line, &mut ks);
            self.need.extend_from_slice(&ks);
            for run in line.split(|c: char| !c.is_alphanumeric()).filter(|r| r.bytes().any(|b| b.is_ascii_digit())) {
                self.need_num.insert(key(run));
            }
        }
        self.need.sort_unstable();
        self.need.dedup();
    }

    pub fn is_empty(&self) -> bool {
        self.need.is_empty() || self.lines == 0
    }

    /// Whether line ``p`` holds key ``k``.
    fn holds(&self, k: u64, p: Post) -> bool {
        self.posts.get(&k).is_some_and(|v| v.binary_search(&p).is_ok())
    }

    /// The lines of the history that the latest request names, best first: for each key of the request that few
    /// lines hold (the rarest first), the ``per_key`` lines that hold the most of the request's keys by weight
    /// (the newest among equals), ``total`` in all. ``skip`` says which are not wanted (already in the view).
    pub fn pick(&self, per_key: usize, total: usize, skip: &mut dyn FnMut(Post) -> bool) -> Vec<Post> {
        let l = self.lines.max(2) as f64;
        let mut held: Vec<(usize, u64)> =
            self.need.iter().filter_map(|k| self.posts.get(k).map(|v| (v.len(), *k))).collect();
        held.sort_unstable();
        held.truncate(NEED_KEYS);
        let weight = |df: usize| (l / df as f64).ln();
        // few holders is few against the history: early on, a word of every other line is not rare
        let rare = RARE.min(self.lines / 16).max(2);
        // keys that name a line: held by few lines, or by a small share of all of them
        let named_cap = (8 * rare).max(self.lines / 10);
        let mut out: Vec<Post> = Vec::new();
        let mut seen: FxHashSet<Post> = FxHashSet::default();
        // a number is a name even when it is held by more lines (a small number is in many tables)
        for &(df, k) in held.iter().filter(|&&(df, k)| df <= rare || (df <= 8 * rare && self.need_num.contains(&k))) {
            if out.len() >= total {
                break;
            }
            // what the view holds whole the model has: a key that a line of it holds is not looked for
            if self.posts[&k].iter().any(|&p| p.0 != self.asked && skip(p)) {
                continue;
            }
            let mut cands: Vec<(f64, Post)> = self.posts[&k]
                .iter()
                .filter(|p| p.0 != self.asked && !seen.contains(*p))
                .filter_map(|&p| {
                    let (mut score, mut named) = (0.0, 0);
                    for &(d, _) in held.iter().filter(|&&(_, o)| o == k || self.holds(o, p)) {
                        score += weight(d);
                        named += (d <= named_cap) as usize;
                    }
                    // one rare key is a line by chance (a number that a log line holds), unless it is a unique one
                    (df <= 2 || named >= 2).then_some((score, p))
                })
                .collect();
            cands.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
            let mut taken = 0;
            for (_, p) in cands {
                if taken >= per_key || out.len() >= total {
                    break;
                }
                if skip(p) {
                    continue;
                }
                seen.insert(p);
                out.push(p);
                taken += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Index {
        let mut ix = Index::default();
        // unit 0: the same setting read twice (the later read is the one in force), then filler for the weights
        ix.add(0, "[topic 412] port = 1111\nfiller line one\n");
        ix.add(1, "[topic 412] port = 2222\n");
        for u in 2..1500 {
            ix.add(u, &format!("line {u} of the log, handled in {} ms\nnothing else here\n", u % 7));
        }
        ix.add(1500, "What is the port of the service in topic 412?");
        ix.want(1500, "What is the port of the service in topic 412?");
        ix
    }

    fn lines(ix: &Index, per_key: usize) -> Vec<Post> {
        ix.pick(per_key, 10, &mut |_| false)
    }

    #[test]
    fn a_number_names_the_line_and_the_newest_comes_first() {
        let ix = session();
        assert_eq!(ix.pick(1, 1, &mut |_| false), vec![(1, 0)], "the newest of equal lines");
        let both = lines(&ix, 4);
        assert!(both.contains(&(0, 0)) && both.contains(&(1, 0)), "{both:?}");
        assert!(both.iter().all(|p| p.0 != 1500), "the request is not its own history");
    }

    #[test]
    fn a_number_that_a_log_line_happens_to_hold_brings_nothing() {
        let mut ix = session();
        ix.want(1500, "Look at the service in topic 5, it took 3 ms");
        assert!(lines(&ix, 4).is_empty(), "{:?}", lines(&ix, 4));
    }

    #[test]
    fn what_the_view_holds_is_not_looked_for() {
        let ix = session();
        // the newer read is in the view: the model has the number already, and the older read is not brought
        assert!(ix.pick(4, 10, &mut |p| p.0 == 1).is_empty());
        assert!(!ix.pick(4, 10, &mut |_| false).is_empty());
    }
}
