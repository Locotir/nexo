//! A synthetic session generator, for stress and invariants only (`SIMULATOR-SPEC.md` 6).
//!
//! It writes a golden fixture (`{"now", "req": {"op": "view", ...}}` lines, one request per line, each a
//! delta on the previous one) in which the agent reads files (hex dumps, headers and JSON bodies, 600-line
//! logs, source, prose) and, with probability `recite`, quotes a planted word of every chunk again some
//! steps later. The planted words are the only distinctive words an assistant message holds, so a quote
//! the curator's view lacks is exactly one `cited` event. The dependencies go to a side file
//! (`X.synth.deps.json`). Nothing fitted, weighed or searched may use these files: `refuse` says so.

use serde_json::{json, Value};

use super::fit::Rng;

/// The families the generator knows.
pub const FAMILIES: [&str; 6] = ["binary", "asm", "http", "logs", "code", "chat"];

/// Parameters of one generated session.
#[derive(Debug, Clone)]
pub struct Synth {
    pub seed: u64,
    /// Requests (the file has one line per request).
    pub steps: usize,
    pub family: String,
    /// Distinct files the agent reads (a file is read again at another offset).
    pub files: usize,
    /// Share of chunks that are dumps (hex for `binary`, wire format for `http`).
    pub hex_share: f64,
    /// Share of chunks that are 600-line logs.
    pub log_share: f64,
    /// Mean of the geometric lag between a chunk and the step that quotes it again.
    pub lag_mean: f64,
    /// Probability that a chunk is quoted again.
    pub recite: f64,
}

impl Synth {
    pub fn new(seed: u64, steps: usize, family: &str) -> Synth {
        Synth {
            seed,
            steps,
            family: family.to_string(),
            files: 8,
            hex_share: 0.5,
            log_share: 0.1,
            lag_mean: 6.0,
            recite: 0.5,
        }
    }
}

/// A planted dependency: a word only one chunk holds, quoted by a later reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub word: String,
    pub file: usize,
    /// The request (1-based) that first holds the chunk, and the message of the history that holds it.
    pub chunk_request: usize,
    pub chunk_mi: usize,
    /// The request (1-based) whose recorded reply quotes the word.
    pub cite_request: usize,
}

/// A generated session: the fixture text and its planted dependencies.
pub struct Generated {
    pub jsonl: String,
    pub deps: Vec<Dep>,
}

impl Generated {
    pub fn deps_json(&self) -> Value {
        Value::Array(
            self.deps
                .iter()
                .map(|d| {
                    json!({"word": d.word, "file": d.file, "chunk_request": d.chunk_request,
                        "chunk_mi": d.chunk_mi, "cite_request": d.cite_request})
                })
                .collect(),
        )
    }
}

/// Synthetic files are for stress only: fit, search and report call this on every input.
pub fn refuse(path: &str) -> Result<(), String> {
    if path.ends_with(".synth.jsonl") || path.ends_with(".synth.deps.json") {
        Err(format!("{path}: a synthetic session is for stress tests only; fit, search and report refuse it"))
    } else {
        Ok(())
    }
}

