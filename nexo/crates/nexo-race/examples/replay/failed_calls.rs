use super::*;

/// What a failed tool call says went wrong, from the first lines of its output; `fail` when no clear cause.
pub(super) fn error_kind(out: &str, is_error: bool) -> Option<&'static str> {
    use std::sync::OnceLock;
    static KINDS: OnceLock<Vec<(&'static str, regex::Regex)>> = OnceLock::new();
    static ANY: OnceLock<regex::Regex> = OnceLock::new();
    let kinds = KINDS.get_or_init(|| {
        [
            ("glob", r"no matches found"),
            ("notool", r"command not found|is not recognized as|: not found\b|no such command|unknown command"),
            (
                "readonly",
                r"permission denied|read-only file system|operation not permitted|access is denied|EACCES|EROFS",
            ),
            ("missing", r"no such file|does not exist|cannot find|can't open|not a directory|ENOENT|file not found"),
            ("blocked", r"\bblocked\b|denied by|not allowed|was rejected"),
            ("timeout", r"timed out|timeout"),
        ]
        .into_iter()
        .map(|(k, p)| (k, regex::RegexBuilder::new(p).case_insensitive(true).build().unwrap()))
        .collect()
    });
    let any = ANY.get_or_init(|| {
        regex::RegexBuilder::new(
            r"exit code [1-9]|exit status [1-9]|traceback \(most recent|\berror[:\[]|\bfatal:|panicked at",
        )
        .case_insensitive(true)
        .build()
        .unwrap()
    });
    let head: String = out.chars().take(400).collect();
    if let Some((k, _)) = kinds.iter().find(|(_, r)| r.is_match(&head)) {
        return Some(k);
    }
    (is_error || any.is_match(&head)).then_some("fail")
}

