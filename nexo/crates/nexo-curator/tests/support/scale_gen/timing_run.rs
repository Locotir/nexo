use super::*;

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
    let mut cur = Curator::new(&spec()).expect("spec");
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
            let mut fresh = Curator::new(&spec()).expect("spec");
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

// ---------------------------------------------------------------------------------------------------------
// The cold scale test: many distinct topics, big pasted texts, and questions that correlate facts across
// topics. Offline and deterministic (seeded); the session is generated block by block while it runs, so a
// 100,000-topic history never exists as a file. No model: the oracle knows the source line of every fact.
