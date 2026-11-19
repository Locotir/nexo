//! Generator for the seeded property tests (`tests/it/invariants.rs`): a session shaped as an agent session
//! is (turns, steps with tool calls and results, reads made again, edits, recalls, long outputs, images,
//! reasoning blocks), fully determined by one seed. Every history string avoids the marks the curator writes
//! itself, so a mark found in a view always comes from the curator.
#![allow(dead_code)]

use std::collections::BTreeMap;

use nexo_curator::is_request;
use serde_json::{json, Value};

pub const TOOLS: [&str; 8] = ["recall", "note", "Read", "Write", "Edit", "Bash", "Grep", "WebFetch"];

const WORDS: [&str; 32] = [
    "parse", "token", "buffer", "cache", "index", "route", "query", "frame", "batch", "queue", "state", "value",
    "error", "retry", "shard", "event", "chunk", "field", "table", "layer", "match", "scope", "trace", "patch",
    "build", "store", "reply", "graph", "split", "merge", "probe", "lexer",
];

/// Letters without `M` and `m`, then `+` and `/`: no digit, and never a message number.
const ALPHA: &[u8] = b"ABCDEFGHIJKLNOPQRSTUVWXYZabcdefghijklnopqrstuvwxyz+/";

const VERBS: [&str; 8] = ["Fix", "Refactor", "Explain", "Review", "Speed up", "Document", "Rename", "Test"];

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// Both ends included.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }

    pub fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }
}

/// FNV-1a, 64 bits.
pub fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

pub fn seed(k: u64) -> u64 {
    0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k + 1)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub name: &'static str,
    pub window: u64,
    pub system: u64,
    pub recall: bool,
}

pub fn config(seed: u64) -> Config {
    match seed % 4 {
        0 => Config { name: "C0", window: 0, system: 0, recall: false },
        1 => Config { name: "C1", window: 12_000, system: 1_000, recall: true },
        2 => Config { name: "C2", window: 20_000, system: 3_000, recall: true },
        _ => Config { name: "C3", window: 40_000, system: 1_000, recall: true },
    }
}

#[derive(Clone, Default)]
pub struct Opts {
    /// The S2 shape: typed text inside a results message. Off while S2 is open.
    pub typed_in_results: bool,
}

#[derive(Clone)]
pub struct Case {
    pub seed: u64,
    pub config: Config,
    pub history: Vec<Value>,
    /// Prefix lengths the curator is called with.
    pub calls: Vec<usize>,
}

// ---- small helpers on messages ----

pub fn role(m: &Value) -> &str {
    m["role"].as_str().unwrap_or("")
}

