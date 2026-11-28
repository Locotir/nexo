//! The family tagger (`SIMULATOR-SPEC.md` 7.2): which kind of work a session is, by the share of result
//! tokens each kind of command produced.
//!
//! Each tool result goes to one family, first match in priority order: `binary` (the command runs a
//! debugger or a dumper), `asm` (the result is an assembly listing), `http` (a request tool or the wire
//! format in the result), `logs` (a build or test command, an error marker, or a result of over 200 lines),
//! `code` (read, search and edit tools). The family with the largest share of result tokens wins; ties go
//! to the earlier one. A session with no tool result at all is `chat`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::{json, Value};

use super::events::Tool;
use super::input::{text_of, Request};
use super::price::tokens_of;

/// The families, in priority order (the order ties are broken in).
pub const FAMILIES: [&str; 6] = ["binary", "asm", "http", "logs", "code", "chat"];

/// Results with more lines than this count as logs when no command says otherwise.
pub const LONG_RESULT_LINES: usize = 200;

struct Patterns {
    binary: regex::Regex,
    http_cmd: regex::Regex,
    http_body: regex::Regex,
    logs_cmd: regex::Regex,
    logs_body: regex::Regex,
    asm_line: regex::Regex,
    asm_path: regex::Regex,
    dump_line: regex::Regex,
    read_cmd: regex::Regex,
    numbered: regex::Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        binary: regex::Regex::new(
            r"\b(gdb|objdump|readelf|xxd|hexdump|strings|nm|ltrace|strace|r2|radare2|disas\w*)\b|\bod\s",
        )
        .unwrap(),
        http_cmd: regex::Regex::new(r"\b(curl|wget|nc|ncat|socat)\b|\.har\b").unwrap(),
        http_body: regex::Regex::new(r"HTTP/1\.|HTTP/2|Content-Type:|Set-Cookie|Authorization:").unwrap(),
        logs_cmd: regex::Regex::new(
            r"cargo (test|build|check|run|clippy)|\bmake\b|pytest|npm (run |test)|go test|journalctl|tail -n|\bgcc\b|\bcc\b",
        )
        .unwrap(),
        logs_body: regex::Regex::new(r"error\[|warning:|FAILED|test result:").unwrap(),
        asm_line: regex::Regex::new(
            r"^\s*(?:[0-9a-f]+:\s+(?:[0-9a-f]{2}\s)+\s*)?(?:mov\w*|lea\w*|push\w*|pop\w*|call\w*|jmp\w*|j[a-z]{1,3}|cmp\w*|add\w*|sub\w*|ret\w*|xor\w*|test\w*|and\w*|or\w*|shl\w*|shr\w*|sar\w*|imul\w*|idiv\w*|inc\w*|dec\w*|neg\w*|not\w*|nop\w*|cvt\w+|set[a-z]+|cmov\w+|leave|endbr64|ldr|str|stp|ldp|bl|b\.\w+)\b",
        )
        .unwrap(),
        asm_path: regex::Regex::new(r"(?i)\.(asm|s|nasm)\b|assembly|disassembl").unwrap(),
        dump_line: regex::Regex::new(r"^\s*(?:0x)?[0-9a-f]{4,16}(?: <[^>]*>)?:\s+(?:(?:0x)?[0-9a-f]{2,8}\s?){2,}").unwrap(),
        read_cmd: regex::Regex::new(r"(?s)\b(cat|nl|bat|sed -n|grep|rg)\b.*(\.(rs|py|c|h|cc|cpp|go|ts|js|java|toml)\b|src/|tests/)|(\.(rs|py|c|h|cc|cpp|go|ts|js|java|toml)\b|src/|tests/).*\b(cat|nl|bat|sed -n|grep|rg)\b").unwrap(),
        numbered: regex::Regex::new(r"^\s*\d+[│|:\t]\s?").unwrap(),
    })
}

/// The tokens of tool results by family, and their sum.
#[derive(Debug, Clone, Default)]
pub struct Shares {
    pub tokens: BTreeMap<&'static str, i64>,
    pub total: i64,
}

impl Shares {
    /// The share of the family in the result tokens (0 when there are none).
    pub fn share(&self, family: &str) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.tokens.get(family).copied().unwrap_or(0) as f64 / self.total as f64
    }
}

