//! A deterministic coding session for the curator's perf checks: requests of the person, then steps of the agent
//! (reads, edits some of which fail, shell reads, debugger moves and looks, test runs) whose outputs are large.
//! Only `serde_json` and a small LCG, so the unit test (`src/engine/tests/work.rs`) and `examples/perf.rs` both
//! include it with `#[path]`.
//!
//! Every step is two messages (the call and its result); a turn ends with the agent's text and the person's next
//! request, also two messages. So `Session::step` always adds two messages, and a history of `n` messages is
//! `n / 2` steps after the first request.

#![allow(dead_code)]

use serde_json::{json, Value};

/// What a session is made of.
#[derive(Clone, Debug)]
pub struct Shape {
    /// The seed: the same shape and seed give the same messages.
    pub seed: u64,
    /// The largest tool output, in KB (outputs run from 2 KB, or from this when it is smaller, up to it).
    pub out_kb: usize,
    /// The percent of steps that edit a file.
    pub edit_pct: usize,
    /// How many files the session works on.
    pub files: usize,
    /// How many steps make a turn (a request of the person).
    pub turn_steps: usize,
}

impl Default for Shape {
    fn default() -> Shape {
        Shape { seed: 1, out_kb: 16, edit_pct: 25, files: 40, turn_steps: 8 }
    }
}

/// A linear congruential generator: enough to make a session vary, the same every time.
pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Lcg {
        Lcg(seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407))
    }

    pub fn draw(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 24
    }

    /// A number below `n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.draw() % n as u64) as usize
    }
}

/// A made-up word, different for every `x`.
pub fn word(x: u64) -> String {
    const SYL: [&str; 16] =
        ["ka", "lo", "mi", "ren", "tu", "vas", "zel", "or", "pim", "qu", "dra", "sen", "ul", "fo", "bri", "hax"];
    let mut v = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut w = String::new();
    for _ in 0..3 {
        v = v.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        w += SYL[(v >> 60) as usize];
    }
    w
}

/// A session being written, step by step.
pub struct Session {
    shape: Shape,
    rng: Lcg,
    /// The calls made, for their ids; also the counter that makes every line of every output its own.
    calls: u64,
    /// Steps taken in the current turn.
    in_turn: usize,
    /// Requests made.
    turns: usize,
}

impl Session {
    pub fn new(shape: Shape) -> Session {
        Session { rng: Lcg::new(shape.seed), shape, calls: 0, in_turn: 0, turns: 0 }
    }

    /// The first message of the session: the person's first request.
    pub fn first(&mut self) -> Value {
        self.request()
    }

    fn request(&mut self) -> Value {
        self.turns += 1;
        let (a, b) = (self.rng.draw(), self.rng.draw());
        json!({"role": "user", "content": format!(
            "Request {}: fix the {} handling in the {} module and run the tests.", self.turns, word(a), word(b))})
    }

    /// A path of file `f`, spelled one of five ways that name the same file.
    fn path(&mut self) -> String {
        let f = self.rng.below(self.shape.files.max(1));
        let (dir, name) = (f % 7, f);
        match self.rng.below(8) {
            0 => format!("./src/m{dir}/f{name}.rs"),
            1 => format!("src\\m{dir}\\f{name}.rs"),
            2 => format!("C:\\p\\src\\m{dir}\\f{name}.rs"),
            3 => format!("/work/src/m{dir}/f{name}.rs"),
            _ => format!("src/m{dir}/f{name}.rs"),
        }
    }

    /// A tool output of 2 KB up to `out_kb` KB, no two lines alike.
    fn output(&mut self) -> String {
        let top = self.shape.out_kb.max(1) * 1024;
        let low = top.min(2048);
        let want = low + self.rng.below(top - low + 1);
        let mut out = String::with_capacity(want + 80);
        while out.len() < want {
            self.calls += 1;
            let c = self.calls;
            out += &format!(
                "{:>6}:  {} {}, {}   ; slot {c}\n",
                0x400 + 4 * (c % 4096),
                word(c),
                word(c + 100),
                word(c + 200)
            );
        }
        out
    }

    /// The call and its result for one step of the agent.
    fn tool(&mut self, name: &str, input: Value, out: &str, error: bool) -> [Value; 2] {
        self.calls += 1;
        let id = format!("call_{}", self.calls);
        [
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out, "is_error": error}]}),
        ]
    }

    /// Two more messages: a step of the agent, or the end of a turn (its text, then the person's next request).
    pub fn step(&mut self) -> [Value; 2] {
        if self.in_turn >= self.shape.turn_steps {
            self.in_turn = 0;
            let text = format!("Done with that: {} is fixed.", word(self.rng.draw()));
            return [json!({"role": "assistant", "content": [{"type": "text", "text": text}]}), self.request()];
        }
        self.in_turn += 1;
        let r = self.rng.below(100);
        if r < self.shape.edit_pct {
            let path = self.path();
            let failed = self.rng.below(6) == 0;
            let input = json!({"path": path, "old_str": word(self.rng.draw()), "new_str": word(self.rng.draw())});
            let note = if failed { "no match for old_str" } else { "edited" };
            return self.tool("edit_file", input, note, failed);
        }
        let out = self.output();
        match self.rng.below(10) {
            0..=3 => {
                let path = self.path();
                self.tool("read_file", json!({"path": path}), &out, false)
            }
            4 | 5 => {
                let path = self.path();
                self.tool("bash", json!({"command": format!("cat {path}")}), &out, false)
            }
            6 => {
                let path = self.path();
                self.tool("bash", json!({"command": format!("sed -n '1,120p' {path}")}), &out, false)
            }
            7 => {
                let command = ["-s S1 run", "-s S2 step", "continue", "regs", "-s S1 bt"][self.rng.below(5)];
                self.tool("debug", json!({"command": command}), &out, false)
            }
            _ => {
                let error = self.rng.below(5) == 0;
                self.tool("bash", json!({"command": "cargo test --workspace"}), &out, error)
            }
        }
    }

    /// Messages up to `n` or one more (a step is two).
    pub fn extend(&mut self, msgs: &mut Vec<Value>, n: usize) {
        if msgs.is_empty() {
            msgs.push(self.first());
        }
        while msgs.len() < n {
            msgs.extend(self.step());
        }
    }
}

/// A whole session of `n` messages (one more when `n` is even).
pub fn history(shape: &Shape, n: usize) -> Vec<Value> {
    let mut msgs = Vec::new();
    Session::new(shape.clone()).extend(&mut msgs, n);
    msgs
}
