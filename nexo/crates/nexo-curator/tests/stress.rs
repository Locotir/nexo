//! Stress replay: a long generated session with reads of mixed sizes, reads made again, `recall` calls on old
//! outputs and edits, fed to the curator one request at a time as the host does. At each checkpoint it prints
//! the time a request takes, the tokens the view sends, the stats and the memory, so that the cost per request
//! can be read at the start and at the end of a very long session. Ignored by default (slow):
//! `cargo test --release -p nexo-curator --test stress -- --ignored --nocapture`.
//! `STRESS_STEPS` sets the steps of the agent (default 20000), `STRESS_SPEC` the settings over the defaults
//! (`pockets=1`), `STRESS_SEED` the seed.

#[path = "support/scale_gen/mod.rs"]
mod scale_gen;

use std::time::Instant;

use nexo_curator::Curator;
use scale_gen::{median, pct, rss_mb};
use serde_json::{json, Value};

/// A read the agent made: the message of its output, its file and its lines.
type Read = (usize, String, usize, usize);

struct Rng(u64);

impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.next(100) < percent
    }
}

fn hash(path: &str, line: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.bytes().chain(line.to_string().bytes()) {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Lines `a..=b` of file `path`: every line its own, and the same whenever it is read.
fn lines(path: &str, a: usize, b: usize) -> String {
    let stem = path.replace(['/', '.'], "_");
    (a..=b)
        .map(|l| {
            let h = hash(path, l);
            format!("{l:>5}: let item_{stem}_{l} = load_{}(0x{:x}, {});\n", h % 97, h >> 40, h % 1000)
        })
        .collect()
}

struct Gen {
    rng: Rng,
    msgs: Vec<Value>,
    reads: Vec<Read>,
    task: usize,
    step: usize,
    /// Steps left on the open task.
    left: usize,
}

impl Gen {
    fn new(seed: u64) -> Gen {
        let mut g = Gen { rng: Rng(seed | 1), msgs: Vec::new(), reads: Vec::new(), task: 0, step: 0, left: 0 };
        g.request();
        g
    }

    fn module(&self) -> usize {
        self.task / 8
    }

    /// The next task: it names the module and the job, as the recorded sessions do, not a file (a request
    /// that names a file read many times before brings every read of it back, by `named`).
    fn request(&mut self) {
        let (t, m) = (self.task, self.module());
        self.msgs.push(json!({"role": "user", "content": format!("Task {t}: job_{t} fails in module m{m}. Find the cause and fix it.")}));
        self.left = 6 + self.rng.next(12);
    }

    fn call(&mut self, name: &str, input: Value, out: String, note: Option<String>) {
        let id = format!("c{}", self.step);
        let mut content = vec![json!({"type": "text", "text": format!("Checking step {}.", self.step)})];
        if let Some(n) = note {
            content[0]["text"] = json!(format!("Finding: {n}"));
        }
        content.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
        self.msgs.push(json!({"role": "assistant", "content": content}));
        self.msgs
            .push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}));
    }

    /// One step of the agent: a call and its output, or the reply that closes the task and the next request.
    fn advance(&mut self) {
        self.step += 1;
        if self.left == 0 {
            let t = self.task;
            self.msgs.push(json!({"role": "assistant", "content": format!("Done with job_{t}: the loader in module m{} was the cause.", self.module())}));
            self.task += 1;
            self.request();
            return;
        }
        self.left -= 1;
        let m = self.module();
        let roll = self.rng.next(100);
        let note = self.rng.chance(20).then(|| format!("job_{} depends on item_{}.", self.task, self.rng.next(5000)));
        if roll < 55 || self.reads.is_empty() {
            // a read of a file of the module, of a small, middling or large range
            let path = format!("src/m{m}/f{}.rs", self.rng.next(5));
            let n = match self.rng.next(100) {
                0..=49 => 8,
                50..=84 => 60,
                _ => 300,
            };
            let a = 1 + 20 * self.rng.next(40);
            let out = lines(&path, a, a + n - 1);
            self.reads.push((self.msgs.len() + 1, path.clone(), a, a + n - 1));
            self.call("read", json!({"path": path, "offset": a, "limit": n}), out, note);
        } else if roll < 70 {
            let out: String = (0..6)
                .map(|j| {
                    let (f, l) = (j % 5, 7 + 13 * j + self.step % 11);
                    let path = format!("src/m{m}/f{f}.rs");
                    format!("{path}:{}", lines(&path, l, l).trim_start())
                })
                .collect();
            self.call("bash", json!({"command": format!("grep -n item_src_m{m} src/m{m}")}), out, note);
        } else if roll < 80 {
            // the same read as an earlier one, old or new
            let (_, path, a, b) = self.reads[self.rng.next(self.reads.len())].clone();
            let out = lines(&path, a, b);
            self.reads.push((self.msgs.len() + 1, path.clone(), a, b));
            self.call("read", json!({"path": path, "offset": a, "limit": b + 1 - a}), out, note);
        } else if roll < 88 {
            // a recall of an earlier output, by the name the curator gives its message
            let (mi, path, a, b) = self.reads[self.rng.next(self.reads.len())].clone();
            let out = format!("m{:05} (the output of read), as it was:\n{}", mi + 1, lines(&path, a, b).trim_end());
            self.call("recall", json!({"message": format!("m{:05}", mi + 1)}), out, note);
        } else if roll < 95 {
            let (_, path, a, _) = self.reads[self.rng.next(self.reads.len())].clone();
            let h = hash(&path, a);
            let input = json!({"path": path, "old_string": format!("load_{}(", h % 97), "new_string": format!("load_{}(", (h + 1) % 97)});
            self.call("edit", input, format!("Edited {path}."), note);
        } else {
            self.call(
                "bash",
                json!({"command": "cargo test 2>&1 | tail -3"}),
                format!("test result: ok. {} passed; 0 failed\n", 40 + self.step % 7),
                note,
            );
        }
    }
}

