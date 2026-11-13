use super::*;

/// How much a fact bears on the task: the task's words it holds, its names thrice.
pub(super) fn bearing(text: &str, q: &BTreeSet<String>, qn: &BTreeSet<String>) -> usize {
    let t = terms(text);
    t.intersection(q).count() + 2 * t.intersection(qn).count()
}

/// A request longer than this is a pasted document or a summary, not a rule to keep.
pub(super) const RULE_REQUEST: usize = 1500;

#[derive(Debug, Default)]
pub(super) struct FileNote {
    /// The lines of the latest read, and whether they still match the disk.
    pub(super) read: Option<(usize, usize)>,
    pub(super) current: Option<bool>,
    pub(super) read_at: usize,
    pub(super) edits: usize,
    pub(super) written: bool,
    pub(super) changed_at: usize,
    /// Read in a call of several files, whose lines are not told apart.
    pub(super) read_in_many: bool,
    pub(super) last: usize,
}

/// Facts of the conversation, as lines.
#[derive(Debug, Default)]
pub(super) struct Ledger {
    pub(super) request: Option<String>,
    /// The message of the latest request when the ledger states it whole.
    pub(super) request_at: Option<usize>,
    pub(super) constraints: Vec<String>,
    pub(super) files: Vec<String>,
    pub(super) failing: Vec<String>,
    pub(super) plan: Vec<String>,
}

pub(super) const MARKERS: &[&str] = &[
    "don't",
    "do not",
    "never",
    "always",
    "must",
    "only",
    "avoid",
    "without",
    "keep ",
    "prefer",
    "make sure",
    "no ",
    "nunca",
    "siempre",
    "sin ",
    "solo ",
];

