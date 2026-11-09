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

// ---------------------------------------------------------------------------------------------------------
// The cold scale test: many distinct topics, big pasted texts, and questions that correlate facts across
// topics. Offline and deterministic (seeded); the session is generated block by block while it runs, so a
// 100,000-topic history never exists as a file. No model: the oracle knows the source line of every fact.

/// Parameters of one cold scale session.
#[derive(Debug, Clone)]
pub struct ColdParams {
    /// Distinct topics the session works through before the questions (each holds three facts).
    pub topics: usize,
    /// Facts a question correlates, each from another topic (1, 2 or 3; 0: questions take 1, 2, 3 in turn).
    pub hops: usize,
    pub seed: u64,
    /// Questions asked after the topics, each after a few switches to fresh topics.
    pub questions: usize,
    /// Every this many topics the person pastes a big text (log, numeric table or prose).
    pub paste_every: usize,
    pub paste_lines: usize,
    /// The curator is called on every `stride`-th request (1: every request, as the host does) and always on questions.
    pub stride: usize,
    /// Stop the session when it has run this long (seconds) or its process holds this much memory (MB); 0: no limit.
    pub max_secs: f64,
    pub max_rss_mb: f64,
    /// Standing facts planted early in the session (0 to 10), each asked about by 3 tasks that suppose its default.
    pub standing: usize,
}

impl ColdParams {
    pub fn new(topics: usize, hops: usize, seed: u64) -> ColdParams {
        ColdParams {
            topics,
            hops,
            seed,
            questions: 30,
            paste_every: 10,
            paste_lines: 80,
            stride: 1,
            max_secs: 0.0,
            max_rss_mb: 0.0,
            standing: 0,
        }
    }

