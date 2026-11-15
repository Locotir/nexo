//! Ranking the items of a palette against its query.

use super::*;

impl Palette {
    pub(super) fn refilter(&mut self) {
        self.aliases.clear();
        self.hits.clear();
        self.groups.clear();
        match &self.kind {
            PaletteKind::Commands => {
                let blank = self.query.trim().is_empty();
                let mut ranked = Vec::new();
                if blank {
                    let recent: Vec<usize> = self
                        .recent
                        .iter()
                        .filter_map(|n| self.items.iter().position(|i| i == n))
                        .take(MAX_RECENT)
                        .collect();
                    for (k, i) in recent.into_iter().enumerate() {
                        ranked.push(i);
                        self.aliases.push(None);
                        self.hits.push(Vec::new());
                        self.groups.push((k == 0).then(|| "recent".to_string()));
                    }
                    let mut last = "";
                    for (g, i) in commands::grouped() {
                        // a command listed under "recent" is not listed again below
                        if ranked.iter().take(self.recent.len().min(MAX_RECENT)).any(|r| *r == i) {
                            continue;
                        }
                        ranked.push(i);
                        self.aliases.push(None);
                        self.hits.push(Vec::new());
                        self.groups.push((g != last).then(|| g.to_string()));
                        last = g;
                    }
                } else {
                    for r in commands::rank(&self.query) {
                        ranked.push(r.index);
                        self.aliases.push(r.via_alias);
                        self.hits.push(r.hits);
                        self.groups.push(None);
                    }
                }
                let names: Vec<String> = self.custom.iter().map(|(n, _)| n.clone()).collect();
                for (k, (i, h)) in fuzzy::rank_hits(&self.query, &names).into_iter().enumerate() {
                    ranked.push(COMMANDS.len() + i);
                    self.aliases.push(None);
                    self.hits.push(h);
                    self.groups.push((blank && k == 0).then(|| "your commands".to_string()));
                }
                self.ranked = ranked;
            }
            PaletteKind::Files => {
                let mut r = fuzzy::rank_path_hits(&self.query, &self.items);
                r.truncate(200);
                (self.ranked, self.hits) = r.into_iter().unzip();
                self.groups = vec![None; self.ranked.len()];
            }
            // sessions are found by id or by title; only the id's letters light up
            PaletteKind::Args(ref c) if c == "resume" => {
                let hay: Vec<String> = self.items.iter().zip(&self.descs).map(|(i, d)| format!("{i} {d}")).collect();
                let items = &self.items;
                (self.ranked, self.hits) = rank_words(&self.query, &hay)
                    .into_iter()
                    .map(|(k, h)| {
                        let n = items[k].chars().count();
                        (k, h.into_iter().filter(|&x| x < n).collect())
                    })
                    .unzip();
                self.groups = vec![None; self.ranked.len()];
            }
            PaletteKind::Args(_) => {
                (self.ranked, self.hits) = rank_words(&self.query, &self.items).into_iter().unzip();
                self.groups = vec![None; self.ranked.len()];
            }
        }
        self.aliases.resize(self.ranked.len(), None);
        self.selected = 0;
        self.top = 0;
    }
}

/// Values best first: a prefix, then the letters in order; ties keep the given order.
pub(super) fn rank_words(query: &str, items: &[String]) -> Vec<(usize, Vec<usize>)> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return (0..items.len()).map(|i| (i, Vec::new())).collect();
    }
    let mut prefix = Vec::new();
    let mut loose = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let low = it.to_ascii_lowercase();
        let h = commands::hits(&q, it);
        if low.starts_with(&q) {
            prefix.push((i, h));
        } else if h.len() == q.chars().count() {
            loose.push((i, h));
        }
    }
    prefix.extend(loose);
    prefix
}