pub(super) fn short(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        s
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

impl Ledger {
    /// The facts of `history`, those that bear on `task` first.
    pub(super) fn of(history: &[Message], cwd: &Path, check: bool, task: &str) -> Ledger {
        let us = units(history);
        let (q, qn) = (terms(task), names(task));
        let mut l = Ledger::default();
        let requests: Vec<&Unit> = us.iter().filter(|u| u.kind == Kind::Person).collect();
        if let Some(last) = requests.last() {
            if last.text.trim() != task.trim() {
                let r = short(&last.text, 500);
                if r == short(&last.text, usize::MAX) {
                    l.request_at = Some(last.msg);
                }
                l.request = Some(r);
            }
        }
        let mut rules: Vec<(usize, String)> = vec![];
        for r in requests.iter().rev().skip(1).filter(|r| r.text.len() <= RULE_REQUEST) {
            for sentence in r.text.split(['\n', ';']).flat_map(|s| s.split(". ")) {
                let low = sentence.to_lowercase();
                let s = short(sentence, 200);
                if s.len() >= 8 && MARKERS.iter().any(|m| low.contains(m)) && !rules.iter().any(|(_, x)| *x == s) {
                    rules.push((bearing(&s, &q, &qn), s));
                }
            }
            if rules.len() >= 16 {
                break;
            }
        }
        // those that bear on the task first, the latest first among equals
        rules.sort_by_key(|a| std::cmp::Reverse(a.0));
        l.constraints = rules.into_iter().take(6).map(|(_, s)| s).collect();
        // files
        let mut files: BTreeMap<String, FileNote> = BTreeMap::new();
        for u in &us {
            if u.kind == Kind::Result && u.tool == "read" && !u.error {
                for p in &u.files {
                    let f = files.entry(p.clone()).or_default();
                    f.last = f.last.max(u.msg);
                    f.read_in_many = true;
                    f.read_at = u.msg;
                }
            }
            let Some(p) = &u.path else { continue };
            let f = files.entry(p.clone()).or_default();
            f.last = f.last.max(u.msg);
            match (u.kind, u.tool.as_str()) {
                (Kind::Result, "read") if !u.error => {
                    if let Some(r) = range(&u.text) {
                        f.read = Some(r);
                        f.read_at = u.msg;
                        f.current = if check { current(&u.text, &resolve(cwd, p)) } else { None };
                    }
                }
                // a write counts unless its result failed (one still running counts)
                (Kind::Call, t) if writes_file(t) && !u.error => {
                    if t == "write" {
                        f.written = true;
                    } else {
                        f.edits += 1;
                    }
                    f.changed_at = u.msg;
                }
                _ => {}
            }
        }
        // files the task names first, then the latest
        let mut order: Vec<(&String, &FileNote)> = files.iter().collect();
        order.sort_by(|a, b| {
            let (x, y) = (bearing(a.0, &q, &qn) > 0, bearing(b.0, &q, &qn) > 0);
            y.cmp(&x).then(b.1.last.cmp(&a.1.last)).then(a.0.cmp(b.0))
        });
        for (p, f) in order.into_iter().take(12) {
            let mut what = vec![];
            if let Some((a, b)) = f.read {
                let state = if f.changed_at > f.read_at {
                    " (changed after)".to_string()
                } else {
                    match f.current {
                        Some(true) => " (current)".into(),
                        Some(false) => " (changed on disk since)".into(),
                        None => String::new(),
                    }
                };
                what.push(format!("read lines {a}-{b}{state}"));
            }
            if f.read.is_none() && f.read_in_many {
                what.push(if f.changed_at > f.read_at { "read (changed after)" } else { "read" }.into());
            }
            if f.written {
                what.push("written".into());
            }
            if f.edits > 0 {
                what.push(format!("edited {}x", f.edits));
            }
            if !what.is_empty() {
                l.files.push(format!("- {}: {}", shown(cwd, p), what.join("; ")));
            }
        }
        // failing commands: an error with no later success of the same command
        let mut last: BTreeMap<&str, &Unit> = BTreeMap::new();
        for u in us.iter().filter(|u| u.kind == Kind::Result && u.tool == "bash") {
            if let Some(c) = &u.command {
                last.insert(c.as_str(), u);
            }
        }
        // still failing: those that bear on the task, and the latest one since the person's last request
        let since = requests.last().map(|r| r.msg).unwrap_or(0);
        let mut failing: Vec<(usize, &Unit)> = last
            .into_values()
            .filter(|u| u.error)
            .map(|u| {
                (
                    bearing(
                        &format!("{}\n{}", u.command.as_deref().unwrap_or(""), error_lines(&u.text).join("\n")),
                        &q,
                        &qn,
                    ),
                    u,
                )
            })
            .collect();
        failing.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.msg.cmp(&a.1.msg)));
        let latest = failing.iter().map(|(_, u)| u.msg).max().filter(|m| *m > since);
        for (_, u) in failing.into_iter().filter(|(b, u)| *b > 0 || Some(u.msg) == latest).take(3) {
            let code = u.text.rsplit_once("[exit code ").and_then(|(_, r)| r.split(']').next());
            let code = code.map(|c| format!("exit {c}")).unwrap_or_else(|| "failed".into());
            let mut out = format!("- `{}` ({code})", short(u.command.as_deref().unwrap_or(""), 120));
            for line in error_lines(&u.text) {
                out.push_str(&format!("\n    {line}"));
            }
            l.failing.push(out);
        }
        // the plan
        if let Some(items) = history
            .iter()
            .flat_map(|m| m.tool_calls())
            .filter(|(_, n, _)| *n == "todo")
            .filter_map(|(_, _, a)| a.get("items").and_then(Value::as_array))
            .last()
        {
            for it in items.iter().take(12) {
                let text = it.get("text").and_then(Value::as_str).unwrap_or("");
                let status = it.get("status").and_then(Value::as_str).unwrap_or("pending");
                let mark = match status {
                    "done" | "completed" => "x",
                    "in_progress" => "~",
                    _ => " ",
                };
                if !text.is_empty() {
                    l.plan.push(format!("- [{mark}] {}", short(text, 120)));
                }
            }
        }
        l
    }

    /// The ledger within `budget` tokens, and how many facts it holds.
    pub(super) fn render(&self, budget: usize) -> (String, usize) {
        let mut out = String::new();
        let mut facts = 0;
        let fits = |out: &str, add: &str| tokens(out) + tokens(add) <= budget;
        if let Some(r) = &self.request {
            let line = format!("The person's latest request: {r}\n");
            if fits(&out, &line) {
                out.push_str(&line);
            }
        }
        let sections: [(&str, &Vec<String>); 4] = [
            ("Constraints the person gave earlier:", &self.constraints),
            ("Files (latest first):", &self.files),
            ("Failing commands:", &self.failing),
            ("Plan:", &self.plan),
        ];
        for (title, items) in sections {
            let mut body = String::new();
            for it in items {
                let it = if title.starts_with("Constraints") { format!("- {it}") } else { it.clone() };
                let add = format!("{it}\n");
                if !fits(&format!("{out}{title}\n{body}"), &add) {
                    break;
                }
                body.push_str(&add);
                facts += 1;
            }
            if !body.is_empty() {
                out.push_str(&format!("{title}\n{body}"));
            }
        }
        (out, facts)
    }
}