    /// A session of about `tokens` tokens: as many topics as that takes, measured on the first 200 (the text of
    /// a topic depends on the seed and its position only).
    pub fn for_tokens(tokens: u64, hops: usize, seed: u64) -> ColdParams {
        use nexo_curator::sim::price::tokens_of;
        let mut p = ColdParams::new(200, hops, seed);
        let g = Cold::new(p.clone());
        let per: u64 = (0..200).flat_map(|t| g.topic(t)).map(|m| tokens_of(&m) as u64).sum::<u64>() / 200;
        p.topics = (tokens / per.max(1)).max(200) as usize;
        p
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn mix(seed: u64, a: u64, b: u64) -> u64 {
    splitmix(seed ^ splitmix(a ^ splitmix(b)))
}

const OWNERS: [&str; 16] = [
    "alder",
    "birch",
    "cedar",
    "dunmore",
    "elmwood",
    "fenwick",
    "gable",
    "harlow",
    "ironside",
    "juniper",
    "kestrel",
    "lindqvist",
    "marlow",
    "norwood",
    "oakley",
    "pemberton",
];

/// The names of the three facts of a topic, in order.
pub const FACTS: [&str; 3] = ["port", "version", "owner"];

/// The three source lines of topic `t` (a port, a version and an owner), a function of the seed alone.
pub fn cold_facts(seed: u64, t: usize) -> [String; 3] {
    let h = mix(seed, t as u64, 1);
    [
        format!("[topic {t}] port = {}", 1024 + h % 60000),
        format!("[topic {t}] version = {}.{}.{}", (h >> 20) % 9 + 1, (h >> 28) % 40, (h >> 36) % 100),
        format!("[topic {t}] owner = {}{:03}", OWNERS[((h >> 44) % 16) as usize], (h >> 50) % 1000),
    ]
}

/// A step of the session: messages to append (alternating roles, the first a user message unless noted) and,
/// for a question block, the source lines its first message needs in the view; for a task block, the source
/// line of the standing fact whose default the task supposes (`stand`).
pub struct Block {
    pub msgs: Vec<Value>,
    pub ask: Option<Vec<String>>,
    pub stand: Option<Vec<String>>,
}

/// Ways a standing fact reaches the history: the person says it, the agent notes it (`Finding:`), or a
/// `cat` of a config file shows it.
#[derive(Clone, Copy, PartialEq)]
enum Via {
    User,
    Model,
    Cat,
}

/// A standing fact that overrides a default, as planted: how each source words it, and the task that supposes
/// the default without naming the fact.
pub struct Fixed {
    via: Via,
    /// The source line the oracle looks for in the view (a `key = value` line of a config, a sentence of the
    /// person, or what follows `Finding:` in the agent's reply).
    pub line: String,
    /// The request that supposes the default.
    pub task: &'static str,
}

/// Standing facts a session can plant, at most.
pub const FIXED_MAX: usize = 10;
/// Tasks that suppose the default of each planted fact.
pub const STAND_REPS: usize = 3;

/// Fact `i` of a session of `seed`: wording a function of the seed and the index alone. The shell rc, the
/// database port, the token file, the legacy folder, the search tool, the staging host, the editor config,
/// the logs, the build output and the backups: each overrides what a model would assume.
pub fn fixed_fact(seed: u64, i: usize) -> Fixed {
    let v = 100 + mix(seed, i as u64, 7) % 900;
    // (how it comes, the line, the task)
    let (via, line, task) = match i % FIXED_MAX {
        0 => (Via::Cat, format!("shell_rc = /etc/skel-{v}/rc"), "Add an alias to my shell."),
        1 => (
            Via::Model,
            format!("the database (postgres) listens on port {}, not 5432", 6000 + v),
            "Connect to the database and count the rows in users.",
        ),
        2 => (Via::Cat, format!("api_token_file = /run/secrets/api{v}"), "Rotate the API token."),
        3 => {
            (Via::User, format!("Don't touch /srv/legacy{v}, it is the legacy folder."), "Clean up the legacy folder.")
        }
        4 => (Via::Model, "searching the repo goes with rg, not grep".to_string(), "Search the repo for TODO."),
        5 => (
            Via::User,
            format!("The staging server is at stage{v}.internal:8797, not localhost:8080."),
            "Deploy to staging.",
        ),
        6 => {
            (Via::Cat, format!("editor_config = /home/dev/.config/edit{v}/init.vim"), "Change a setting in my editor.")
        }
        7 => (
            Via::User,
            format!("The app logs are stored in /data/logs{v}, not /var/log/app."),
            "Look through the app logs for errors.",
        ),
        8 => (
            Via::Model,
            format!("build artifacts are kept in /data/out{v}, not ./dist"),
            "Copy the build artifacts to the release folder.",
        ),
        _ => (
            Via::User,
            format!("Backups are stored in /mnt/backup{v}, not /tmp."),
            "Run a backup of the database dump.",
        ),
    };
    Fixed { via, line, task }
}

pub struct Cold {
    p: ColdParams,
    /// The next block: 0 is the opening, then the topics with the standing tasks among them, then the
    /// questions with their switches.
    next: usize,
    /// The next topic and the next standing task to hand out.
    t_next: usize,
    j_next: usize,
    rng: u64,
}

const SWITCHES: usize = 3;

impl Cold {
    pub fn new(p: ColdParams) -> Cold {
        assert!(p.hops <= 3 && p.topics >= 3, "hops must be 0 to 3, and there are at least 3 topics");
        assert!(p.standing <= FIXED_MAX && (p.standing == 0 || p.topics >= 10), "up to {FIXED_MAX} standing facts");
        let rng = splitmix(p.seed ^ 0x5CA1_E5CA) | 1;
        Cold { p, next: 0, t_next: 0, j_next: 0, rng }
    }

    /// Blocks handed out so far.
    pub fn position(&self) -> usize {
        self.next
    }

    /// Standing tasks in the session.
    pub fn tasks(&self) -> usize {
        self.p.standing * STAND_REPS
    }

    pub fn total_blocks(&self) -> usize {
        1 + self.p.topics + self.tasks() + self.p.questions * (SWITCHES + 1)
    }

    /// The topics (a few percent of them, at the start) that hold the standing facts: fact `i` is in topic `plant(i)`.
    fn span(&self) -> usize {
        (self.p.topics / 50).max(4).min(self.p.topics)
    }

    fn plant(&self, i: usize) -> usize {
        i * self.span() / self.p.standing.max(1)
    }

    /// The topic after which standing task `j` comes: spread from after a tenth of the topics to the last.
    fn after(&self, j: usize) -> usize {
        let lo = (self.span() + 1 + self.p.topics / 10).min(self.p.topics - 1);
        lo + j * (self.p.topics - 1 - lo) / self.tasks().max(1)
    }

    fn rand(&mut self, n: usize) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % n as u64) as usize
    }

