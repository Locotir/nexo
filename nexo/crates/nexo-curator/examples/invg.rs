//! Measurements for questions G4 and G16, on the corpus (`examples/corpus.rs`), aggregates only.
//!
//! `invg g4 FILE...`: how often what a closed step read is needed again, and what a smaller trace would cost.
//! A step is closed when the person has written a later request. A word of its output is needed when a later
//! reply or call of the agent says it, no earlier text, request or call holds it, and no output of the open
//! request does. A trace is kept for each closed step from its close to the end of the session.
//!
//! `invg g16 [--link X] FILE...`: precision and recall of the topic split against two kinds of labels derived
//! from the structure of the corpus: spliced sessions (the first turns of one session followed by the first
//! turns of another one from a different project: the junction is a new topic) and requests that open with an
//! explicit marker of a new task or of a continuation.

use std::collections::{HashMap, HashSet};

use nexo_curator::{distinctive_words, Curator};
use serde_json::Value;

const WORD_USD: f64 = 0.00035;
const MISS: f64 = 0.28;
const HIT: f64 = 0.028;

fn load(path: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut msgs: Vec<Value> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(mut rec) = serde_json::from_str::<Value>(line) else { continue };
        let base = rec["req"]["base"].as_u64().unwrap_or(0) as usize;
        msgs.truncate(base);
        if let Value::Array(a) = rec["req"]["new"].take() {
            msgs.extend(a);
        }
    }
    msgs
}

fn blocks(m: &Value) -> Vec<Value> {
    match &m["content"] {
        Value::String(s) => vec![serde_json::json!({"type": "text", "text": s})],
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

fn result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn input_text(b: &Value) -> String {
    match &b["input"] {
        Value::Object(m) => {
            m.values().map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string)).collect::<Vec<_>>().join(" ")
        }
        o => o.to_string(),
    }
}

const NOT_REQUEST: [&str; 5] = ["<system-reminder>", "<local-command", "<command-", "Caveat:", "[Request interrupted"];

/// The person's own words in a user message, if any.
fn request_text(m: &Value) -> Option<String> {
    if m["role"] != "user" {
        return None;
    }
    let t: Vec<String> = blocks(m)
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str().map(str::to_string))
        .filter(|s| !s.trim().is_empty() && !NOT_REQUEST.iter().any(|p| s.trim_start().starts_with(p)))
        .collect();
    (!t.is_empty()).then(|| t.join("\n"))
}

struct Step {
    turn: usize,
    words: HashSet<String>,
    head_tail: String,
    chars: usize,
}

fn cut(s: &str, n: usize, tail: bool) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= n {
        return s.to_string();
    }
    if tail {
        c[c.len() - n..].iter().collect()
    } else {
        c[..n].iter().collect()
    }
}

