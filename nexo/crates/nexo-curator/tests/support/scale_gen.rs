//! Shared by `examples/scale.rs` and `tests/scale.rs`: synthetic sessions of four shapes, fed to the curator
//! one request at a time as the host does, and the numbers measured on the way.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::Instant;

use nexo_curator::Curator;
use serde_json::{json, Value};

/// The spec the runs measure: the default one, or the settings `NEXO_SCALE_SPEC` gives over it.
fn spec() -> String {
    std::env::var("NEXO_SCALE_SPEC").unwrap_or_default()
}

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
    /// The model's window in tokens for the curator arm and the sierra arm (0: the curator is unlimited and the sierra
    /// arm runs at `SAW_WINDOW`). The plain arm is always the unlimited reference.
    pub window: u64,
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
            window: 0,
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
pub struct Sent {
    msgs: Vec<Value>,
    tokens: Vec<i64>,
}

impl Sent {
    /// Sends `now`; returns (all tokens, tokens read from the cache). The cache share of a request is the share of
    /// its prefix equal to the previous request's: the `prefijo` column.
    pub fn send(&mut self, now: Vec<Value>) -> (i64, i64) {
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
    /// Median over the calls of the share of each request's prefix equal to the previous request's, in percent: this
    /// arm's and the plain arm's (the `prefijo` column).
    pub prefix: f64,
    pub plain_prefix: f64,
    /// The window the arm ran at (0: none) and, for the sierra arm, the compactions it made.
    pub window: u64,
    pub compactions: usize,
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
            "{:.2}M tokens | {} topics | {} | {}{} | {} | {:.2}M | {:.4} | prefijo {:.1} | {:.2}/{:.2} | {:.0} | plain máx {} {:.2}M $ prefijo {:.1}{}",
            self.tokens as f64 / 1e6,
            self.topics,
            r(self.by_hops[0], self.by_hops[1]),
            r(self.by_hops[2], self.by_hops[3]),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.prefix,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            self.plain_max,
            self.plain_sent as f64 / 1e6,
            self.plain_prefix,
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

    /// The recall column: the percent of questions answered, or for a session whose questions take 1, 2 and 3 facts in
    /// turn (`hops` 0) the percent at one hop and at two or three.
    fn recall_col(&self) -> String {
        if self.hops == 0 {
            let r = |hit: usize, n: usize| 100.0 * hit as f64 / n.max(1) as f64;
            format!("{:.1}/{:.1}", r(self.by_hops[0], self.by_hops[1]), r(self.by_hops[2], self.by_hops[3]))
        } else {
            format!("{:.1}", self.recall())
        }
    }

    /// The window the arm ran at, as `128k` or `1M`.
    pub fn window_label(w: u64) -> String {
        match w {
            0 => "sin tope".to_string(),
            w if w >= 1_000_000 && w.is_multiple_of(1_000_000) => format!("{}M", w / 1_000_000),
            w if w >= 1000 && w.is_multiple_of(1000) => format!("{}k", w / 1000),
            w => w.to_string(),
        }
    }

    /// The one-line report: N | hops | recall % | view máx | enviados | DeepSeek $ | prefijo % | ms/request p50/p99 |
    /// RSS máx MB, then the plain arm (unlimited): largest request, sent, $, prefix, and whether it fits 128k and 1M
    /// windows. `hops` 0 shows the recall at one hop and at two or three (`a/b`).
    pub fn line(&self) -> String {
        let fit = |w: i64| if self.plain_max <= w { "cabe" } else { "no cabe" };
        format!(
            "{} | {} | {}{} | {} | {:.2}M | {:.4} | prefijo {:.1} | {:.2}/{:.2} | {:.0} | plain máx {} ({} 128k, {} 1M) enviados {:.2}M {:.4} $ prefijo {:.1}{}{}",
            self.topics,
            self.hops,
            self.recall_col(),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.prefix,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            self.plain_max,
            fit(128_000),
            fit(1_000_000),
            self.plain_sent as f64 / 1e6,
            self.plain_usd,
            self.plain_prefix,
            if self.window > 0 { format!(" | ventana {}", Self::window_label(self.window)) } else { String::new() },
            if self.stride > 1 { format!(" | stride {}", self.stride) } else { String::new() },
        )
    }

    /// The sierra arm's line, in the columns of `line()` up to RSS, then the window and the compactions it made.
    pub fn line_saw(&self) -> String {
        format!(
            "{} | {} | {}{} | {} | {:.2}M | {:.4} | prefijo {:.1} | {:.2}/{:.2} | {:.0} | sierra ventana {}, {} compactaciones{}",
            self.topics,
            self.hops,
            self.recall_col(),
            self.fijos_col(),
            self.view_max,
            self.sent as f64 / 1e6,
            self.usd,
            self.prefix,
            self.p50_ms,
            self.p99_ms,
            self.rss_mb,
            Self::window_label(self.window),
            self.compactions,
            if self.stride > 1 { format!(" | stride {}", self.stride) } else { String::new() },
        )
    }

    /// A row before any request.
    fn blank(p: &ColdParams) -> ColdRow {
        ColdRow {
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
            prefix: 0.0,
            plain_prefix: 0.0,
            window: p.window,
            compactions: 0,
            stride: p.stride.max(1),
            by_hops: [0; 4],
            standing: p.standing,
            stand: [0; 2],
            tokens: 0,
            stopped: None,
            secs: [0.0; 4],
        }
    }

    /// Counts one call: its view's size, and whether it held the lines of the question or of the standing task.
    fn judge(&mut self, view: &[Value], tokens: i64, question: Option<Vec<String>>, task: Option<Vec<String>>) {
        self.calls += 1;
        self.view_max = self.view_max.max(tokens);
        if let Some(lines) = question {
            self.questions += 1;
            let ok = usize::from(holds(view, &lines));
            self.answered += ok;
            let k = if lines.len() == 1 { 0 } else { 2 };
            self.by_hops[k] += ok;
            self.by_hops[k + 1] += 1;
        }
        if let Some(lines) = task {
            self.stand[0] += usize::from(holds(view, &lines));
            self.stand[1] += 1;
        }
    }
}

/// The tokens of the system prompt in the priced requests: 9000, less in a small window (an eighth of it at most).
fn system_tokens(window: u64) -> i64 {
    if window == 0 {
        9000
    } else {
        9000.min(window as i64 / 8)
    }
}

/// The window of the sierra arm when `--window` says none.
pub const SAW_WINDOW: u64 = 128_000;

/// The share of a request's tokens (system prompt included) equal to the previous request's, in percent.
fn share(all: i64, cached: i64) -> f64 {
    100.0 * cached as f64 / all.max(1) as f64
}

/// Runs the cold session: the curator sees every request (or every `stride`-th, and every question), the
/// oracle checks each question's view, and both arms are priced at the replay's DeepSeek prices.
pub fn run_cold(p: &ColdParams) -> ColdRow {
    use nexo_curator::sim::price::{tokens_of, Prices};
    let system = system_tokens(p.window);
    let prices = Prices::LIVE;
    let mut g = Cold::new(p.clone());
    let mut cur = Curator::new(&spec()).expect("spec");
    if p.window > 0 {
        cur.window(p.window);
    }
    let mut msgs: Vec<Value> = Vec::new();
    let mut sent = Sent::default();
    let (mut now, mut requests) = (0.0, 0usize);
    let mut row = ColdRow::blank(p);
    let (mut miss, mut hit, mut plain_total, mut plain_before) = (0i64, 0i64, 0i64, 0i64);
    let (mut times, mut shares, mut plain_shares) = (Vec::new(), Vec::new(), Vec::new());
    // the peak memory counted below is this session's alone
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let start = Instant::now();
    let (mut view_s, mut price_s) = (0.0, 0.0);
    // `SCALE_CHECK=1`: a hash of every reply of the session goes to stderr (the same number before and after a change that
    // must not change what the curator does)
    let (check, mut fold) = (std::env::var_os("SCALE_CHECK").is_some(), 0u64);
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
            let reply = cur.view(system as u64, &msgs, now).expect("view");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            view_s += t.elapsed().as_secs_f64();
            if check {
                // everything a call returns, in order: a change of behavior shows as another number
                let mut h = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(&(fold, serde_json::to_string(&reply.view).unwrap_or_default()), &mut h);
                std::hash::Hash::hash(
                    &(reply.tokens, reply.memory.to_string(), format!("{:?}{:?}", reply.stats, reply.curated)),
                    &mut h,
                );
                std::hash::Hash::hash(&(reply.added, reply.removed), &mut h);
                fold = std::hash::Hasher::finish(&h);
            }
            let priced = Instant::now();
            row.judge(&reply.view, reply.tokens, question, task);
            let (all, cached) = sent.send(reply.view);
            let (all, cached) = (all + system, if row.calls == 1 { 0 } else { cached + system });
            miss += all - cached;
            hit += cached;
            let plain_all = plain_total + system;
            if row.calls > 1 {
                shares.push(share(all, cached));
                plain_shares.push(share(plain_all, plain_before.min(plain_all)));
            }
            row.plain_max = row.plain_max.max(plain_all);
            row.plain_sent += plain_all;
            row.plain_usd += prices.dollars(plain_all - plain_before.min(plain_all), plain_before.min(plain_all));
            plain_before = plain_all;
            price_s += priced.elapsed().as_secs_f64();
        }
    }
    if check {
        eprintln!("replies hash {fold:016x}");
    }
    row.sent = miss + hit;
    row.usd = prices.dollars(miss, hit);
    row.p50_ms = pct(&times, 0.5);
    row.p99_ms = pct(&times, 0.99);
    row.prefix = pct(&shares, 0.5);
    row.plain_prefix = pct(&plain_shares, 0.5);
    row.rss_mb = peak_rss_mb();
    row.tokens = plain_total;
    let total = start.elapsed().as_secs_f64();
    row.secs = [view_s, (total - view_s - price_s).max(0.0), price_s, total];
    row
}