    /// The configuration file of topic `t`: filler settings with the three fact lines among them.
    fn config(&self, t: usize) -> String {
        let facts = cold_facts(self.p.seed, t);
        let mut h = mix(self.p.seed, t as u64, 3);
        let mut lines: Vec<String> = (0..9)
            .map(|i| {
                h = splitmix(h);
                format!("{} = {}", WORDS[(h % 24) as usize], (h >> 8) % 100_000 + i)
            })
            .collect();
        for f in facts {
            h = splitmix(h);
            lines.insert((h % (lines.len() as u64 + 1)) as usize, f);
        }
        for f in self.planted(t).into_iter().filter(|f| f.via == Via::Cat) {
            h = splitmix(h);
            lines.insert((h % (lines.len() as u64 + 1)) as usize, f.line);
        }
        format!("# service config, topic {t}\n{}\n", lines.join("\n"))
    }

    /// The standing facts planted in topic `t`.
    fn planted(&self, t: usize) -> Vec<Fixed> {
        (0..self.p.standing).filter(|&i| self.plant(i) == t).map(|i| fixed_fact(self.p.seed, i)).collect()
    }

    /// A big pasted text for topic `t`: a log, a numeric table or prose, never a fact line.
    fn paste(&self, t: usize) -> String {
        let mut h = mix(self.p.seed, t as u64, 2);
        let kind = h % 3;
        let mut s = String::new();
        for i in 0..self.p.paste_lines {
            h = splitmix(h);
            match kind {
                0 => {
                    s += &format!(
                        "2026-11-08T{:02}:{:02}:{:02}Z INFO worker-{} handled request id={:08x} in {} ms status={}\n",
                        h % 24,
                        (h >> 8) % 60,
                        (h >> 16) % 60,
                        (h >> 24) % 8,
                        (h >> 32) as u32,
                        (h >> 5) % 900,
                        [200, 200, 200, 404, 500][((h >> 40) % 5) as usize]
                    )
                }
                1 => {
                    s += &format!(
                        "{:>6} {:>10.3} {:>10.3} {:>8} {:>12}\n",
                        i,
                        (h % 1_000_000) as f64 / 1000.0,
                        ((h >> 20) % 1_000_000) as f64 / 7000.0,
                        (h >> 40) % 9999,
                        splitmix(h) % 10_000_000_000
                    )
                }
                _ => {
                    let n = 9 + (h >> 60) as usize % 5;
                    let mut x = h;
                    let line: Vec<&str> = (0..n)
                        .map(|_| {
                            x = splitmix(x);
                            WORDS[(x % 24) as usize]
                        })
                        .collect();
                    s += &line.join(" ");
                    s.push('\n');
                }
            }
        }
        s
    }

