//! The exact clairvoyant optimum of a prefix-cached conversation, as a pure function.
//!
//! This is the reference the cold replay (`examples/replay.rs`) and the tests use. The engine never calls it.
//!
//! Request `i` sends a view `V_i` of the history. It costs `hit * tokens(V_i) + (miss - hit) * missed_i`, where
//! `missed_i` is the tokens of `V_i` at or after the first position where it differs from `V_{i-1}` (everything
//! at request 0). A clairvoyant knows which pieces of the history will never be needed again (`dead`) and
//! picks, request by request, whether to cut and where; cutting at a piece removes every dead piece from there
//! on but pays the cache behind it. [`optimum`] returns the cheapest schedule of cuts.
//!
//! A view that greedily drops every piece as soon as it may go is only an upper bound of this; ignoring the
//! price of breaking the cache is a lower bound.

/// One piece of the history, in view order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub tokens: i64,
    /// The first request that sends it.
    pub born: usize,
    /// It is in every view of the requests `born..dead` (`dead` exclusive, `dead > born`); from request `dead`
    /// on it may be absent. `dead == n` means it can never go.
    pub dead: usize,
}

/// A price, split as the replay prints it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub total: f64,
    /// `hit * ` all tokens sent.
    pub rent: f64,
    /// `(miss - hit) * ` all tokens missed.
    pub rewrite: f64,
    pub cuts: usize,
}

#[derive(Clone, Copy, Default)]
struct Cell {
    total: f64,
    rent: f64,
    cuts: u32,
}