/// The thing a call acts on: the path it names, else for a command the path its error names, the
/// program it says is missing, or the program it runs.
fn entity_of(name: &str, input: &Value, out: &str, kind: &str) -> String {
    for k in ["file_path", "path", "notebook_path"] {
        if let Some(p) = input[k].as_str() {
            return p.to_string();
        }
    }
    let Some(cmd) = input["command"].as_str() else {
        let key = input.as_object().and_then(|m| m.values().find_map(Value::as_str)).unwrap_or("");
        return format!("{name}:{}", key.chars().take(80).collect::<String>());
    };
    use std::sync::OnceLock;
    static PATH: OnceLock<regex::Regex> = OnceLock::new();
    static TOOL: OnceLock<regex::Regex> = OnceLock::new();
    let head: String = out.chars().take(400).collect();
    if matches!(kind, "missing" | "readonly") {
        let re = PATH.get_or_init(|| regex::Regex::new(r#"(?:^|[\s'"`:(])((?:~|\.{1,2})?/[^\s'"`:,)]+)"#).unwrap());
        if let Some(c) = re.captures(&head) {
            return c[1].to_string();
        }
    }
    if kind == "notool" {
        let re = TOOL.get_or_init(|| {
            regex::Regex::new(
                r"command not found: (\S+)|(\S+): command not found|(\S+): not found|'(\S+)' is not recognized",
            )
            .unwrap()
        });
        if let Some(c) = re.captures(&head) {
            if let Some(m) = c.iter().skip(1).flatten().next() {
                return format!("$ {}", m.as_str());
            }
        }
    }
    if kind == "glob" {
        return "$ zsh glob".into();
    }
    let skip = ["sudo", "pkexec", "nice", "timeout", "env", "time", "command", "exec", "nohup"];
    for seg in cmd.split(['\n', ';', '|', '&']) {
        let seg = seg.trim();
        if seg.is_empty() || seg.starts_with("cd ") || seg == "cd" {
            continue;
        }
        let prog = seg.split_whitespace().find(|t| {
            !t.contains('=') && !skip.contains(t) && !t.starts_with('-') && !t.chars().all(|c| c.is_ascii_digit())
        });
        if let Some(p) = prog {
            return format!("$ {}", p.rsplit('/').next().unwrap_or(p));
        }
    }
    format!("$ {}", cmd.chars().take(40).collect::<String>())
}

/// A failure the session met on one entity, as a fact the curator would keep.
pub(super) struct Fact {
    pub(super) kind: &'static str,
    /// The first line of the failed output: how the view is searched for it.
    pub(super) head: String,
    pub(super) call: usize,
}

/// Repeated failures on the same entity in one session, and whether the first was still in the context
/// the model had when it repeated the call (`Fable Q4`).
#[derive(Default)]
pub(super) struct Errs {
    pub(super) calls: usize,
    pub(super) errors: usize,
    pub(super) facts: HashMap<String, Fact>,
    /// call id -> the tool, its input and the call's number in the session
    pub(super) pending: HashMap<String, (String, Value, usize)>,
    pub(super) repeats: usize,
    pub(super) in_view: usize,
    pub(super) in_sent: usize,
    pub(super) labeled: usize,
    pub(super) gap: [usize; 3],
    /// Repeats 30 or more calls after the first, with the first out of the view.
    pub(super) late_out: usize,
    /// A call on an entity with a live fact that then worked: the fact, shown, would have misled.
    pub(super) stale_by: std::collections::BTreeMap<&'static str, usize>,
    /// Facts of a clear cause (not `fail`) alive at each request, summed.
    pub(super) live_labeled: usize,
    /// Calls on an entity with a live fact (where a footer would have shown it).
    pub(super) shown: usize,
    /// Facts alive at each request, summed (a fact is about 20 tokens).
    pub(super) live: usize,
    pub(super) by_kind: std::collections::BTreeMap<&'static str, usize>,
    /// Machine facts (a missing program, the shell's glob, a blocked call), first seen in this session.
    pub(super) machine: Vec<String>,
}

pub(super) fn has_line(s: &Sending, line: &str) -> bool {
    !line.is_empty() && s.msgs.iter().any(|m| text_of(m).contains(line))
}

impl Errs {
    /// The new messages of a request: calls of the reply (made from `view`, `sent`), results after them.
    pub(super) fn see(&mut self, new: &[Value], view: &Sending, sent: Option<&Sending>) {
        for m in new {
            for b in blocks_of(m) {
                if b["type"] == "tool_use" {
                    self.calls += 1;
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    let name = b["name"].as_str().unwrap_or("").to_string();
                    self.pending.insert(id, (name, b["input"].clone(), self.calls));
                } else if b["type"] == "tool_result" {
                    let id = b["tool_use_id"].as_str().unwrap_or("");
                    let Some((name, input, call)) = self.pending.remove(id) else { continue };
                    let out = text_of(&json!({"content": [b.clone()]}));
                    let kind = error_kind(&out, b["is_error"] == true);
                    let entity = entity_of(&name, &input, &out, kind.unwrap_or(""));
                    // the fact alive when the call was made, and whether its line was in what the model saw
                    if let Some(f) = self.facts.get(&entity) {
                        self.shown += 1;
                        let (in_view, in_sent) = (has_line(view, &f.head), sent.is_some_and(|s| has_line(s, &f.head)));
                        match kind {
                            Some(k) if k == f.kind => {
                                self.repeats += 1;
                                self.in_view += usize::from(in_view);
                                self.in_sent += usize::from(in_sent);
                                self.labeled += usize::from(k != "fail");
                                let gap = call - f.call;
                                self.gap[if gap < 5 {
                                    0
                                } else if gap < 30 {
                                    1
                                } else {
                                    2
                                }] += 1;
                                self.late_out += usize::from(gap >= 30 && !in_view);
                                *self.by_kind.entry(k).or_default() += 1;
                            }
                            None => *self.stale_by.entry(f.kind).or_default() += 1,
                            _ => {}
                        }
                    }
                    match kind {
                        Some(k) => {
                            self.errors += 1;
                            let head = out
                                .lines()
                                .map(str::trim)
                                .find(|l| l.len() >= 12)
                                .unwrap_or("")
                                .chars()
                                .take(120)
                                .collect();
                            let fact = format!("{k} {entity}");
                            if matches!(k, "notool" | "glob" | "blocked") && !self.machine.contains(&fact) {
                                self.machine.push(fact);
                            }
                            self.facts.insert(entity, Fact { kind: k, head, call });
                        }
                        None => {
                            self.facts.remove(&entity);
                        }
                    }
                }
            }
        }
        self.live += self.facts.len();
        self.live_labeled += self.facts.values().filter(|f| f.kind != "fail").count();
    }

    pub(super) fn json(&self) -> Value {
        json!({
            "calls": self.calls, "errors": self.errors, "repeats": self.repeats, "in_view": self.in_view,
            "in_sent": self.in_sent, "labeled": self.labeled, "gap": self.gap, "late_out": self.late_out,
            "stale_by": self.stale_by, "live_labeled": self.live_labeled, "shown": self.shown, "live": self.live, "by_kind": self.by_kind, "machine": self.machine,
        })
    }
}
