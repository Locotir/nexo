//! The lexical selection: the same shape as the curator's without its model.

use super::*;

pub(super) struct Cand {
    pub(super) unit: usize,
    pub(super) msg: usize,
    pub(super) start: usize,
    pub(super) hold: BTreeSet<String>,
    pub(super) factor: f64,
    pub(super) text: String,
    pub(super) tokens: usize,
}

/// The excerpts of `history` that bear on `task`, within `budget` tokens, in
/// conversation order: the curator's selection without its model. A turn
/// (from one request of the person to the next) is as relevant as the share of
/// the task's word weight its words hold, the latest one a little more; a
/// paragraph is worth the task words it holds (IDF-weighted, names double) times
/// that relevance; reads a later write made stale are left out; long paragraphs
/// keep the lines around the task's words; the pick is a greedy coverage.
pub fn lexical(task: &str, history: &[Message], budget: usize) -> Vec<Span> {
    lexical_but(task, history, budget, None)
}

/// Like [`lexical`], with the person's words of message `skip` left out of the excerpts.
pub(super) fn lexical_but(task: &str, history: &[Message], budget: usize, skip: Option<usize>) -> Vec<Span> {
    let us = units(history);
    let q = terms(task);
    let q_names = names(task);
    if q.is_empty() || us.is_empty() {
        return vec![];
    }
    // the turns
    let mut turn_of = vec![0usize; history.len()];
    let mut turn = 0usize;
    let mut started = false;
    let mut by_msg_request = vec![false; history.len()];
    for u in &us {
        if u.kind == Kind::Person {
            by_msg_request[u.msg] = true;
        }
    }
    for (mi, slot) in turn_of.iter_mut().enumerate() {
        if by_msg_request[mi] {
            if started {
                turn += 1;
            }
            started = true;
        }
        *slot = turn;
    }
    let last_turn = turn;
    // writes per path, for stale reads: those whose result did not fail
    let mut writes: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for u in &us {
        if u.kind == Kind::Call && !u.error && writes_file(&u.tool) {
            if let Some(p) = &u.path {
                writes.entry(p.as_str()).or_default().push(u.msg);
            }
        }
    }
    // candidates
    struct Raw<'a> {
        unit: usize,
        lines: Vec<&'a str>,
        start: usize,
        words: BTreeSet<String>,
    }
    let mut raws: Vec<Raw> = vec![];
    for (k, u) in us.iter().enumerate() {
        if u.text.trim().is_empty()
            || u.tool == "task"
            || (u.kind == Kind::Person && Some(u.msg) == skip)
            || (u.kind == Kind::Call && tokens(&u.text) < 40)
        {
            continue;
        }
        if u.kind == Kind::Result && u.tool == "read" {
            if let Some(p) = &u.path {
                if writes.get(p.as_str()).is_some_and(|w| w.iter().any(|&m| m > u.msg)) {
                    continue;
                }
            }
        }
        let lines: Vec<&str> = u.text.split('\n').collect();
        let parts = if u.kind == Kind::Result { paragraphs(&lines) } else { vec![(0, lines.len())] };
        for (a, b) in parts {
            let chunk = &lines[a..b];
            let words = terms(&chunk.join("\n"));
            raws.push(Raw { unit: k, lines: chunk.to_vec(), start: a, words });
        }
    }
    if raws.is_empty() {
        return vec![];
    }
    let n = raws.len() as f64;
    let mut df: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &raws {
        for w in &r.words {
            *df.entry(w.as_str()).or_default() += 1;
        }
    }
    let mut idf: BTreeMap<String, f64> = BTreeMap::new();
    for w in &q {
        let mut v = (1.0 + n / (1.0 + *df.get(w.as_str()).unwrap_or(&0) as f64)).ln();
        if q_names.contains(w) {
            v *= 2.0;
        }
        idf.insert(w.clone(), v);
    }
    let total: f64 = idf.values().sum::<f64>().max(1e-9);
    // the relevance of each turn
    let mut turn_words: Vec<BTreeSet<&str>> = vec![BTreeSet::new(); last_turn + 1];
    for r in &raws {
        let t = turn_of[us[r.unit].msg];
        turn_words[t].extend(r.words.iter().map(String::as_str));
    }
    let rel: Vec<f64> = turn_words
        .iter()
        .enumerate()
        .map(|(t, ws)| {
            let s: f64 = idf.iter().filter(|(w, _)| ws.contains(w.as_str())).map(|(_, v)| v).sum();
            (s / total + if t == last_turn { 0.25 } else { 0.0 }).min(1.0)
        })
        .collect();
    let mut cands: Vec<Cand> = vec![];
    for r in &raws {
        let u = &us[r.unit];
        let hold: BTreeSet<String> = r.words.iter().filter(|w| idf.contains_key(*w)).cloned().collect();
        if hold.is_empty() {
            continue;
        }
        let weight: f64 = hold.iter().map(|w| idf[w]).sum();
        let named = hold.iter().any(|w| q_names.contains(w));
        if weight / total < MIN_SHARE && !named {
            continue;
        }
        let lines = if r.lines.len() > LONG { trim(&r.lines, &idf) } else { r.lines.clone() };
        let text = lines.join("\n").trim_matches('\n').to_string();
        if text.trim().is_empty() {
            continue;
        }
        let factor = (0.35 + 0.65 * rel[turn_of[u.msg]]) * if u.error && u.kind != Kind::Call { 1.15 } else { 1.0 };
        cands.push(Cand { unit: r.unit, msg: u.msg, start: r.start, hold, factor, tokens: tokens(&text), text });
    }
    // greedy weighted coverage
    let mut picked: Vec<Cand> = vec![];
    let mut covered: BTreeSet<String> = BTreeSet::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut left = budget;
    while !cands.is_empty() && picked.len() < MAX_SPANS {
        let mut best: Option<(f64, usize)> = None;
        for (k, c) in cands.iter().enumerate() {
            if c.tokens > left {
                continue;
            }
            let gain: f64 = c.hold.iter().map(|w| if covered.contains(w) { 0.25 * idf[w] } else { idf[w] }).sum();
            let worth = gain * c.factor / (c.tokens as f64).sqrt();
            // ties: the later passage (it is the fresher one)
            let better = match best {
                None => true,
                Some((bw, bk)) => worth > bw || (worth == bw && (c.msg, c.start) > (cands[bk].msg, cands[bk].start)),
            };
            if better {
                best = Some((worth, k));
            }
        }
        let Some((worth, k)) = best else { break };
        if worth <= 0.0 {
            break;
        }
        let c = cands.remove(k);
        if !seen.insert(c.text.clone()) {
            continue;
        }
        covered.extend(c.hold.iter().cloned());
        left -= c.tokens;
        picked.push(c);
    }
    picked.sort_by_key(|c| (c.msg, c.unit, c.start));
    picked.into_iter().map(|c| Span { msg: c.msg, label: label(&us[c.unit]), text: c.text, tokens: c.tokens }).collect()
}

