//! Shared by `examples/scale.rs` and `tests/scale.rs`: synthetic sessions of four shapes, fed to the curator
//! one request at a time as the host does, and the numbers measured on the way.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::Instant;

use nexo_curator::Curator;
use serde_json::{json, Value};

/// The four shapes: `e8` (the shape of examples/bench.rs, one topic per 25 steps), `a` (a text-only assistant
/// reply before each request), `b` (topics that return every 6 steps), `c` (a chat without tools).
pub const KINDS: [&str; 4] = ["e8", "a", "b", "c"];

pub struct Gen {
    kind: &'static str,
    pub msgs: Vec<Value>,
    pending: VecDeque<Value>,
    cycle: usize,
    rng: u64,
    /// User messages so far (each one is a request to the model).
    pub requests: usize,
}

const WORDS: [&str; 24] = [
    "invoice", "ledger", "cache", "latency", "schema", "report", "budget", "client", "export", "queue", "index",
    "backup", "render", "policy", "deploy", "metric", "tariff", "survey", "ticket", "vendor", "season", "recipe",
    "garden", "route",
];

impl Gen {
    pub fn new(kind: &str) -> Gen {
        let kind = KINDS.iter().copied().find(|k| *k == kind).expect("unknown generator");
        Gen { kind, msgs: Vec::new(), pending: VecDeque::new(), cycle: 0, rng: 0x9E37_79B9_7F4A_7C15, requests: 0 }
    }

