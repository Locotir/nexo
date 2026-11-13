use super::*;

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
    pub(super) fn topic(&self, t: usize) -> Vec<Value> {
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