#[derive(Default)]
struct G4 {
    sessions: usize,
    sessions_multi: usize,
    steps: usize,
    steps_closed: usize,
    closed_tokens: f64,
    needed_steps: usize,
    needed_words: usize,
    word_events: usize,
    dist: [usize; 5],
    keep_all_usd: f64,
    // per variant: covered words, trace tokens, trace usd
    var: Vec<(&'static str, usize, f64, f64)>,
    // coverage when the trace expires after K turns: (K, words covered by whole result kept for K turns)
    expire: Vec<(usize, usize, f64)>,
}

fn g4_session(msgs: &[Value], g: &mut G4) {
    let mut turn_of = vec![0usize; msgs.len()];
    let mut t = 0usize;
    let mut started = false;
    for (i, m) in msgs.iter().enumerate() {
        if request_text(m).is_some() {
            if started {
                t += 1;
            }
            started = true;
        }
        turn_of[i] = t;
    }
    let last_turn = t;
    g.sessions += 1;
    if last_turn == 0 {
        return;
    }
    g.sessions_multi += 1;
    let mut calls: HashMap<String, usize> = HashMap::new();
    let mut steps: Vec<Step> = Vec::new();
    let mut step_msg: Vec<usize> = Vec::new();
    let mut first_non: HashMap<String, usize> = HashMap::new();
    let mut holders: HashMap<String, Vec<usize>> = HashMap::new();
    // events: (message index, word) of agent words
    let mut agent_words: Vec<(usize, Vec<String>)> = Vec::new();
    let mut df: HashMap<String, usize> = HashMap::new();
    for (i, m) in msgs.iter().enumerate() {
        let mut nonres = String::new();
        for b in blocks(m) {
            match b["type"].as_str() {
                Some("text") => {
                    nonres.push_str(b["text"].as_str().unwrap_or(""));
                    nonres.push('\n');
                }
                Some("tool_use") => {
                    nonres.push_str(&input_text(&b));
                    nonres.push('\n');
                    if let Some(id) = b["id"].as_str() {
                        calls.insert(id.to_string(), i);
                    }
                }
                Some("tool_result") => {
                    let id = b["tool_use_id"].as_str().unwrap_or("");
                    if !calls.contains_key(id) {
                        continue;
                    }
                    let text = result_text(&b);
                    let words: HashSet<String> = distinctive_words(&text).into_iter().collect();
                    let s = steps.len();
                    for w in &words {
                        holders.entry(w.clone()).or_default().push(s);
                        *df.entry(w.clone()).or_insert(0) += 1;
                    }
                    let ht = format!("{}\n{}", cut(&text, 160, false), cut(&text, 160, true));
                    steps.push(Step { turn: turn_of[i], words, head_tail: ht, chars: text.len() });
                    step_msg.push(i);
                }
                _ => {}
            }
        }
        let ws = distinctive_words(&nonres);
        for w in &ws {
            first_non.entry(w.clone()).or_insert(i);
        }
        if m["role"] == "assistant" {
            agent_words.push((i, ws));
        }
    }
    let n_requests = agent_words.len();
    let requests_after = |turn: usize| -> usize { agent_words.iter().filter(|(i, _)| turn_of[*i] > turn).count() };
    // needs
    let mut need: HashMap<usize, Vec<(String, usize)>> = HashMap::new(); // step -> (word, turn distance)
    for (mi, ws) in &agent_words {
        for w in ws {
            if first_non.get(w) != Some(mi) {
                continue;
            }
            g.word_events += 1;
            let Some(h) = holders.get(w) else { continue };
            let before: Vec<usize> = h.iter().copied().filter(|&s| step_msg[s] < *mi).collect();
            let Some(&latest) = before.last() else { continue };
            if before.iter().any(|&s| steps[s].turn == turn_of[*mi]) {
                continue;
            }
            need.entry(latest).or_default().push((w.clone(), turn_of[*mi] - steps[latest].turn));
        }
    }
    let _ = n_requests;
    let mut var: Vec<(&'static str, usize, f64, f64)> = vec![
        ("head+tail 160B", 0, 0.0, 0.0),
        ("head+tail+10 rare words", 0, 0.0, 0.0),
        ("head+tail+30 rare words", 0, 0.0, 0.0),
    ];
    for (si, s) in steps.iter().enumerate() {
        g.steps += 1;
        if s.turn >= last_turn {
            continue;
        }
        g.steps_closed += 1;
        let rem = requests_after(s.turn) as f64;
        let tok = s.chars as f64 / 4.0;
        g.closed_tokens += tok;
        g.keep_all_usd += tok * (MISS + HIT * rem) / 1e6;
        let ev = need.get(&si);
        if let Some(e) = ev {
            g.needed_steps += 1;
            g.needed_words += e.len();
            for (_, d) in e {
                let k = match d {
                    1 => 0,
                    2 => 1,
                    3..=5 => 2,
                    6..=10 => 3,
                    _ => 4,
                };
                g.dist[k] += 1;
            }
        }
        let mut rare: Vec<&String> = s.words.iter().collect();
        rare.sort_by_key(|w| (df.get(*w).copied().unwrap_or(0), std::cmp::Reverse(w.len()), (*w).clone()));
        let base_set: HashSet<String> = distinctive_words(&s.head_tail).into_iter().collect();
        for (vi, k) in [0usize, 10, 30].into_iter().enumerate() {
            let mut set = base_set.clone();
            let mut chars = s.head_tail.len();
            for w in rare.iter().take(k) {
                set.insert((*w).clone());
                chars += w.len() + 1;
            }
            let ttok = chars as f64 / 4.0;
            let cov = ev.map_or(0, |e| e.iter().filter(|(w, _)| set.contains(w)).count());
            var[vi].1 += cov;
            var[vi].2 += ttok;
            var[vi].3 += ttok * (MISS + HIT * rem) / 1e6;
        }
        if g.expire.is_empty() {
            g.expire = [1usize, 2, 5, 10].iter().map(|&k| (k, 0, 0.0)).collect();
        }
        for e in g.expire.iter_mut() {
            let covered = ev.map_or(0, |ev| ev.iter().filter(|(_, d)| *d <= e.0).count());
            e.1 += covered;
            let rem_k = agent_words.iter().filter(|(i, _)| turn_of[*i] > s.turn && turn_of[*i] <= s.turn + e.0).count();
            e.2 += tok * (MISS + HIT * rem_k as f64) / 1e6;
        }
    }
    if g.var.is_empty() {
        g.var = var;
    } else {
        for (a, b) in g.var.iter_mut().zip(var) {
            a.1 += b.1;
            a.2 += b.2;
            a.3 += b.3;
        }
    }
}

fn g4(files: &[String]) {
    let mut g = G4::default();
    for f in files {
        let msgs = load(f);
        g4_session(&msgs, &mut g);
    }
    println!("G4 sessions {} (with 2+ requests {})", g.sessions, g.sessions_multi);
    println!(
        "closed steps {} of {}; their output {:.0} tokens; keeping all of it to the end costs {:.2} $",
        g.steps_closed, g.steps, g.closed_tokens, g.keep_all_usd
    );
    println!(
        "agent words with no other holder: {}; needed again from a closed step: {} words in {} steps ({:.1} % of closed steps)",
        g.word_events,
        g.needed_words,
        g.needed_steps,
        100.0 * g.needed_steps as f64 / g.steps_closed.max(1) as f64
    );
    println!(
        "turns until first need (1, 2, 3-5, 6-10, 11+): {:?}; cost of losing them all at {WORD_USD} $ a word: {:.2} $",
        g.dist,
        g.needed_words as f64 * WORD_USD
    );
    for (name, cov, tok, usd) in &g.var {
        let gain = *cov as f64 * WORD_USD;
        println!(
            "trace {name}: covers {cov} of {} needed words ({:.1} %), {:.0} tokens, cost {:.2} $, saves {:.2} $, net {:+.2} $",
            g.needed_words,
            100.0 * *cov as f64 / g.needed_words.max(1) as f64,
            tok,
            usd,
            gain,
            gain - usd
        );
    }
    for (k, cov, usd) in &g.expire {
        let gain = *cov as f64 * WORD_USD;
        println!(
            "whole output kept {k} turns after close: covers {cov} words ({:.1} %), cost {:.2} $, saves {:.2} $, net {:+.2} $",
            100.0 * *cov as f64 / g.needed_words.max(1) as f64,
            usd,
            gain,
            gain - usd
        );
    }
}

const NEW_MARKERS: [&str; 14] = [
    "new task",
    "nueva tarea",
    "different task",
    "unrelated",
    "separate question",
    "switching to",
    "on another note",
    "otra cosa",
    "cambiando de tema",
    "ahora vamos con otra",
    "new topic",
    "nuevo tema",
    "next task",
    "let's move on to",
];
const CONT_MARKERS: [&str; 14] = [
    "also ",
    "and also",
    "continue",
    "sigue",
    "tambien",
    "también",
    "keep going",
    "that didn't work",
    "ese error",
    "still ",
    "same file",
    "mismo",
    "now fix",
    "now make it",
];

#[derive(Default, Clone, Copy)]
struct Conf {
    tp: usize,
    fp: usize,
    fnn: usize,
    tn: usize,
}

impl Conf {
    fn add(&mut self, truth: bool, said: bool) {
        match (truth, said) {
            (true, true) => self.tp += 1,
            (false, true) => self.fp += 1,
            (true, false) => self.fnn += 1,
            (false, false) => self.tn += 1,
        }
    }
    fn show(&self, name: &str) {
        let p = self.tp as f64 / (self.tp + self.fp).max(1) as f64;
        let r = self.tp as f64 / (self.tp + self.fnn).max(1) as f64;
        println!(
            "{name}: n={} tp={} fp={} fn={} tn={} precision {:.1} % recall {:.1} % f1 {:.1} %",
            self.tp + self.fp + self.fnn + self.tn,
            self.tp,
            self.fp,
            self.fnn,
            self.tn,
            100.0 * p,
            100.0 * r,
            100.0 * 2.0 * p * r / (p + r).max(1e-9)
        );
    }
}

/// The turns of a session: (first message, the person's words).
fn turns(msgs: &[Value]) -> Vec<(usize, String)> {
    msgs.iter().enumerate().filter_map(|(i, m)| request_text(m).map(|t| (i, t))).collect()
}

fn cwd_of(msgs: &[Value]) -> String {
    for m in msgs.iter().take(3) {
        for b in blocks(m) {
            if let Some(t) = b["text"].as_str() {
                if let Some(p) = t.find("Primary working directory: ") {
                    return t[p + 27..].lines().next().unwrap_or("").to_string();
                }
            }
        }
    }
    String::new()
}

fn g16(files: &[String], link: Option<f64>) {
    let spec = link.map_or(String::new(), |l| format!("link={l}"));
    let mut splice = Conf::default();
    let mut within = Conf::default();
    let mut marker_new = Conf::default();
    let mut marker_cont = Conf::default();
    let (mut n_sessions, mut n_pairs) = (0, 0);
    let sessions: Vec<Vec<Value>> = files.iter().map(|f| load(f)).collect();
    let cwds: Vec<String> = sessions.iter().map(|s| cwd_of(s)).collect();
    // (a) splice: A first K turns + B first K turns, different cwd or (when unknown) different file
    const K: usize = 2;
    let cut_at = |msgs: &[Value]| -> Option<usize> {
        let ts = turns(msgs);
        (ts.len() >= K).then(|| ts.get(K).map_or(msgs.len(), |t| t.0))
    };
    for (a, shift) in (0..sessions.len()).flat_map(|a| [3usize, 101, 211].map(move |s| (a, s))) {
        let b = (a * 7 + shift) % sessions.len();
        if a == b || (!cwds[a].is_empty() && cwds[a] == cwds[b]) {
            continue;
        }
        let (Some(ca), Some(cb)) = (cut_at(&sessions[a]), cut_at(&sessions[b])) else { continue };
        // the second session's first turn follows the first's: its tool ids may clash, so prefix them
        let mut msgs: Vec<Value> = sessions[a][..ca].to_vec();
        let junction_msg = msgs.len();
        let mut extra: Vec<Value> = sessions[b][..cb].to_vec();
        for m in extra.iter_mut() {
            if let Value::Array(bl) = &mut m["content"] {
                for x in bl.iter_mut() {
                    for k in ["id", "tool_use_id"] {
                        if let Some(s) = x[k].as_str().map(|s| format!("b{s}")) {
                            x[k] = Value::String(s);
                        }
                    }
                }
            }
        }
        msgs.extend(extra);
        // alternate roles must hold: if the join puts two user messages together the provider joins them, skip
        let mut cur = Curator::new(&spec).expect("spec");
        let th = cur.threads(&msgs);
        n_pairs += 1;
        let mut max_before = 0;
        for (i, &(start, thread)) in th.iter().enumerate() {
            if start < junction_msg {
                max_before = max_before.max(thread);
                continue;
            }
            let first_after = start == junction_msg || th[i - 1].0 < junction_msg;
            if first_after {
                // new topic if it opens a thread unseen before the junction
                splice.add(true, thread > max_before);
            } else {
                // inside the second session: truth continue (same session); said new = opens an unseen thread
                let seen_after = th[..i].iter().filter(|x| x.0 >= junction_msg).any(|x| x.1 == thread);
                let opened_before = th[..i].iter().any(|x| x.1 == thread);
                let _ = seen_after;
                within.add(false, !opened_before);
            }
        }
    }
    // (b) markers inside real sessions
    for msgs in &sessions {
        let ts = turns(msgs);
        if ts.len() < 2 {
            continue;
        }
        n_sessions += 1;
        let mut cur = Curator::new(&spec).expect("spec");
        let th = cur.threads(msgs);
        let mut opened: HashSet<usize> = HashSet::new();
        for (i, &(start, thread)) in th.iter().enumerate() {
            let new = !opened.contains(&thread);
            opened.insert(thread);
            if i == 0 {
                continue;
            }
            let Some((_, text)) = ts.iter().find(|t| t.0 == start) else { continue };
            let low = text.to_lowercase();
            let head: String = low.chars().take(160).collect();
            if NEW_MARKERS.iter().any(|m| head.contains(m)) {
                marker_new.add(true, new);
            } else if CONT_MARKERS.iter().any(|m| head.starts_with(m)) {
                marker_cont.add(false, new);
            }
        }
    }
    println!("G16 link={:?}: sessions {n_sessions}, splices {n_pairs}", link);
    splice.show("splice junction (truth: new topic)");
    within.show("splice, turns after the junction inside session B (truth: no new topic; tp/fn n/a)");
    marker_new.show("explicit new-task marker (truth: new topic; only recall means anything)");
    marker_cont.show("explicit continuation opener (truth: not new; fp = false split)");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().cloned().unwrap_or_default();
    let mut link = None;
    let mut files = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--dir" {
            let d = it.next().cloned().unwrap_or_default();
            let every: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
            let mut names: Vec<String> = std::fs::read_dir(&d)
                .map(|r| {
                    r.flatten()
                        .map(|e| e.path().to_string_lossy().to_string())
                        .filter(|p| p.ends_with(".jsonl"))
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            files.extend(names.into_iter().step_by(every.max(1)));
        } else if a == "--link" {
            link = it.next().and_then(|v| v.parse().ok());
        } else {
            files.push(a.clone());
        }
    }
    match mode.as_str() {
        "g4" => g4(&files),
        "g16" => g16(&files, link),
        _ => eprintln!("usage: invg g4|g16 [--link X] FILE..."),
    }
}