/// Whether most lines of a result are assembly instructions or labels.
fn looks_like_asm(text: &str) -> bool {
    let p = patterns();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() < 20 {
        return false;
    }
    let asm = lines
        .iter()
        .filter(|l| {
            p.asm_line.is_match(l)
                || (l.ends_with(':') && !l.contains(' '))
                || l.starts_with(".text")
                || l.starts_with(".globl")
                || l.starts_with(".section")
        })
        .count();
    asm * 10 >= lines.len() * 4
}

/// Whether most lines of a result are a disassembly or a hex dump (address, then machine bytes).
fn looks_like_dump(text: &str) -> bool {
    let p = patterns();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 20 {
        return false;
    }
    let hit = lines.iter().filter(|l| p.dump_line.is_match(&p.numbered.replace(l, ""))).count();
    hit * 10 >= lines.len() * 4
}

/// The family of one tool result: the tool and its call (`input`), and the text it returned.
pub fn classify(tool: &str, input: &Value, text: &str) -> Option<&'static str> {
    let p = patterns();
    let kind = Tool::of(tool);
    let call = match (kind, input.get("command").and_then(Value::as_str)) {
        (Tool::Bash, Some(c)) => c.to_string(),
        _ => input.to_string(),
    };
    if kind == Tool::Bash && p.binary.is_match(&call) {
        return Some("binary");
    }
    if looks_like_dump(text) {
        return Some("binary");
    }
    let reads = kind == Tool::Read || (kind == Tool::Bash && p.read_cmd.is_match(&call));
    if looks_like_asm(text) || (reads && p.asm_path.is_match(&call)) {
        return Some("asm");
    }
    if (kind == Tool::Bash && p.http_cmd.is_match(&call)) || p.http_body.is_match(text) {
        return Some("http");
    }
    let logs_cmd = kind == Tool::Bash && p.logs_cmd.is_match(&call);
    if kind == Tool::Bash && !logs_cmd && p.read_cmd.is_match(&call) && !p.logs_body.is_match(text) {
        return Some("code");
    }
    if logs_cmd || p.logs_body.is_match(text) || (kind != Tool::Read && text.lines().count() > LONG_RESULT_LINES) {
        return Some("logs");
    }
    let t = tool.to_ascii_lowercase();
    let code_tool = kind == Tool::Read
        || matches!(
            t.as_str(),
            "edit" | "write" | "str_replace" | "patch" | "apply_patch" | "grep" | "glob" | "find" | "ls" | "multi_edit"
        );
    code_tool.then_some("code")
}

/// The result tokens of the longest request of a session, by family.
pub fn shares(requests: &[Request]) -> Shares {
    let mut out = Shares::default();
    let Some(q) = requests.iter().max_by_key(|q| q.messages.len()) else { return out };
    let mut uses: BTreeMap<String, (String, Value)> = BTreeMap::new();
    for m in &q.messages {
        let Some(blocks) = m["content"].as_array() else { continue };
        for b in blocks {
            if b["type"] == "tool_use" {
                let id = b["id"].as_str().unwrap_or("").to_string();
                uses.insert(id, (b["name"].as_str().unwrap_or("").to_string(), b["input"].clone()));
            } else if b["type"] == "tool_result" {
                let Some((name, input)) = uses.get(b["tool_use_id"].as_str().unwrap_or("")) else { continue };
                let one = json!({"content": [b]});
                let t = tokens_of(&one);
                out.total += t;
                if let Some(f) = classify(name, input, &text_of(&one)) {
                    *out.tokens.entry(f).or_insert(0) += t;
                }
            }
        }
    }
    out
}

/// The family of a session: the largest share of result tokens, ties to the earlier family in
/// [`FAMILIES`]; `chat` when no tool was called; `code` when tools were called but nothing is told apart.
pub fn tag(requests: &[Request]) -> &'static str {
    let s = shares(requests);
    if s.total == 0 {
        // no tool result to go by: the first message may still name an assembly listing
        let first = requests.first().and_then(|q| q.messages.first()).map(text_of).unwrap_or_default();
        return if patterns().asm_path.is_match(&first) { "asm" } else { "chat" };
    }
    let tokens = |f: &str| s.tokens.get(f).copied().unwrap_or(0);
    let mut best = ("code", 0);
    for f in &FAMILIES[..5] {
        if tokens(f) > best.1 {
            best = (f, tokens(f));
        }
    }
    // a family that comes earlier in the order and is within a quarter of the leader takes a near tie,
    // except `code`, which is what reading source looks like in every session
    for f in &FAMILIES[..4] {
        if *f == best.0 {
            break;
        }
        if tokens(f) > 0 && tokens(f) * 4 >= best.1 * 3 {
            return f;
        }
    }
    best.0
}
