//! Experimental local segmentation, derived on demand rather than persisted. A segment never crosses
//! a tool-result unit: only a uniquely paired file read supplies an explicit scope. This deliberately
//! does not infer project identity from keywords, group parallel results, or assert currentness.
use serde_json::Value;

use super::{original_line, paired};
use crate::hindsight::{UKind, Unit};
use crate::index::{Index, Post};
use crate::msg::get_str;
use crate::sorted::FxHashSet;
use crate::tools::{classify, Kind};

const SEEDS: usize = 8;
const EXTRA: usize = 12;
const RADIUS: u32 = 2;

/// Only admitted original picks may anchor context. Exclude every selector pick, including those
/// that did not fit: expansion must not turn a rejected original into an unanchored extra.
pub(super) fn expand(index: &Index, history: &[Value], units: &[Unit], admitted: &[Post], picks: &[Post]) -> Vec<Post> {
    let mut seen: FxHashSet<Post> = picks.iter().copied().collect();
    let mut extras = Vec::new();
    for &p in admitted.iter().take(SEEDS) {
        let Some(u) = units.get(p.0 as usize) else { continue };
        if u.kind != UKind::Result {
            continue;
        }
        let Some((_, _, _, call)) = paired(history, u) else { continue };
        let (kind, path, _) = classify(get_str(call, "name"), call.get("input"));
        if kind != Kind::Read || path.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        // Stop at an empty, non-indexable, or missing line. Never walk transitively from an extra.
        for forward in [false, true] {
            for d in 1..=RADIUS {
                let n = if forward { p.1.checked_add(d) } else { p.1.checked_sub(d) };
                let Some(n) = n else { break };
                let q = (p.0, n);
                let Some(line) = original_line(index, history, units, q) else { break };
                if line.trim().is_empty() {
                    break;
                }
                if seen.insert(q) {
                    extras.push(q);
                }
                if extras.len() >= EXTRA {
                    break;
                }
            }
            if extras.len() >= EXTRA {
                break;
            }
        }
        if extras.len() >= EXTRA {
            break;
        }
    }
    extras
}
