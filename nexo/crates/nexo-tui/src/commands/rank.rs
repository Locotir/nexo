//! Ranking for the palette: fuzzy matching of a query, the grouped listing and the closest name to a typo.

use super::table::{COMMANDS, GROUPS};

/// One row of the palette: the table index, the alias the query matched, if
/// any, and the letters it matched (character positions in the alias when
/// one matched, else in the name), for the palette to light them up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub index: usize,
    pub via_alias: Option<&'static str>,
    pub hits: Vec<usize>,
}

/// The commands `query` can mean, best first: a prefix of a primary name,
/// then a prefix of an alias, then a fuzzy match of the name, then of an
/// alias; ties keep the table's order. An empty query lists everything, by group.
pub fn rank(query: &str) -> Vec<Ranked> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return grouped().into_iter().map(|(_, index)| Ranked { index, via_alias: None, hits: Vec::new() }).collect();
    }
    let mut tiers: [Vec<Ranked>; 4] = Default::default();
    for (index, s) in COMMANDS.iter().enumerate() {
        if s.name.starts_with(&q) {
            tiers[0].push(Ranked { index, via_alias: None, hits: hits(&q, s.name) });
        } else if let Some(a) = s.aliases.iter().find(|a| a.starts_with(&q)) {
            tiers[1].push(Ranked { index, via_alias: Some(a), hits: hits(&q, a) });
        } else if subsequence(&q, s.name) {
            tiers[2].push(Ranked { index, via_alias: None, hits: hits(&q, s.name) });
        } else if let Some(a) = s.aliases.iter().find(|a| subsequence(&q, a)) {
            tiers[3].push(Ranked { index, via_alias: Some(a), hits: hits(&q, a) });
        }
    }
    tiers.into_iter().flatten().collect()
}

/// Whether the characters of `q` appear in `s` in order.
fn subsequence(q: &str, s: &str) -> bool {
    let mut chars = s.chars();
    q.chars().all(|c| chars.any(|x| x == c))
}

/// Where the letters of `q` fall in `s`, the first in order: positions in characters.
pub fn hits(q: &str, s: &str) -> Vec<usize> {
    let mut want = q.chars().map(|c| c.to_ascii_lowercase()).peekable();
    let mut out = Vec::new();
    for (i, c) in s.chars().enumerate() {
        match want.peek() {
            None => break,
            Some(&w) if w == c.to_ascii_lowercase() => {
                out.push(i);
                want.next();
            }
            _ => {}
        }
    }
    out
}

/// Every command once, by group: `(group, table index)`. A command no group
/// names (none should be) comes last, under "more".
pub fn grouped() -> Vec<(&'static str, usize)> {
    let mut out: Vec<(&'static str, usize)> = Vec::new();
    for (g, names) in GROUPS {
        for n in names.iter() {
            if let Some(i) = COMMANDS.iter().position(|s| s.name == *n) {
                out.push((g, i));
            }
        }
    }
    for i in 0..COMMANDS.len() {
        if !out.iter().any(|(_, j)| *j == i) {
            out.push(("more", i));
        }
    }
    out
}

/// The command a mistyped name most likely meant: the name or alias at the
/// smallest edit distance, if it is close enough to be a guess worth showing.
pub fn closest(query: &str) -> Option<&'static str> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return None;
    }
    // names before aliases, so a tie goes to the name
    let words = COMMANDS
        .iter()
        .map(|s| (s.name, s.name))
        .chain(COMMANDS.iter().flat_map(|s| s.aliases.iter().map(move |a| (*a, s.name))));
    let mut best: Option<(usize, &'static str)> = None;
    for (word, name) in words {
        let d = distance(&q, word);
        if best.is_none_or(|(b, _)| d < b) {
            best = Some((d, name));
        }
    }
    let limit = (q.chars().count() / 3).max(1) + 1;
    best.filter(|(d, _)| *d <= limit).map(|(_, n)| n)
}

/// Edit distance in characters, a swap of two neighbours counting as one edit.
pub(super) fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}