fn unit(r: &mut Rng) -> f64 {
    (r.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dump,
    Log,
    Plain,
}

const WORDS: [&str; 12] = [
    "checking",
    "region",
    "against",
    "earlier",
    "output",
    "comparing",
    "continue",
    "reading",
    "following",
    "next",
    "section",
    "carefully",
];

fn prose(r: &mut Rng, lines: usize) -> String {
    let mut s = String::new();
    for _ in 0..lines {
        let n = 8 + r.below(6);
        let line: Vec<&str> = (0..n).map(|_| WORDS[r.below(WORDS.len())]).collect();
        s += &line.join(" ");
        s.push('\n');
    }
    s
}

/// A chunk of `kind` for `family`, with `word` on one line of it.
fn chunk(family: &str, kind: Kind, n: usize, word: &str, r: &mut Rng) -> String {
    let (lines, at) = match kind {
        Kind::Dump => (256, 0),
        Kind::Log => (600, 0),
        Kind::Plain => (120, 0),
    };
    let _ = at;
    let mark = r.below(lines);
    let mut s = String::new();
    for i in 0..lines {
        let line = match (family, kind) {
            ("binary", Kind::Dump) => {
                let mut l = format!("{:08x}:", 0x0040_0000u64 + (n as u64) * 0x1_0000 + i as u64 * 16);
                for _ in 0..8 {
                    l += &format!(" {:04x}", r.next_u64() & 0xffff);
                }
                l += "  ";
                for _ in 0..16 {
                    l.push((b'a' + (r.next_u64() % 26) as u8) as char);
                }
                l
            }
            ("http", Kind::Dump) => match i % 6 {
                0 => "HTTP/1.1 200 OK".to_string(),
                1 => "Content-Type: application/json".to_string(),
                2 => format!("Set-Cookie: sid={:016x}; Path=/", r.next_u64()),
                3 => format!("Authorization: Bearer {:016x}", r.next_u64()),
                4 => String::new(),
                _ => format!("{{\"id\":{},\"name\":\"item {}\",\"ok\":true}}", r.next_u64() % 1_000_000, i),
            },
            ("http", Kind::Log) => format!(
                "127.0.0.1 - - [08/Nov/2026:12:{:02}:{:02} +0000] \"GET /api/items/{} HTTP/1.1\" 200 {}",
                i / 60 % 60,
                i % 60,
                r.next_u64() % 100_000,
                r.next_u64() % 9000
            ),
            (_, Kind::Log) => {
                format!("test suite::case_{}_{} ... ok", n, i)
            }
            ("asm", _) => match i % 4 {
                0 => format!("L{}_{}:", n, i),
                1 => format!("    mov rax, [rbp-{}]", 8 + 8 * (i % 16)),
                2 => "    add rax, rbx".to_string(),
                _ => format!("    cmp rax, {}", r.next_u64() % 4096),
            },
            ("binary", _) => {
                let w: String = (0..8 + r.below(10)).map(|_| (b'a' + (r.next_u64() % 26) as u8) as char).collect();
                w
            }
            ("code", _) | ("logs", _) => format!("    let value = compute(arg, {});", r.next_u64() % 9999),
            _ => prose(r, 1).trim_end().to_string(),
        };
        if i == mark {
            let tag = match family {
                "binary" if kind == Kind::Dump => format!("{line}  <{word}>"),
                "asm" => format!("{word}:"),
                "http" => format!("{line} x-trace: {word}"),
                _ => format!("{line} // {word}"),
            };
            s += &tag;
        } else {
            s += &line;
        }
        s.push('\n');
    }
    s
}

/// The command (tool and input) that produces a chunk.
fn call(family: &str, kind: Kind, file: usize, off: u64) -> (&'static str, Value) {
    let f = format!("f{file:02}");
    match (family, kind) {
        ("binary", Kind::Dump) => ("bash", json!({"command": format!("xxd -s 0x{:x} -l 0x1000 {f}", off % 0x800)})),
        ("binary", Kind::Plain) => ("bash", json!({"command": format!("strings {f}")})),
        ("http", Kind::Dump) => ("bash", json!({"command": format!("curl -si localhost -H 'x: {f}'")})),
        ("asm", _) => ("read", json!({"path": f, "offset": off % 0x800})),
        ("code", _) => ("read", json!({"path": f, "offset": off % 0x800})),
        (_, Kind::Log) => ("bash", json!({"command": format!("tail -n 600 {f}")})),
        ("logs", _) => ("bash", json!({"command": format!("cargo test {f}")})),
        _ => ("read", json!({"path": f, "offset": off % 0x800})),
    }
}

/// Generates the session of `p`.
pub fn generate(p: &Synth) -> Generated {
    let mut r = Rng::new(p.seed ^ 0x5eed_5eed);
    let steps = p.steps.max(2);
    let family = p.family.as_str();
    let tag: String = family.chars().take(3).collect();
    // chunk k (0-based) is in the result of request k + 2; it may be quoted by the reply of a later request
    let mut cites: Vec<Option<usize>> = vec![None; steps + 2];
    let mut deps = Vec::new();
    let mut words = vec![String::new(); steps + 2];
    #[allow(clippy::needless_range_loop)]
    for line in 2..=steps {
        words[line] = format!("zq_{tag}_{:04}", line);
        if unit(&mut r) < p.recite {
            // geometric lag with the mean asked for, at least one request after the chunk's own
            let q = 1.0 / p.lag_mean.max(1.0);
            let mut lag = 0;
            while unit(&mut r) > q && lag < steps {
                lag += 1;
            }
            let cite_line = line + 1 + lag;
            if cite_line <= steps {
                deps.push(Dep {
                    word: words[line].clone(),
                    file: (line - 2) % p.files.max(1),
                    chunk_request: line,
                    chunk_mi: 2 * line - 2,
                    cite_request: cite_line - 1,
                });
                // one quote per reply: a later free slot, or the next one
                let mut at = cite_line;
                while at <= steps && cites[at].is_some() {
                    at += 1;
                }
                if at <= steps {
                    cites[at] = Some(line);
                    deps.last_mut().unwrap().cite_request = at - 1;
                } else {
                    deps.pop();
                }
            }
        }
    }
    let mut out = String::new();
    let mut total = 0usize;
    for line in 1..=steps {
        let now = 1_750_000_000u64 + 2 * (line as u64 - 1);
        let new: Vec<Value> = if line == 1 {
            vec![json!({"role": "user", "content": match family {
                "chat" => "Let us go through my notes on the project, one topic at a time.",
                _ => "Work through the files one by one and report what each one holds.",
            }})]
        } else {
            let file = (line - 2) % p.files.max(1);
            let u = unit(&mut r);
            let kind = if u < p.hex_share {
                Kind::Dump
            } else if u < p.hex_share + p.log_share {
                Kind::Log
            } else {
                Kind::Plain
            };
            let said = match cites[line] {
                Some(src) => {
                    format!("This matches {} from the earlier region, so I continue with the next one.", words[src])
                }
                None => "Checking the next region and comparing it with what I saw before.".to_string(),
            };
            let body = chunk(family, kind, line, &words[line], &mut r);
            if family == "chat" {
                vec![json!({"role": "assistant", "content": said}), json!({"role": "user", "content": body})]
            } else {
                let (name, input) = call(family, kind, file, r.next_u64());
                let id = format!("s{line}");
                vec![
                    json!({"role": "assistant", "content": [
                        {"type": "text", "text": said},
                        {"type": "tool_use", "id": id, "name": name, "input": input},
                    ]}),
                    json!({"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": id, "content": body},
                    ]}),
                ]
            }
        };
        let base = if line == 1 { 0 } else { 2 * line - 3 };
        total = base + new.len();
        let rec =
            json!({"now": now, "req": {"op": "view", "system_tokens": 500, "base": base, "new": new, "total": total}});
        out += &rec.to_string();
        out.push('\n');
    }
    let _ = total;
    Generated { jsonl: out, deps }
}

/// The side file of `out` (`X.synth.jsonl` gives `X.synth.deps.json`).
pub fn deps_path(out: &str) -> String {
    format!("{}.deps.json", out.strip_suffix(".jsonl").unwrap_or(out))
}

/// Generates the session and writes the fixture to `out` and the dependencies next to it.
pub fn write(p: &Synth, out: &str) -> Result<Generated, String> {
    let g = generate(p);
    std::fs::write(out, &g.jsonl).map_err(|e| format!("{out}: {e}"))?;
    let d = deps_path(out);
    std::fs::write(&d, serde_json::to_string_pretty(&g.deps_json()).unwrap() + "\n")
        .map_err(|e| format!("{d}: {e}"))?;
    Ok(g)
}