    fn rand(&mut self, n: usize) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % n as u64) as usize
    }

    fn sentence(&mut self, topic: usize, n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            let w = if i % 3 == 0 { WORDS[(topic * 5 + i) % WORDS.len()] } else { WORDS[self.rand(WORDS.len())] };
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(w);
        }
        s
    }

    fn step(k: usize, m: usize) -> [Value; 2] {
        let f = k % 7;
        let id = format!("c{k}");
        let out: String = (0..6)
            .map(|j| format!("src/m{m}/f{f}.rs:{}: let v_{k}_{j} = load({})?;\n", k % 13 + 11 * j, j * k))
            .collect();
        [
            json!({"role": "assistant", "content": [{"type": "text", "text": format!("Checking step {k}.")},
                {"type": "tool_use", "id": id, "name": "bash", "input": {"command": format!("grep -n err_{k} src/m{m}/f{f}.rs")}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
        ]
    }

    fn fill(&mut self) {
        let c = self.cycle;
        self.cycle += 1;
        let k = c + 1;
        let mut out: Vec<Value> = Vec::new();
        match self.kind {
            "e8" | "a" => {
                if c == 0 {
                    out.push(json!({"role": "user", "content": "Keep the service healthy and fix what breaks."}));
                }
                if c.is_multiple_of(25) {
                    if self.kind == "a" && c > 0 {
                        out.push(json!({"role": "assistant", "content": format!("Module m{} is done: the failing jobs trace back to one loader.", c / 25 - 1)}));
                    }
                    out.push(json!({"role": "user", "content": format!("Now look at module m{}: why does job_{k} fail?", c / 25)}));
                }
                out.extend(Self::step(k, c / 25));
            }
            "b" => {
                let r = c / 3;
                let m = 2 * (r / 40) + r % 2;
                if c.is_multiple_of(3) {
                    if r > 0 {
                        let p = r - 1;
                        out.push(json!({"role": "assistant", "content": format!("That covers what module m{} showed so far.", 2 * (p / 40) + p % 2)}));
                    }
                    out.push(
                        json!({"role": "user", "content": format!("Back to module m{m}: why does job_{k} fail?")}),
                    );
                }
                out.extend(Self::step(k, m));
            }
            _ => {
                let topic = c / 15;
                let q = self.sentence(topic, 12);
                let a = self.sentence(topic, 40);
                out.push(json!({"role": "user", "content": format!("Question {k}: what about {q}?")}));
                out.push(json!({"role": "assistant", "content": format!("On that: {a}.")}));
            }
        }
        self.pending.extend(out);
    }

    /// Adds messages up to and including the next user message; the history is then one request longer.
    pub fn advance(&mut self) {
        loop {
            if self.pending.is_empty() {
                self.fill();
            }
            let m = self.pending.pop_front().expect("filled");
            let user = m["role"] == "user";
            self.msgs.push(m);
            if user {
                self.requests += 1;
                return;
            }
        }
    }
}

pub fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

pub fn pct(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    if s.is_empty() {
        0.0
    } else {
        s[(((s.len() - 1) as f64) * p).round() as usize]
    }
}

pub fn median(v: &[f64]) -> f64 {
    pct(v, 0.5)
}

#[derive(Debug, Clone)]
pub struct Row {
    pub kind: &'static str,
    pub requests: usize,
    pub messages: usize,
    pub median_ms: f64,
    pub p99_ms: f64,
    /// Median of curate time plus serializing the memory reply.
    pub with_reply_ms: f64,
    pub reply_kb: f64,
    pub rss_mb: f64,
    pub ingest_ms_per_1k: f64,
    /// CPU time of this thread per timed request (curate plus reply), from /proc: wall time is inflated by a loaded machine, this much less.
    pub cpu_ms: f64,
    pub view: usize,
}

/// CPU seconds (user plus system) this thread has used, from /proc (10 ms ticks).
pub fn thread_cpu_s() -> f64 {
    let s = std::fs::read_to_string("/proc/thread-self/stat").unwrap_or_default();
    let rest = s.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (tick(11) + tick(12)) / 100.0
}

pub struct Plan {
    pub checkpoints: Vec<usize>,
    /// Requests timed one by one just before each checkpoint.
    pub tail: usize,
    /// Before the tail, once past `dense_until`, only every `stride`-th request calls the curator.
    pub stride: usize,
    pub dense_until: usize,
    /// Also take in the whole history in one call at each checkpoint (ingest ms per 1,000 messages).
    pub ingest: bool,
}

/// One session of one shape. At each checkpoint the last `tail` requests are timed one by one: curate time
/// (`Curator::view`, the `memory` value built inside it, as nexo calls it) and then, on its own, the
/// serialization of the memory reply.
pub fn run(kind: &str, plan: &Plan) -> Vec<Row> {
    let mut g = Gen::new(kind);
    let mut cur = Curator::new("").expect("default spec");
    let mut rows = Vec::new();
    for &cp in &plan.checkpoints {
        let mut curate = Vec::new();
        let mut both = Vec::new();
        let mut cpu0: Option<f64> = None;
        let (mut kb, mut view) = (0.0, 0);
        while g.requests < cp {
            g.advance();
            let r = g.requests;
            let timed = r + plan.tail > cp;
            if !timed && r > plan.dense_until && !r.is_multiple_of(plan.stride) {
                continue;
            }
            if timed && cpu0.is_none() {
                cpu0 = Some(thread_cpu_s());
            }
            let t = Instant::now();
            let reply = cur.view(9000, &g.msgs, 0.0).expect("view");
            let dt = t.elapsed().as_secs_f64() * 1000.0;
            if timed {
                let t = Instant::now();
                let text = reply.memory.to_string();
                both.push(dt + t.elapsed().as_secs_f64() * 1000.0);
                curate.push(dt);
                kb = text.len() as f64 / 1024.0;
                view = reply.view.len();
            }
        }
        let cpu_ms = cpu0.map_or(0.0, |c| (thread_cpu_s() - c) * 1000.0 / curate.len().max(1) as f64);
        let rss = rss_mb();
        let ingest = if plan.ingest {
            let mut fresh = Curator::new("").expect("default spec");
            let t = Instant::now();
            fresh.view(9000, &g.msgs, 0.0).expect("view");
            t.elapsed().as_secs_f64() * 1000.0 / (g.msgs.len() as f64 / 1000.0)
        } else {
            0.0
        };
        rows.push(Row {
            kind: g.kind,
            requests: cp,
            messages: g.msgs.len(),
            median_ms: median(&curate),
            p99_ms: pct(&curate, 0.99),
            with_reply_ms: median(&both),
            reply_kb: kb,
            rss_mb: rss,
            ingest_ms_per_1k: ingest,
            cpu_ms,
            view,
        });
    }
    rows
}