// ---------------------------------------------------------------------------------------------------------
// The sierra arm: what other harnesses do. The whole history goes to the model until the request would pass 0.9 of the
// window; then everything but the last messages is replaced by a summary of a fixed size made by rules (the first
// request, the last requests cut short, the commands run, and only the last `SAW_KV` lines of the form `key = value`)
// and the session goes on from there: the view grows, drops, grows. What the summary does not keep is lost.

/// Lines `key = value` the summary keeps, requests it quotes (each cut to `SAW_CUT` characters), commands it lists, the
/// messages that stay after a compaction at most, and the characters of the summary at most (about 2,000 tokens).
const SAW_KV: usize = 40;
const SAW_REQS: usize = 8;
const SAW_CUT: usize = 300;
const SAW_FILES: usize = 40;
const SAW_KEEP: usize = 20;
const SAW_CHARS: usize = 8000;

/// The sierra arm's memory: what the summary is made of, the summary, and the messages after it.
#[derive(Default)]
pub struct Saw {
    first: String,
    reqs: VecDeque<String>,
    files: VecDeque<String>,
    kv: VecDeque<String>,
    summary: Option<(Value, Value)>,
    live: Vec<Value>,
    live_tokens: i64,
    summary_tokens: i64,
    pub compactions: usize,
}

