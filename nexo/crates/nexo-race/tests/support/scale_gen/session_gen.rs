use super::*;

/// The four shapes: `e8` (the shape of examples/bench.rs, one topic per 25 steps), `a` (a text-only assistant
/// reply before each request), `b` (topics that return every 6 steps), `c` (a chat without tools).
pub const KINDS: [&str; 4] = ["e8", "a", "b", "c"];

pub struct Gen {
    pub(super) kind: &'static str,
    pub msgs: Vec<Value>,
    pending: VecDeque<Value>,
    cycle: usize,
    rng: u64,
    /// User messages so far (each one is a request to the model).
    pub requests: usize,
}

pub(super) const WORDS: [&str; 24] = [
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
