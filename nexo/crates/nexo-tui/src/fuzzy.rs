//! Fuzzy ranking for the palette and the pickers.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Indices of `items` matching `query`, best first (all, in order, when the query is empty).
pub fn rank(query: &str, items: &[String]) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| pattern.score(Utf32Str::new(s, &mut buf), &mut matcher).map(|sc| (sc, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Like [`rank`], with the character positions of `items[i]` the query matched, in order.
pub fn rank_hits(query: &str, items: &[String]) -> Vec<(usize, Vec<usize>)> {
    hits_by(query, items, false)
}

/// Like [`rank_hits`] for paths: of two that match as well, the shorter comes first, so `a`
/// offers `a.txt` before `AGENTS.md`.
pub fn rank_path_hits(query: &str, items: &[String]) -> Vec<(usize, Vec<usize>)> {
    hits_by(query, items, true)
}

fn hits_by(query: &str, items: &[String], shorter_first: bool) -> Vec<(usize, Vec<usize>)> {
    let query = query.trim();
    if query.is_empty() {
        return (0..items.len()).map(|i| (i, Vec::new())).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut idx = Vec::new();
    let mut scored: Vec<(u32, usize, Vec<usize>)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            idx.clear();
            let sc = pattern.indices(Utf32Str::new(s, &mut buf), &mut matcher, &mut idx)?;
            let mut hits: Vec<usize> = idx.iter().map(|&x| x as usize).collect();
            hits.sort_unstable();
            hits.dedup();
            Some((sc, i, hits))
        })
        .collect();
    let len = |i: usize| if shorter_first { items[i].chars().count() } else { 0 };
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(len(a.1).cmp(&len(b.1))).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i, h)| (i, h)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_the_closest_first_and_drops_non_matches() {
        let items: Vec<String> =
            ["model", "models", "compact", "sessions", "cost"].iter().map(|s| s.to_string()).collect();
        let r = rank("mod", &items);
        assert_eq!(r[0], 0);
        assert!(r.contains(&1));
        assert!(!r.contains(&2));
        assert_eq!(rank("", &items), vec![0, 1, 2, 3, 4]);
        let r = rank("cst", &items);
        assert_eq!(r[0], 4, "cost before compact for 'cst'");
    }

    #[test]
    fn says_which_letters_matched() {
        let items: Vec<String> = ["src/main.rs", "README.md"].iter().map(|s| s.to_string()).collect();
        let r = rank_hits("mai", &items);
        assert_eq!(r[0].0, 0);
        assert_eq!(r[0].1, vec![4, 5, 6]);
        assert_eq!(rank_hits("", &items), vec![(0, vec![]), (1, vec![])]);
    }

    #[test]
    fn of_two_paths_that_match_as_well_the_shorter_comes_first() {
        let items: Vec<String> = ["AGENTS.md", "a.txt", "src/app.rs"].iter().map(|s| s.to_string()).collect();
        let r: Vec<usize> = rank_path_hits("a", &items).into_iter().map(|(i, _)| i).collect();
        assert_eq!(r[0], 1, "a.txt before AGENTS.md for 'a': {r:?}");
        let r: Vec<usize> = rank_hits("a", &items).into_iter().map(|(i, _)| i).collect();
        assert_eq!(r[0], 0, "other lists keep their order on a tie");
    }
}