/// Is `m` a request of the person (a user message that is not a tool result)?
fn is_request(m: &Value) -> bool {
    m["role"] == "user"
        && match &m["content"] {
            Value::Array(blocks) => !blocks.iter().any(|b| b["type"] == "tool_result"),
            _ => true,
        }
}

fn keep_last(q: &mut VecDeque<String>, item: String, n: usize) {
    if q.back() != Some(&item) {
        q.retain(|x| *x != item);
        q.push_back(item);
    }
    while q.len() > n {
        q.pop_front();
    }
}

impl Saw {
    /// Tokens of the view (the summary and what follows it), without the system prompt.
    pub fn tokens(&self) -> i64 {
        self.summary_tokens + self.live_tokens
    }

    /// The view: the summary (with its acknowledgement) and the messages after it.
    pub fn view(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.summary.iter().flat_map(|(a, b)| [a.clone(), b.clone()]).collect();
        v.extend(self.live.iter().cloned());
        v
    }

    pub fn push(&mut self, m: Value) {
        self.live_tokens += nexo_curator::sim::price::tokens_of(&m);
        self.live.push(m);
    }

    /// Compacts when the view and the system prompt pass 0.9 of the window. The messages that stay are the last
    /// `SAW_KEEP` from a request of the person on, as many as fit in a quarter of the window, and at least the last request.
    pub fn compact_if_over(&mut self, window: i64, system: i64) -> bool {
        use nexo_curator::sim::price::tokens_of;
        if (system + self.tokens()) * 10 <= window * 9 {
            return false;
        }
        let n = self.live.len();
        let mut cut = (0..n).rev().find(|&i| is_request(&self.live[i])).unwrap_or(n - 1);
        let mut tokens: i64 = self.live[cut..].iter().map(tokens_of).sum();
        // earlier requests join while they fit
        for i in (0..cut).rev().filter(|&i| is_request(&self.live[i])) {
            let more: i64 = self.live[i..cut].iter().map(tokens_of).sum();
            if n - i > SAW_KEEP || (tokens + more) * 4 > window {
                break;
            }
            (cut, tokens) = (i, tokens + more);
        }
        let dropped: Vec<Value> = self.live.drain(..cut).collect();
        for m in &dropped {
            self.read(m);
        }
        self.live_tokens = tokens;
        self.compactions += 1;
        let (up, ack) = (
            json!({"role": "user", "content": self.text()}),
            json!({"role": "assistant", "content": "Understood. Continuing from the summary."}),
        );
        self.summary_tokens = tokens_of(&up) + tokens_of(&ack);
        self.summary = Some((up, ack));
        true
    }