/// The lines around those holding at least half the task word weight of the
/// best line (a word every line holds singles none out); gaps marked `…`.
pub(super) fn trim<'a>(lines: &[&'a str], idf: &BTreeMap<String, f64>) -> Vec<&'a str> {
    let weight: Vec<f64> = lines
        .iter()
        .map(|l| {
            let body = numbered(l).map(|(_, r)| r).unwrap_or(l);
            terms(body).iter().filter_map(|w| idf.get(w)).sum()
        })
        .collect();
    let top = weight.iter().cloned().fold(0.0, f64::max);
    if top <= 0.0 {
        return lines[..LONG.min(lines.len())].to_vec();
    }
    let mut hit: Vec<usize> = (0..lines.len()).filter(|&k| weight[k] >= 0.5 * top).collect();
    if hit.len() > LONG {
        let mut order: Vec<usize> = (0..lines.len()).collect();
        order.sort_by(|&a, &b| weight[b].total_cmp(&weight[a]).then(a.cmp(&b)));
        hit = order.into_iter().take(LONG / 2).collect();
        hit.sort_unstable();
    }
    let keep: BTreeSet<usize> =
        hit.iter().flat_map(|&k| k.saturating_sub(AROUND)..(k + AROUND + 1).min(lines.len())).collect();
    let mut out = vec![];
    let mut prev: Option<usize> = None;
    for j in keep {
        if prev.is_some_and(|p| j != p + 1) {
            out.push("…");
        }
        out.push(lines[j]);
        prev = Some(j);
    }
    out
}