#[test]
#[ignore]
fn a_very_long_session_keeps_its_view_and_its_time_per_request_bounded() {
    let steps: usize = std::env::var("STRESS_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let spec = std::env::var("STRESS_SPEC").unwrap_or_default();
    let seed: u64 = std::env::var("STRESS_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(7);
    let mut cur = Curator::new(&spec).expect("a spec the curator runs");
    cur.tools(&["read".into(), "bash".into(), "edit".into(), "recall".into(), "note".into()]);
    let mut g = Gen::new(seed);
    let checkpoints = [500usize, 1000, 2000, 5000, 10_000, 20_000, 50_000, 100_000];
    // the time and the tokens of every request; a checkpoint reads the last 500
    let (mut ms, mut toks): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    let last = |v: &[f64]| v[v.len().saturating_sub(500)..].to_vec();
    let mut total = 0i64;
    let started = Instant::now();
    println!("spec {spec:?}, {steps} steps, seed {seed}");
    println!(
        "{:>7} {:>7} {:>8} {:>8} {:>8} {:>8} {:>12} {:>6}  stats",
        "step", "msgs", "ms med", "ms p99", "tok med", "tok max", "tok total", "rss"
    );
    for s in 1..=steps {
        g.advance();
        let t = Instant::now();
        let r = cur.view(9000, &g.msgs, 2.0 * s as f64).expect("a view");
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
        toks.push(r.tokens as f64);
        total += r.tokens;
        if checkpoints.contains(&s) || s == steps {
            let keys = [
                "miss",
                "closed_steps",
                "recalled_steps",
                "agent_recalled_steps",
                "left_messages",
                "dropped_chunks",
                "rewrites",
                "cap_rewrites",
                "live_dropped",
            ];
            let stats: Vec<String> = keys.iter().filter_map(|k| r.stats.get(k).map(|v| format!("{k} {v}"))).collect();
            let (w_ms, w_tok) = (last(&ms), last(&toks));
            println!(
                "{s:>7} {:>7} {:>8.2} {:>8.2} {:>8.0} {:>8.0} {:>12} {:>5.0}M  {}",
                g.msgs.len(),
                median(&w_ms),
                pct(&w_ms, 0.99),
                median(&w_tok),
                pct(&w_tok, 1.0),
                total,
                rss_mb(),
                stats.join(", ")
            );
        }
    }
    println!(
        "ms per request: first 500 median {:.2}, last 500 median {:.2}; {:.0} s in all, view {} messages",
        median(&ms[..ms.len().min(500)]),
        median(&last(&ms)),
        started.elapsed().as_secs_f64(),
        cur.view(9000, &g.msgs, 2.0 * steps as f64).map(|r| r.view.len()).unwrap_or(0)
    );
}