/// The cheapest cost of `n` requests (`0..n`) over `pieces`, with a prefix cache that reads a token at price
/// `hit` and writes it at price `miss`.
///
/// At each request `i` in `1..n` a schedule may cut at one position `c`, a piece with `dead <= i` still present:
/// every piece at or after `c` with `dead <= i` leaves at `i`. Pieces never come back.
///
/// With `stub > 0` every removable piece gets a companion of `stub` tokens right after it, present from its
/// `dead` request to the end and never removed: the tokens a view shows in place of what it cut. That gives
/// an upper bound of the optimum with stubs (a stub only shows once its piece has left, and here it always
/// does); `stub == 0` is the optimum without them. Pieces must be in view order with `born` not decreasing.
pub fn optimum(pieces: &[Piece], n: usize, hit: f64, miss: f64, stub: i64) -> Cost {
    if n == 0 {
        return Cost::default();
    }
    let extra = miss - hit;
    let mut all: Vec<Piece> = Vec::with_capacity(pieces.len() * 2);
    for p in pieces {
        let p = Piece { dead: p.dead.min(n), ..*p };
        all.push(p);
        if stub > 0 && p.dead < n {
            all.push(Piece { tokens: stub, born: p.dead, dead: n });
        }
    }
    // the part no schedule changes: each piece carried over the requests it must be in, missed once when born
    let (mut carried, mut born_tokens) = (0f64, 0f64);
    for p in all.iter().filter(|p| p.born < n) {
        carried += p.tokens as f64 * (p.dead - p.born.min(p.dead)) as f64;
        born_tokens += p.tokens as f64;
    }
    let fixed_rent = hit * carried;
    let fixed_rewrite = extra * born_tokens;
    // the removable positions, adjacent ones with the same `dead` merged (cutting at the first dominates)
    let mut groups: Vec<(usize, usize, f64)> = Vec::new();
    let mut last = usize::MAX;
    for (idx, p) in all.iter().enumerate().filter(|(_, p)| p.dead < n && p.born < p.dead) {
        match groups.last_mut() {
            Some(g) if last != usize::MAX && idx == last + 1 && g.1 == p.dead => g.2 += p.tokens as f64,
            _ => groups.push((idx, p.dead, p.tokens as f64)),
        }
        last = idx;
    }
    let done = |top: Cell| Cost {
        total: fixed_rent + fixed_rewrite + top.total,
        rent: fixed_rent + top.rent,
        rewrite: fixed_rewrite + top.total - top.rent,
        cuts: top.cuts as usize,
    };
    if groups.is_empty() {
        return done(Cell::default());
    }
    // the table of G(a, b, k) for the current k, updated in place from k + 1 down to k: only the cells with
    // a < dead_k <= b change, and they read cells of the same k with a smaller b and cells with a >= dead_k
    let at = |a: usize, b: usize| b * (b - 1) / 2 + a;
    let mut table = vec![Cell::default(); n * (n + 1) / 2];
    let mut diff = vec![0f64; n + 2];
    let mut cut_cost = vec![0f64; n + 1];
    let mut added = all.len();
    for &(pos, dead, tokens) in groups.iter().rev() {
        // K(i, pos): the tokens after `pos` with born < i < dead, as a running sum of ranges
        while added > pos + 1 {
            added -= 1;
            let q = &all[added];
            let (lo, hi) = (q.born + 1, q.dead.min(n).saturating_sub(1));
            if q.born < n && lo <= hi {
                diff[lo] += q.tokens as f64;
                diff[hi + 1] -= q.tokens as f64;
            }
        }
        let mut run = 0.0;
        for (i, c) in cut_cost.iter_mut().enumerate() {
            run += diff[i];
            *c = extra * run;
        }
        for b in dead..=n {
            let rent = hit * tokens * (b - dead) as f64;
            for a in 0..dead {
                let old = table[at(a, b)];
                let mut best = Cell { total: old.total + rent, rent: old.rent + rent, cuts: old.cuts };
                let mut by = None;
                for i in dead.max(a + 1)..b {
                    let total = cut_cost[i] + table[at(a, i)].total + table[at(i, b)].total;
                    if total < best.total && by.is_none_or(|(t, _)| total < t) {
                        by = Some((total, i));
                    }
                }
                if let Some((total, i)) = by {
                    let (l, r) = (table[at(a, i)], table[at(i, b)]);
                    best = Cell { total, rent: l.rent + r.rent, cuts: l.cuts + r.cuts + 1 };
                }
                table[at(a, b)] = best;
            }
        }
    }
    done(table[at(0, n)])
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % below
        }
    }

    /// A random instance: `born` not decreasing along the pieces, at most 6 removable.
    fn instance(rng: &mut Lcg) -> (Vec<Piece>, usize) {
        let n = 2 + rng.next(5);
        let m = 1 + rng.next(10);
        let mut born: Vec<usize> = (0..m).map(|_| rng.next(n)).collect();
        born.sort_unstable();
        let mut removable = 0;
        let pieces = born
            .into_iter()
            .map(|b| {
                let mut dead = n;
                if b + 1 < n && removable < 6 && rng.next(3) > 0 {
                    dead = b + 1 + rng.next(n - 1 - b);
                    removable += 1;
                }
                Piece { tokens: 1 + rng.next(20) as i64, born: b, dead }
            })
            .collect();
        (pieces, n)
    }

    /// The literal price of a sequence of views (lists of piece ids): prefix compare.
    fn price(views: &[Vec<usize>], pieces: &[Piece], hit: f64, miss: f64) -> f64 {
        let tok = |v: &[usize]| v.iter().map(|&p| pieces[p].tokens as f64).sum::<f64>();
        views
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let same = if i == 0 { 0 } else { v.iter().zip(&views[i - 1]).take_while(|(a, b)| a == b).count() };
                hit * tok(v) + (miss - hit) * tok(&v[same..])
            })
            .sum()
    }

    fn view(pieces: &[Piece], gone: &[bool], i: usize) -> Vec<usize> {
        (0..pieces.len()).filter(|&p| pieces[p].born <= i && !gone[p]).collect()
    }

    fn brute(
        pieces: &[Piece],
        n: usize,
        i: usize,
        gone: &[bool],
        views: &mut Vec<Vec<usize>>,
        hit: f64,
        miss: f64,
    ) -> f64 {
        if i == n {
            return price(views, pieces, hit, miss);
        }
        let mut best = f64::MAX;
        let mut choices = vec![None];
        choices.extend((0..pieces.len()).filter(|&p| pieces[p].dead <= i && !gone[p] && pieces[p].born < i).map(Some));
        for c in choices {
            let mut next = gone.to_vec();
            if let Some(c) = c {
                for q in c..pieces.len() {
                    if pieces[q].dead <= i {
                        next[q] = true;
                    }
                }
            }
            views.push(view(pieces, &next, i));
            best = best.min(brute(pieces, n, i + 1, &next, views, hit, miss));
            views.pop();
        }
        best
    }

    fn brute_force(pieces: &[Piece], n: usize, hit: f64, miss: f64) -> f64 {
        let gone = vec![false; pieces.len()];
        let mut views = vec![view(pieces, &gone, 0)];
        brute(pieces, n, 1, &gone, &mut views, hit, miss)
    }

    const PRICES: [(f64, f64); 4] = [(0.1, 1.0), (0.02, 1.0), (0.5, 1.0), (0.1, 1.25)];

    #[test]
    fn matches_brute_force() {
        let mut cutting = 0;
        for (hit, miss) in PRICES {
            let mut rng = Lcg(0x5eed ^ (hit * 1000.0) as u64);
            for case in 0..300 {
                let (pieces, n) = instance(&mut rng);
                let want = brute_force(&pieces, n, hit, miss);
                let got = optimum(&pieces, n, hit, miss, 0);
                assert!((want - got.total).abs() < 1e-6, "case {case} {pieces:?} n {n}: brute {want} dp {}", got.total);
                assert!((got.rent + got.rewrite - got.total).abs() < 1e-6);
                cutting += usize::from(got.cuts > 0);
            }
            assert!(cutting > 30, "only {cutting} instances cut");
        }
    }

    /// Cuts the first present dead piece at every request that has one (`always`), or only when it pays by the
    /// oracle's rule (tokens cut times requests left over the cache behind it).
    fn greedy(pieces: &[Piece], n: usize, hit: f64, miss: f64, always: bool) -> f64 {
        let mut gone = vec![false; pieces.len()];
        let mut views = vec![view(pieces, &gone, 0)];
        for i in 1..n {
            if let Some(c) = (0..pieces.len()).find(|&p| pieces[p].dead <= i && !gone[p] && pieces[p].born < i) {
                let freed: f64 = (c..pieces.len())
                    .filter(|&q| pieces[q].dead <= i && !gone[q])
                    .map(|q| pieces[q].tokens as f64)
                    .sum();
                let behind: f64 = (c..pieces.len())
                    .filter(|&q| !gone[q] && pieces[q].dead > i)
                    .map(|q| pieces[q].tokens as f64)
                    .sum();
                if always || hit * freed * (n - i) as f64 > (miss - hit) * behind {
                    for q in c..pieces.len() {
                        if pieces[q].dead <= i {
                            gone[q] = true;
                        }
                    }
                }
            }
            views.push(view(pieces, &gone, i));
        }
        price(&views, pieces, hit, miss)
    }

    #[test]
    fn never_worse_than_the_greedy_schedule() {
        for (hit, miss) in PRICES {
            let mut rng = Lcg(0xbeef ^ (miss * 100.0) as u64);
            for case in 0..300 {
                let (pieces, n) = instance(&mut rng);
                let got = optimum(&pieces, n, hit, miss, 0).total;
                for always in [false, true] {
                    let g = greedy(&pieces, n, hit, miss, always);
                    assert!(got <= g + 1e-6, "case {case}: optimum {got} over greedy {g} (always {always})");
                }
            }
        }
    }

    fn tail_case(n: usize) -> (Vec<Piece>, usize) {
        let piece = |tokens, dead| Piece { tokens, born: 0, dead };
        // 100 kept, 50 that may go from request 1, 10 kept behind it: a cut at 1 costs 9 and saves 5 a request
        (vec![piece(100, n), piece(50, 1), piece(10, n)], n)
    }

    #[test]
    fn one_dead_piece_near_the_tail_is_cut_only_when_it_pays() {
        let (p, n) = tail_case(2);
        assert_eq!(optimum(&p, n, 0.1, 1.0, 0).cuts, 0, "saves 5, costs 9");
        let (p, n) = tail_case(3);
        let c = optimum(&p, n, 0.1, 1.0, 0);
        assert_eq!(c.cuts, 1, "saves 10, costs 9");
        let plain = 0.1 * 160.0 * 3.0 + 0.9 * 160.0;
        assert!((c.total - (plain - 1.0)).abs() < 1e-9, "{}", c.total);
    }

    #[test]
    fn nothing_dead_costs_the_plain_price() {
        let pieces = [Piece { tokens: 30, born: 0, dead: 4 }, Piece { tokens: 7, born: 2, dead: 4 }];
        let c = optimum(&pieces, 4, 0.1, 1.0, 0);
        assert_eq!(c.cuts, 0);
        let plain = 0.1 * (30.0 * 4.0 + 7.0 * 2.0) + 0.9 * 37.0;
        assert!((c.total - plain).abs() < 1e-9);
    }

    #[test]
    fn a_free_cache_never_cuts() {
        let (p, n) = tail_case(5);
        let c = optimum(&p, n, 0.0, 1.0, 0);
        assert_eq!(c.cuts, 0);
        assert!((c.total - 160.0).abs() < 1e-9);
    }

    #[test]
    fn hit_equal_to_miss_drops_everything_at_dead() {
        let (p, n) = tail_case(5);
        let c = optimum(&p, n, 0.5, 0.5, 0);
        assert!((c.total - 0.5 * (100.0 * 5.0 + 50.0 * 1.0 + 10.0 * 5.0)).abs() < 1e-9, "{}", c.total);
    }

    #[test]
    fn stubs_cost_more_and_stay_bounded() {
        let mut rng = Lcg(77);
        for _ in 0..100 {
            let (pieces, n) = instance(&mut rng);
            let (low, high) = (optimum(&pieces, n, 0.1, 1.0, 0).total, optimum(&pieces, n, 0.1, 1.0, 3).total);
            assert!(low <= high + 1e-9);
        }
    }
}
