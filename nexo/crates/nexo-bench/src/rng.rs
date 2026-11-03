//! A small deterministic generator (splitmix64), so a task's files and hidden cases are the same on
//! every machine and every run without a dependency.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// A number in `lo..=hi`.
    pub fn int(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// `k` distinct elements, in random order.
    pub fn sample<T: Clone>(&mut self, items: &[T], k: usize) -> Vec<T> {
        let mut idx: Vec<usize> = (0..items.len()).collect();
        for i in 0..k.min(idx.len()) {
            let j = i + self.below(idx.len() - i);
            idx.swap(i, j);
        }
        idx[..k.min(items.len())].iter().map(|&i| items[i].clone()).collect()
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_numbers() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        assert!((0..50).all(|_| a.int(1, 6) == b.int(1, 6)));
    }

    #[test]
    fn int_stays_inside_its_bounds_and_sample_is_distinct() {
        let mut r = Rng::new(1);
        assert!((0..1000).map(|_| r.int(-3, 3)).all(|v| (-3..=3).contains(&v)));
        let s = r.sample(&[1, 2, 3, 4, 5, 6], 4);
        let mut d = s.clone();
        d.sort();
        d.dedup();
        assert_eq!((s.len(), d.len()), (4, 4));
    }
}
