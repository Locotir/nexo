//! Read-only, bounded relational lookup over caller-supplied session text.
use crate::{facts::keys, index::Index};

/// A source line selected by Race's experimental shared-key traversal. This is not proof
/// of causality, currency, or inclusion in a model request.
#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    pub source: usize,
    pub line: usize,
    pub quote: String,
}

/// Build a fresh index on explicit submission, never on each keystroke. The source indices
/// correspond to the input slice. Sampling and traversal limits are those of the live index.
pub fn search(sources: &[&str], query: &str) -> Vec<Hit> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let mut index = Index::default();
    for (i, text) in sources.iter().enumerate() {
        index.add_relation(i, text);
    }
    index.want_relation(sources.len(), query);
    index
        .relation_pick(
            |p| {
                let mut out = Vec::new();
                if let Some(line) = sources.get(p.0 as usize).and_then(|s| index.relation_line(p, s)) {
                    keys(line, &mut out);
                }
                out
            },
            |_| true,
        )
        .into_iter()
        .take(crate::index::REL_OUT)
        .filter_map(|p| {
            Some(Hit {
                source: p.0 as usize,
                line: p.1 as usize + 1,
                quote: index.relation_line(p, sources.get(p.0 as usize)?)?.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_original_positions_and_is_read_only() {
        let sources = ["alpha 4872 fixed", "alpha 4872 broken", "unrelated filler"];
        let hits = search(&sources, "alpha 4872");
        assert!(hits.iter().any(|h| h.source == 0 && h.line == 1 && h.quote == sources[0]));
        assert!(hits.iter().any(|h| h.source == 1 && h.quote == sources[1]));
        assert!(search(&sources, "  ").is_empty());
    }
}