    /// The messages of one topic: the switch, a `cat` of its config, and now and then a big paste.
    fn topic(&self, t: usize) -> Vec<Value> {
        let id = format!("t{t}");
        let planted = self.planted(t);
        // what the agent notes of the topic: the standing facts it found there
        let noted: String =
            planted.iter().filter(|f| f.via == Via::Model).map(|f| format!("\nFinding: {}", f.line)).collect();
        let mut out = vec![
            json!({"role": "user", "content": format!("Topic {t}: now look at the {} service config in /srv/t{t}.", WORDS[t % 24])}),
            json!({"role": "assistant", "content": [{"type": "text", "text": format!("Reading the config of topic {t}.")},
                {"type": "tool_use", "id": id, "name": "bash", "input": {"command": format!("cat /srv/t{t}/service.conf")}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": self.config(t)}]}),
            json!({"role": "assistant", "content": format!("Checked it.{noted}")}),
        ];
        for f in planted.iter().filter(|f| f.via == Via::User) {
            out.push(json!({"role": "user", "content": f.line}));
            out.push(json!({"role": "assistant", "content": "Noted."}));
        }
        if t.is_multiple_of(self.p.paste_every.max(1)) {
            out.push(json!({"role": "user", "content": format!("Also, here is what the team pasted about topic {t}:\n{}", self.paste(t))}));
            out.push(json!({"role": "assistant", "content": "Read it."}));
        }
        out
    }

    /// The next block of the session, `None` at its end.
    pub fn next_block(&mut self) -> Option<Block> {
        let b = self.next;
        if b >= self.total_blocks() {
            return None;
        }
        self.next += 1;
        if b == 0 {
            return Some(Block {
                msgs: vec![
                    json!({"role": "user", "content": "Go through the services one topic at a time and keep what each config says."}),
                    json!({"role": "assistant", "content": "Ready."}),
                ],
                ask: None,
                stand: None,
            });
        }
        let i = b - 1;
        let lead = self.p.topics + self.tasks();
        if i < lead {
            // the next topic, unless a standing task is due: the topic it follows has been handed out
            if self.j_next < self.tasks() && self.after(self.j_next) < self.t_next {
                let f = fixed_fact(self.p.seed, self.j_next % self.p.standing);
                self.j_next += 1;
                return Some(Block {
                    msgs: vec![
                        json!({"role": "user", "content": f.task}),
                        json!({"role": "assistant", "content": "On it."}),
                    ],
                    ask: None,
                    stand: Some(vec![f.line]),
                });
            }
            self.t_next += 1;
            return Some(Block { msgs: self.topic(self.t_next - 1), ask: None, stand: None });
        }
        let i = i - lead;
        let j = i % (SWITCHES + 1);
        if j < SWITCHES {
            // a switch to a fresh topic, never asked about
            let q = i / (SWITCHES + 1);
            return Some(Block { msgs: self.topic(self.p.topics + q * SWITCHES + j), ask: None, stand: None });
        }
        let hops = if self.p.hops == 0 { 1 + i / (SWITCHES + 1) % 3 } else { self.p.hops };
        let mut picked: Vec<(usize, usize)> = Vec::new();
        while picked.len() < hops {
            let t = self.rand(self.p.topics);
            if picked.iter().all(|(x, _)| *x != t) {
                let k = self.rand(3);
                picked.push((t, k));
            }
        }
        let part: Vec<String> =
            picked.iter().map(|(t, k)| format!("the {} of the service in topic {t}", FACTS[*k])).collect();
        let lines = picked.iter().map(|(t, k)| cold_facts(self.p.seed, *t)[*k].clone()).collect();
        let text = match part.as_slice() {
            [a] => format!("What is {a}?"),
            [a, b] => format!("What is {a} and {b}?"),
            [a, b, c] => format!("What is {a}, {b} and {c}?"),
            _ => unreachable!("hops is 1 to 3"),
        };
        Some(Block {
            msgs: vec![json!({"role": "user", "content": text}), json!({"role": "assistant", "content": "Noted."})],
            ask: Some(lines),
            stand: None,
        })
    }
}

/// The oracle: does the view hold every source line? (A view without them is a miss.)
pub fn holds(view: &[Value], lines: &[String]) -> bool {
    let text: String = view.iter().map(nexo_curator::sim::input::text_of).collect::<Vec<_>>().join("\n");
    lines.iter().all(|l| text.contains(l.as_str()))
}

/// Peak resident memory of this process in MB (VmHWM).
pub fn peak_rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

/// What was last sent, with the tokens of each message: the prefix cache of a one-slot provider.
#[derive(Default)]
struct Sent {
    msgs: Vec<Value>,
    tokens: Vec<i64>,
}

impl Sent {
    /// Sends `now`; returns (all tokens, tokens read from the cache).
    fn send(&mut self, now: Vec<Value>) -> (i64, i64) {
        use nexo_curator::sim::price::{grown, tokens_of};
        let same = now.iter().zip(&self.msgs).take_while(|(a, b)| a == b).count();
        let mut hit: i64 = self.tokens[..same].iter().sum();
        if let (Some(a), Some(b)) = (now.get(same), self.msgs.get(same)) {
            hit += grown(a, b);
        }
        self.tokens.truncate(same);
        self.msgs.truncate(same);
        for m in now.into_iter().skip(same) {
            self.tokens.push(tokens_of(&m));
            self.msgs.push(m);
        }
        (self.tokens.iter().sum(), hit)
    }
}

/// The result of one cold session (N, hops).
#[derive(Debug, Clone)]
pub struct ColdRow {
    pub topics: usize,
    pub hops: usize,
    pub questions: usize,
    pub answered: usize,
    /// Questions of one fact and answered ones, and of two or three facts and answered ones.
    pub by_hops: [usize; 4],
    /// Standing facts planted (0: the session has none), tasks that suppose their defaults, and tasks whose view held the
    /// source line of the fact.
    pub standing: usize,
    pub stand: [usize; 2],
    pub calls: usize,
    pub view_max: i64,
    /// Input tokens sent (missed plus cached), with the system prompt.
    pub sent: i64,
    pub usd: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub rss_mb: f64,
    /// The plain arm (the whole history every request): its largest request, input tokens sent, and cost.
    pub plain_max: i64,
    pub plain_sent: i64,
    pub plain_usd: f64,
    /// The curator was called on every this-many-th request (1: all; more: the totals above count the calls made).
    pub stride: usize,
    /// Tokens of the history when the session ended (the whole session unless it was stopped).
    pub tokens: i64,
    /// Why the session was stopped early (time or memory), with where the time and the memory went.
    pub stopped: Option<String>,
    /// Seconds in `Curator::view`, in the generator and oracle, in the pricing, and in all.
    pub secs: [f64; 4],
}

impl ColdRow {
    pub fn recall(&self) -> f64 {
        100.0 * self.answered as f64 / self.questions.max(1) as f64
    }

    /// Percent of the standing tasks whose view held the fact they suppose (`fijos`).
    pub fn fijos(&self) -> f64 {
        100.0 * self.stand[0] as f64 / self.stand[1].max(1) as f64
    }

    /// The `fijos` column with its separator, empty for a session without standing facts.
    fn fijos_col(&self) -> String {
        if self.standing > 0 {
            format!(" | fijos {:.1}", self.fijos())
        } else {
            String::new()
        }
    }

    /// The ladder line (`--tokens`): tokens totales | N | % marcas en 1 salto | % en 2-3 saltos | vista máx | enviados |
    /// DeepSeek $ | ms/petición p50/p99 | RSS máx MB | plain, and, for a stopped session, why and where it went.
    pub fn line_tokens(&self) -> String {
        let r = |hit: usize, n: usize| {
            if n == 0 {
                "-".to_string()
            } else {
                format!("{:.1}", 100.0 * hit as f64 / n as f64)
            }
        };
        let mut s = format!(
            "{:.2}M tokens | {} topics | {} | {}{} | {} | {:.2}M | {:.4} | {:.2}/{:.2} | {:.0} | plain máx {} {:.2}M ${}",
            self.tokens as f64 / 1e6,
            self.topics,
            r(self.by_hops[0], self.by_hops[1]),
            r(self.by_hops[2], self.by_hops[3]),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            self.plain_max,
            self.plain_sent as f64 / 1e6,
            if self.stride > 1 { format!(" | stride {}", self.stride) } else { String::new() },
        );
        if let Some(why) = &self.stopped {
            s += &format!(
                " | PARADO: {why}; s: curator {:.0}, generador y oráculo {:.0}, precio {:.0}, total {:.0}",
                self.secs[0], self.secs[1], self.secs[2], self.secs[3]
            );
        }
        s
    }

    /// The one-line report: N | hops | recall % | view máx | enviados | DeepSeek $ | ms/request p50/p99 | RSS máx MB,
    /// then the plain arm: largest request, sent, $, and whether it fits 128k and 1M windows.
    pub fn line(&self) -> String {
        let fit = |w: i64| if self.plain_max <= w { "cabe" } else { "no cabe" };
        format!(
            "{} | {} | {:.1}{} | {} | {:.2}M | {:.4} | {:.2}/{:.2} | {:.0} | plain máx {} ({} 128k, {} 1M) enviados {:.2}M {:.4} ${}",
            self.topics,
            self.hops,
            self.recall(),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            self.plain_max,
            fit(128_000),
            fit(1_000_000),
            self.plain_sent as f64 / 1e6,
            self.plain_usd,
            if self.stride > 1 { format!(" | stride {}", self.stride) } else { String::new() },
        )
    }
}

/// Runs the cold session: the curator sees every request (or every `stride`-th, and every question), the
/// oracle checks each question's view, and both arms are priced at the replay's DeepSeek prices.
pub fn run_cold(p: &ColdParams) -> ColdRow {
    use nexo_curator::sim::price::{tokens_of, Prices};
    const SYSTEM: i64 = 9000;
    let prices = Prices::LIVE;
    let mut g = Cold::new(p.clone());
    let mut cur = Curator::new("").expect("default spec");
    let mut msgs: Vec<Value> = Vec::new();
    let mut sent = Sent::default();
    let (mut now, mut requests) = (0.0, 0usize);
    let mut row = ColdRow {
        topics: p.topics,
        hops: p.hops,
        questions: 0,
        answered: 0,
        calls: 0,
        view_max: 0,
        sent: 0,
        usd: 0.0,
        p50_ms: 0.0,
        p99_ms: 0.0,
        rss_mb: 0.0,
        plain_max: 0,
        plain_sent: 0,
        plain_usd: 0.0,
        stride: p.stride.max(1),
        by_hops: [0; 4],
        standing: p.standing,
        stand: [0; 2],
        tokens: 0,
        stopped: None,
        secs: [0.0; 4],
    };
    let (mut miss, mut hit, mut plain_total, mut plain_before) = (0i64, 0i64, 0i64, 0i64);
    let mut times = Vec::new();
    // the peak memory counted below is this session's alone
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let start = Instant::now();
    let (mut view_s, mut price_s) = (0.0, 0.0);
    'session: while let Some(b) = g.next_block() {
        let mut ask = b.ask;
        let mut stand = b.stand;
        for m in b.msgs {
            let user = m["role"] == "user";
            plain_total += tokens_of(&m);
            msgs.push(m);
            if !user {
                continue;
            }
            requests += 1;
            if requests.is_multiple_of(512) {
                let secs = start.elapsed().as_secs_f64();
                let rss = rss_mb();
                if (p.max_secs > 0.0 && secs > p.max_secs) || (p.max_rss_mb > 0.0 && rss > p.max_rss_mb) {
                    let what = if rss > p.max_rss_mb && p.max_rss_mb > 0.0 { "RSS" } else { "tiempo" };
                    row.stopped = Some(format!(
                        "{what} sobre el tope en el {:.1} % de la sesión ({} peticiones, {} llamadas), RSS {:.0} MB, historia ≈ {:.0} MB de texto",
                        100.0 * g.position() as f64 / g.total_blocks() as f64,
                        requests,
                        row.calls,
                        rss,
                        plain_total as f64 * 4.0 / 1e6
                    ));
                    break 'session;
                }
            }
            let question = ask.take();
            let task = stand.take();
            if question.is_none() && task.is_none() && !requests.is_multiple_of(p.stride.max(1)) {
                continue;
            }
            now += 20.0;
            let t = Instant::now();
            let reply = cur.view(SYSTEM as u64, &msgs, now).expect("view");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            view_s += t.elapsed().as_secs_f64();
            let priced = Instant::now();
            row.calls += 1;
            row.view_max = row.view_max.max(reply.tokens);
            if let Some(lines) = question {
                row.questions += 1;
                let ok = usize::from(holds(&reply.view, &lines));
                row.answered += ok;
                let k = if lines.len() == 1 { 0 } else { 2 };
                row.by_hops[k] += ok;
                row.by_hops[k + 1] += 1;
            }
            if let Some(lines) = task {
                row.stand[0] += usize::from(holds(&reply.view, &lines));
                row.stand[1] += 1;
            }
            let (all, cached) = sent.send(reply.view);
            let (all, cached) = (all + SYSTEM, if row.calls == 1 { 0 } else { cached + SYSTEM });
            miss += all - cached;
            hit += cached;
            let plain_all = plain_total + SYSTEM;
            row.plain_max = row.plain_max.max(plain_all);
            row.plain_sent += plain_all;
            row.plain_usd += prices.dollars(plain_all - plain_before.min(plain_all), plain_before.min(plain_all));
            plain_before = plain_all;
            price_s += priced.elapsed().as_secs_f64();
        }
    }
    row.sent = miss + hit;
    row.usd = prices.dollars(miss, hit);
    row.p50_ms = pct(&times, 0.5);
    row.p99_ms = pct(&times, 0.99);
    row.rss_mb = peak_rss_mb();
    row.tokens = plain_total;
    let total = start.elapsed().as_secs_f64();
    row.secs = [view_s, (total - view_s - price_s).max(0.0), price_s, total];
    row
}