/// The content as blocks: a string is one text block, a missing content is none.
pub fn norm(m: &Value) -> Vec<Value> {
    match &m["content"] {
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

fn kind(b: &Value) -> &str {
    b["type"].as_str().unwrap_or("")
}

pub fn has_result(m: &Value) -> bool {
    norm(m).iter().any(|b| kind(b) == "tool_result")
}

/// The text of a `tool_result` content: a string, or its text blocks joined by a newline.
pub fn result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// What `recall` returns for a message: the string content, or the text blocks and result contents in order.
pub fn recall_text(m: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in norm(m) {
        match kind(&b) {
            "text" => parts.push(b["text"].as_str().unwrap_or("").to_string()),
            "tool_result" => parts.push(result_text(&b)),
            _ => {}
        }
    }
    parts.join("\n")
}

fn texts(m: &Value) -> Vec<String> {
    norm(m).iter().filter(|b| kind(b) == "text").filter_map(|b| b["text"].as_str().map(str::to_string)).collect()
}

/// A person's typed request, with no result in the message.
pub fn typed(h: &[Value], i: usize) -> bool {
    let m = &h[i];
    role(m) == "user"
        && !has_result(m)
        && texts(m).iter().any(|t| is_request(t) && t != "[goal: continue]" && t != "[supervisor: continue]")
}

/// The message that opens the latest turn at a prefix of `len` messages.
pub fn ts(h: &[Value], len: usize) -> usize {
    (0..len)
        .rev()
        .find(|&i| {
            i == 0
                || (role(&h[i]) == "user"
                    && !has_result(&h[i])
                    && texts(&h[i]).iter().any(|t| is_request(t))
                    && role(&h[i - 1]) != "user")
        })
        .unwrap_or(0)
}

fn blob(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// A lower bound of the tokens of what the view must keep at a prefix of `len` messages (see SPEC I4).
pub fn floor(h: &[Value], len: usize) -> i64 {
    let t = ts(h, len);
    let a = (0..len).rev().find(|&i| role(&h[i]) == "assistant");
    let mut keep: Vec<usize> = vec![0, t];
    keep.extend((t + 1..len).filter(|&i| typed(h, i)));
    keep.extend(a);
    if role(&h[len - 1]) == "user" && has_result(&h[len - 1]) {
        keep.push(len - 1);
    }
    keep.sort_unstable();
    keep.dedup();
    let mut total: i64 = 4_000;
    let mut strings: Vec<usize> = Vec::new();
    for i in keep {
        for b in norm(&h[i]) {
            total += 10;
            match kind(&b) {
                "image" => total += 1_500,
                "thinking" | "redacted_thinking" | "tool_use" => total += blob(&b).len() as i64 / 4,
                "text" => strings.push(b["text"].as_str().unwrap_or("").len()),
                "tool_result" => strings.push(result_text(&b).len()),
                _ => {}
            }
        }
    }
    strings.sort_unstable_by(|a, b| b.cmp(a));
    for (k, n) in strings.into_iter().enumerate() {
        total += if k < 8 && n >= 2_000 { (n / 8).max(800) as i64 / 4 + 30 } else { n as i64 / 4 };
    }
    total
}

/// One line for each message: its number, role and blocks, with sizes.
pub fn shape(h: &[Value]) -> Vec<String> {
    let mut tool: BTreeMap<String, String> = BTreeMap::new();
    for m in h {
        for b in norm(m) {
            if kind(&b) == "tool_use" {
                tool.insert(b["id"].as_str().unwrap_or("").into(), b["name"].as_str().unwrap_or("").into());
            }
        }
    }
    h.iter()
        .enumerate()
        .map(|(i, m)| {
            let blocks: Vec<String> = norm(m)
                .iter()
                .map(|b| match kind(b) {
                    "text" => format!("text({})", b["text"].as_str().unwrap_or("").len()),
                    "image" => "image".into(),
                    "thinking" => format!("thinking({})", b["thinking"].as_str().unwrap_or("").len()),
                    "redacted_thinking" => "redacted_thinking".into(),
                    "tool_use" => format!(
                        "tool_use({},{},{})",
                        b["id"].as_str().unwrap_or(""),
                        b["name"].as_str().unwrap_or(""),
                        blob(&b["input"]).len()
                    ),
                    "tool_result" => {
                        let id = b["tool_use_id"].as_str().unwrap_or("");
                        let err = if b["is_error"] == json!(true) { ",error" } else { "" };
                        format!(
                            "tool_result({id},{},{}{err})",
                            tool.get(id).map_or("?", String::as_str),
                            result_text(b).len()
                        )
                    }
                    other => other.to_string(),
                })
                .collect();
            format!("m{:05} {} [{}]", i + 1, role(m), blocks.join(", "))
        })
        .collect()
}

// ---- text ----

pub fn alpha(rng: &mut Rng, n: usize) -> String {
    (0..n).map(|_| ALPHA[rng.below(ALPHA.len())] as char).collect()
}

/// Words joined by spaces, cut at `n` chars.
pub fn filler(rng: &mut Rng, n: usize) -> String {
    let mut s = String::new();
    while s.len() < n {
        s.push_str(WORDS[rng.below(WORDS.len())]);
        s.push(' ');
    }
    s.truncate(n);
    s
}

struct Gen<'a> {
    rng: Rng,
    h: Vec<Value>,
    paths: Vec<String>,
    files: BTreeMap<String, Vec<String>>,
    read: Vec<String>,
    ids: usize,
    recall: bool,
    opts: &'a Opts,
    step_no: usize,
}

/// One call's result: the `tool_use` block, its output and its tool.
struct Call {
    id: String,
    name: &'static str,
    input: Value,
    out: String,
}

impl Gen<'_> {
    fn word(&mut self) -> &'static str {
        WORDS[self.rng.below(WORDS.len())]
    }

    fn line(&mut self) -> String {
        let want = self.rng.range(40, 69);
        let mut s = String::new();
        while s.len() < want {
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(self.word());
            if self.rng.one_in(4) {
                let n = self.rng.below(1000);
                s.push_str(&format!("_{n}"));
            }
        }
        s
    }

    fn sentence(&mut self) -> String {
        let n = self.rng.range(8, 16);
        let mut s: String = (0..n).map(|_| self.word()).collect::<Vec<_>>().join(" ");
        s[..1].make_ascii_uppercase();
        s.push('.');
        s
    }

    /// Sentences joined by spaces, at least `lo` chars where it can and at most `hi`.
    fn prose(&mut self, lo: usize, hi: usize) -> String {
        let want = self.rng.range(lo, hi);
        let mut s = String::new();
        while s.len() < want {
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(&self.sentence());
        }
        s.truncate(want);
        s
    }

    fn path(&mut self) -> String {
        self.paths[self.rng.below(self.paths.len())].clone()
    }

    fn size_class(&mut self, large_used: &mut bool) -> usize {
        if !*large_used && self.rng.one_in(12) {
            *large_used = true;
            self.rng.range(400, 900)
        } else if self.rng.one_in(3) {
            self.rng.range(40, 200)
        } else {
            self.rng.range(1, 15)
        }
    }

    fn lines_of(&mut self, n: usize) -> Vec<String> {
        (0..n).map(|_| self.line()).collect()
    }

    fn ensure_file(&mut self, p: &str, large_used: &mut bool) {
        if !self.files.contains_key(p) {
            let n = self.size_class(large_used);
            let lines = self.lines_of(n);
            self.files.insert(p.to_string(), lines);
        }
    }

    fn numbered(lines: &[String], from: usize) -> String {
        lines.iter().enumerate().map(|(i, l)| format!("{:>6}\t{l}", from + i + 1)).collect::<Vec<_>>().join("\n")
    }

    fn next_id(&mut self) -> String {
        self.ids += 1;
        format!("toolu_{:05}", self.ids)
    }

    fn call(&mut self, large_used: &mut bool, writes: &mut usize) -> Call {
        let id = self.next_id();
        let r = self.rng.below(100);
        let mut name = match r {
            0..=29 => "Read",
            30..=51 => "Bash",
            52..=63 => "Edit",
            64..=73 => "Grep",
            74..=79 => "Write",
            80..=85 => "WebFetch",
            86..=91 => "recall",
            92..=96 => "note",
            _ => "Frobnicate",
        };
        if name == "Edit" && self.read.is_empty() {
            name = "Read";
        }
        if name == "Write" && *writes >= 2 {
            name = "Read";
        }
        if name == "recall" && (!self.recall || self.h.len() < 2) {
            name = "Read";
        }
        if name == "note" && !self.recall {
            name = "Bash";
        }
        if name == "recall" {
            let mut text = None;
            for _ in 0..4 {
                let n = self.rng.range(2, self.h.len());
                let t = recall_text(&self.h[n - 1]);
                if t.len() <= 8_000 {
                    text = Some((n, t));
                    break;
                }
            }
            match text {
                Some((n, out)) => {
                    return Call { id, name: "recall", input: json!({"message": format!("m{n:05}")}), out };
                }
                None => name = "Read",
            }
        }
        match name {
            "Read" => {
                let p = if !self.read.is_empty() && self.rng.one_in(3) {
                    self.read[self.rng.below(self.read.len())].clone()
                } else {
                    self.path()
                };
                self.ensure_file(&p, large_used);
                if !self.read.contains(&p) {
                    self.read.push(p.clone());
                }
                let out = Self::numbered(&self.files[&p], 0);
                Call { id, name: "Read", input: json!({"file_path": p}), out }
            }
            "Edit" => {
                let p = self.read[self.rng.below(self.read.len())].clone();
                let idx = self.rng.below(self.files[&p].len());
                let old = self.files[&p][idx].clone();
                let new = self.line();
                self.files.get_mut(&p).unwrap()[idx] = new.clone();
                let out = if self.rng.one_in(2) {
                    let lines = &self.files[&p];
                    let from = idx.saturating_sub(20);
                    let to = (idx + 20).min(lines.len());
                    format!("The file {p} has been updated. The file now:\n{}", Self::numbered(&lines[from..to], from))
                } else {
                    format!("The file {p} has been updated successfully.")
                };
                Call { id, name: "Edit", input: json!({"file_path": p, "old_string": old, "new_string": new}), out }
            }
            "Write" => {
                *writes += 1;
                let p = self.path();
                let cap = self.rng.range(100, 1_600);
                let mut lines = Vec::new();
                let mut size = 0;
                loop {
                    let l = self.line();
                    if size + l.len() + 1 > cap && !lines.is_empty() {
                        break;
                    }
                    size += l.len() + 1;
                    lines.push(l);
                }
                let content = lines.join("\n");
                self.files.insert(p.clone(), lines);
                Call {
                    id,
                    name: "Write",
                    input: json!({"file_path": p, "content": content}),
                    out: format!("File created successfully at: {p}"),
                }
            }
            "Bash" => self.bash(id, large_used),
            "Grep" => {
                let w = self.word();
                let n = self.size_class(large_used);
                let out = (0..n)
                    .map(|k| {
                        let (w2, l) = (self.word(), self.line());
                        format!("src/{w2}.rs:{}: {l}", k + 1)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Call { id, name: "Grep", input: json!({"pattern": w, "path": "src"}), out }
            }
            "WebFetch" => {
                let (a, b) = (self.word(), self.word());
                let n = self.size_class(large_used);
                let out = self.lines_of(n).join("\n");
                Call { id, name: "WebFetch", input: json!({"url": format!("https://docs.example.com/{a}/{b}")}), out }
            }
            "note" => {
                let (a, b) = (self.sentence(), self.sentence());
                Call {
                    id,
                    name: "note",
                    input: json!({"text": format!("Finding: {a}\nPlan: {b}")}),
                    out: "noted".into(),
                }
            }
            _ => Call { id, name: "Frobnicate", input: json!({"level": 3}), out: "frobnicated 3 widgets".into() },
        }
    }

    fn bash(&mut self, id: String, _large_used: &mut bool) -> Call {
        let kind = self.rng.below(6);
        let (cmd, out) = match kind {
            0 | 1 => {
                let n = self.rng.range(30, 80);
                let mut lines: Vec<String> = (0..n)
                    .map(|_| {
                        let w = self.word();
                        format!("   Compiling {w} v0.1.0 (/work/{w})")
                    })
                    .collect();
                if self.rng.one_in(3) {
                    let w = self.word();
                    lines.push(format!("error: could not compile {w}"));
                }
                ("cargo build".to_string(), lines.join("\n"))
            }
            2 | 3 => {
                let n = self.rng.range(3, 30);
                let bad = self.rng.below(n);
                let mut lines = vec![format!("running {n} tests")];
                for k in 0..n {
                    let (a, b) = (self.word(), self.word());
                    lines.push(format!("test {a}::{b} ... {}", if k == bad { "FAILED" } else { "ok" }));
                }
                lines.push(format!("test result: FAILED. {} passed; 1 failed", n - 1));
                ("cargo test -q".to_string(), lines.join("\n"))
            }
            4 => {
                let n = self.rng.range(1, 10);
                let mut lines = self.lines_of(n);
                lines.push("check: ok".into());
                ("./check.sh".to_string(), lines.join("\n"))
            }
            _ => {
                let n = self.rng.range(3, 20);
                let lines = self.lines_of(n);
                let p = format!("src/gen_{}.rs", self.step_no);
                let cmd = format!("cat > {p} <<'EOF'\n{}\nEOF", lines.join("\n"));
                self.files.insert(p, lines);
                (cmd, String::new())
            }
        };
        Call { id, name: "Bash", input: json!({"command": cmd}), out }
    }

    fn pending_call(&mut self) -> Call {
        let id = self.next_id();
        if self.rng.one_in(2) {
            let p = self.path();
            Call { id, name: "Read", input: json!({"file_path": p}), out: String::new() }
        } else {
            Call { id, name: "Bash", input: json!({"command": "cargo test -q"}), out: String::new() }
        }
    }

    fn use_block(c: &Call) -> Value {
        json!({"type": "tool_use", "id": c.id, "name": c.name, "input": c.input})
    }

    fn user_content(&mut self, text: String) -> Value {
        if self.rng.one_in(3) {
            Value::String(text)
        } else {
            json!([{"type": "text", "text": text}])
        }
    }

    fn step(&mut self, typed_left: &mut usize) {
        self.step_no += 1;
        let mut blocks: Vec<Value> = Vec::new();
        if self.rng.one_in(3) {
            let n = self.rng.range(100, 600);
            let thinking = self.prose(n, n);
            let n = self.rng.range(40, 120);
            let sig = alpha(&mut self.rng, n);
            blocks.push(json!({"type": "thinking", "thinking": thinking, "signature": sig}));
        } else if self.rng.one_in(20) {
            let n = self.rng.range(60, 200);
            let data = alpha(&mut self.rng, n);
            blocks.push(json!({"type": "redacted_thinking", "data": data}));
        }
        if self.rng.one_in(2) {
            let s = self.sentence();
            blocks.push(json!({"type": "text", "text": s}));
        }
        let n = self.rng.range(1, 3);
        let (mut large, mut writes) = (false, 0);
        let calls: Vec<Call> = (0..n).map(|_| self.call(&mut large, &mut writes)).collect();
        blocks.extend(calls.iter().map(Self::use_block));
        self.h.push(json!({"role": "assistant", "content": blocks}));
        let mut results: Vec<Value> = Vec::new();
        for c in &calls {
            if self.rng.one_in(15) {
                let w = self.word();
                results.push(json!({
                    "type": "tool_result", "tool_use_id": c.id, "is_error": true, "content": format!("Error: {w} not found")
                }));
            } else if self.rng.one_in(4) {
                results.push(json!({
                    "type": "tool_result", "tool_use_id": c.id, "content": [{"type": "text", "text": c.out}]
                }));
            } else {
                results.push(json!({"type": "tool_result", "tool_use_id": c.id, "content": c.out}));
            }
        }
        if self.rng.one_in(6) {
            results.reverse();
        }
        if self.opts.typed_in_results && self.rng.one_in(8) {
            let p = self.path();
            results.push(json!({"type": "text", "text": format!("also check {p}")}));
        }
        self.h.push(json!({"role": "user", "content": results}));
        if *typed_left > 0 && self.rng.one_in(8) {
            *typed_left -= 1;
            let p = self.path();
            self.h.push(json!({"role": "user", "content": format!("Also look at {p}.")}));
        }
    }

    fn request(&mut self, first: bool) -> Value {
        if first {
            let verb = VERBS[self.rng.below(8)];
            let (w, p) = (self.word(), self.path());
            let mut text = format!("{verb} the {w} module in {p}.");
            for _ in 0..self.rng.range(0, 4) {
                let s = self.sentence();
                text.push(' ');
                text.push_str(&s);
            }
            text.truncate(1_200);
            return if self.rng.one_in(3) {
                json!({"role": "user", "content": text})
            } else {
                json!({"role": "user", "content": [{"type": "text", "text": text}]})
            };
        }
        let verb = VERBS[self.rng.below(8)].to_lowercase();
        let (w1, w2) = (self.word(), self.word());
        let p = if self.rng.one_in(2) && !self.read.is_empty() {
            self.read[self.rng.below(self.read.len())].clone()
        } else {
            self.path()
        };
        let mut text = format!("Now {verb} the {w1} {w2} in {p}.");
        for _ in 0..self.rng.range(0, 3) {
            let s = self.sentence();
            text.push(' ');
            text.push_str(&s);
        }
        let content = self.user_content(text);
        json!({"role": "user", "content": content})
    }
}

pub fn case(seed: u64, opts: &Opts) -> Case {
    let config = config(seed);
    let mut g = Gen {
        rng: Rng::new(seed),
        h: Vec::new(),
        paths: Vec::new(),
        files: BTreeMap::new(),
        read: Vec::new(),
        ids: 0,
        recall: config.recall,
        opts,
        step_no: 0,
    };
    // 12 paths on distinct words: 8 sources and 4 docs
    let mut order: Vec<usize> = (0..WORDS.len()).collect();
    for i in (1..order.len()).rev() {
        let j = g.rng.below(i + 1);
        order.swap(i, j);
    }
    for (k, &w) in order.iter().take(12).enumerate() {
        g.paths.push(if k < 8 { format!("src/{}.rs", WORDS[w]) } else { format!("docs/{}.md", WORDS[w]) });
    }
    let turns = g.rng.range(3, 5);
    for t in 0..turns {
        if t == 0 {
            let mut m0 = g.request(true);
            if seed % 3 == 0 {
                let mut blocks = norm(&m0);
                let n = g.rng.range(200, 400);
                let data = alpha(&mut g.rng, n);
                blocks.push(
                    json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": data}}),
                );
                m0 = json!({"role": "user", "content": blocks});
            }
            g.h.push(m0);
        } else if g.rng.one_in(5) {
            g.h.push(json!({"role": "user", "content": "[goal: continue]"}));
        } else {
            let m = g.request(false);
            g.h.push(m);
        }
        let mut typed_left = 2;
        for _ in 0..g.rng.range(2, 4) {
            g.step(&mut typed_left);
        }
        if t + 1 == turns && g.rng.one_in(4) {
            let n = g.rng.range(1, 3);
            let calls: Vec<Call> = (0..n).map(|_| g.pending_call()).collect();
            let blocks: Vec<Value> = calls.iter().map(Gen::use_block).collect();
            g.h.push(json!({"role": "assistant", "content": blocks}));
        } else {
            let s = g.prose(100, 600);
            g.h.push(json!({"role": "assistant", "content": [{"type": "text", "text": format!("Done: {s}")}]}));
        }
    }
    let h = g.h;
    let mut calls: Vec<usize> = (1..=h.len()).filter(|&len| role(&h[len - 1]) == "user").collect();
    if role(&h[h.len() - 1]) == "assistant" {
        calls.push(h.len());
    }
    Case { seed, config, history: h, calls }
}