    /// Takes what the summary keeps from a message that leaves the view.
    fn read(&mut self, m: &Value) {
        let text = nexo_curator::sim::input::text_of(m);
        if is_request(m) {
            let cut: String = text.chars().take(SAW_CUT).collect();
            if self.first.is_empty() {
                self.first = cut.clone();
            }
            keep_last(&mut self.reqs, cut, SAW_REQS);
        }
        if let Some(blocks) = m["content"].as_array() {
            for input in blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| &b["input"]) {
                let key = ["command", "path", "file_path", "url"].iter().find_map(|k| input[*k].as_str());
                if let Some(c) = key {
                    keep_last(&mut self.files, c.chars().take(SAW_CUT).collect(), SAW_FILES);
                }
            }
        }
        if m["role"] == "user" {
            for l in text.lines().map(str::trim).filter(|l| l.contains(" = ") && !l.starts_with('#') && l.len() <= 200)
            {
                self.kv.push_back(l.to_string());
                if self.kv.len() > SAW_KV {
                    self.kv.pop_front();
                }
            }
        }
    }

    /// The summary, at most `SAW_CHARS` characters.
    fn text(&self) -> String {
        let list = |q: &VecDeque<String>| q.iter().map(|x| format!("- {x}\n")).collect::<String>();
        let kv: String = self.kv.iter().map(|x| format!("{x}\n")).collect();
        let s = format!(
            "Summary of the conversation so far.\nFirst request: {}\nRecent requests:\n{}Commands run:\n{}Settings seen:\n{kv}",
            self.first,
            list(&self.reqs),
            list(&self.files)
        );
        s.chars().take(SAW_CHARS).collect()
    }
}

/// Runs the sierra arm on the cold session: the history until the view would pass 0.9 of the window, then a summary
/// that keeps the last 40 `key = value` lines and the last messages, and on. Same oracle, same columns, same prices as
/// `run_cold`; the plain fields stay empty (the curator row carries the reference).
pub fn run_saw(p: &ColdParams) -> ColdRow {
    use nexo_curator::sim::price::Prices;
    let window = if p.window > 0 { p.window } else { SAW_WINDOW };
    let system = system_tokens(window);
    let prices = Prices::LIVE;
    let mut g = Cold::new(p.clone());
    let mut saw = Saw::default();
    let mut sent = Sent::default();
    let mut row = ColdRow::blank(p);
    row.window = window;
    let (mut miss, mut hit, mut requests) = (0i64, 0i64, 0usize);
    let (mut times, mut shares) = (Vec::new(), Vec::new());
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let start = Instant::now();
    'session: while let Some(b) = g.next_block() {
        let mut ask = b.ask;
        let mut stand = b.stand;
        for m in b.msgs {
            let user = m["role"] == "user";
            row.tokens += nexo_curator::sim::price::tokens_of(&m);
            saw.push(m);
            if !user {
                continue;
            }
            requests += 1;
            if requests.is_multiple_of(512) {
                let (secs, rss) = (start.elapsed().as_secs_f64(), rss_mb());
                if (p.max_secs > 0.0 && secs > p.max_secs) || (p.max_rss_mb > 0.0 && rss > p.max_rss_mb) {
                    let what = if rss > p.max_rss_mb && p.max_rss_mb > 0.0 { "RSS" } else { "tiempo" };
                    row.stopped = Some(format!("{what} sobre el tope ({requests} peticiones), RSS {rss:.0} MB"));
                    break 'session;
                }
            }
            let t = Instant::now();
            // the compaction is checked on every request; only the calls the curator arm makes are priced
            saw.compact_if_over(window as i64, system);
            let question = ask.take();
            let task = stand.take();
            if question.is_none() && task.is_none() && !requests.is_multiple_of(p.stride.max(1)) {
                continue;
            }
            let view = saw.view();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            row.judge(&view, saw.tokens(), question, task);
            let (all, cached) = sent.send(view);
            let (all, cached) = (all + system, if row.calls == 1 { 0 } else { cached + system });
            miss += all - cached;
            hit += cached;
            if row.calls > 1 {
                shares.push(share(all, cached));
            }
        }
    }
    row.sent = miss + hit;
    row.usd = prices.dollars(miss, hit);
    row.p50_ms = pct(&times, 0.5);
    row.p99_ms = pct(&times, 0.99);
    row.prefix = pct(&shares, 0.5);
    row.compactions = saw.compactions;
    row.rss_mb = peak_rss_mb();
    row.secs[3] = start.elapsed().as_secs_f64();
    row
}
