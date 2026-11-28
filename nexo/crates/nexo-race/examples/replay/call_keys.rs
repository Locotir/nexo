use super::*;

/// The latest result of a call key: where it is in the history, its call number, whether it failed, a
/// line of it and its size, with the file a read holds.
pub(super) struct Res {
    pub(super) mi: usize,
    pub(super) call: usize,
    pub(super) error: bool,
    pub(super) head: String,
    pub(super) tokens: i64,
    pub(super) path: Option<String>,
}

/// B3 of the theory: a call that repeats the key of an earlier one (the same tool on the same target: its
/// path, command, pattern or url, else its whole input, cut at 160 characters) whose latest result was out
/// of what the model saw when it repeated it. For each such repeat, the signals a curator could have read
/// at the request before, to bring that result back by key: the call before wrote the file the repeated
/// read holds (`write_fail` when the write failed, `write_ok` when not), it failed (`error`), or its key
/// was followed by this one earlier in the session (`bigram`). And the other way round, each time such a
/// signal fires with the result it points to out of the view and fresh: whether the call it predicts comes
/// within the next five, by its key or as a read of the same file, and the tokens the result holds.
#[derive(Default)]
pub(super) struct Keys {
    pub(super) calls: usize,
    /// call id -> (key, kind, file, call number)
    pub(super) pending: HashMap<String, (String, &'static str, Option<String>, usize)>,
    /// key -> its latest result
    pub(super) last: HashMap<String, Res>,
    /// file -> the key of the latest read of it
    pub(super) read_of: Vec<(String, String)>,
    /// file -> the call number of the latest write of it that worked
    pub(super) written: Vec<(String, usize)>,
    /// the call before: its key, whether it failed, its kind and file
    pub(super) prev: Option<(String, bool, &'static str, Option<String>)>,
    /// key -> the key that came after it last time
    pub(super) next_of: HashMap<String, String>,
    pub(super) repeats: usize,
    pub(super) out: usize,
    pub(super) out_err: usize,
    /// kind -> repeats, out of view, of those fresh, and the tokens of the fresh ones
    pub(super) by_kind: std::collections::BTreeMap<&'static str, [i64; 4]>,
    /// signal -> the out-of-view repeats it came before
    pub(super) signals: std::collections::BTreeMap<&'static str, usize>,
    /// trigger -> fires, hits by key, hits by file, tokens fired
    pub(super) cand: std::collections::BTreeMap<&'static str, [i64; 4]>,
    /// predictions open: trigger, predicted key, its file, the last call number that counts as a hit
    pub(super) open: Vec<(&'static str, String, Option<String>, usize)>,
    /// triggers the results of the last request armed, weighed against the view built after them
    pub(super) armed: Vec<(&'static str, String)>,
    pub(super) sent_tokens: i64,
}

/// (key, kind, file) of a call, the key as `calls.jq` of the lab makes it, with the tool's name in front.
pub(super) fn call_key(name: &str, input: &Value) -> (String, &'static str, Option<String>) {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    let target = ["file_path", "path", "command", "pattern", "url"]
        .iter()
        .find_map(|k| s(k))
        .map_or_else(|| input.to_string(), str::to_string);
    let key = format!("{name} {}", target.chars().take(160).collect::<String>());
    let command = s("command").or_else(|| s("cmd")).map(str::to_string).or_else(|| s("input").and_then(cmd_in_script));
    let path = ["file_path", "path", "filepath", "filename", "target_file", "file", "notebook_path"]
        .iter()
        .find_map(|k| s(k))
        .map(str::to_string)
        .or_else(|| s("input").and_then(patch_file));
    let words: Vec<String> = name.split(|c: char| !c.is_alphanumeric()).map(str::to_ascii_lowercase).collect();
    let has =
        |ws: &[&str]| words.iter().any(|w| ws.iter().any(|k| w == k || w.starts_with(k) && w.len() > k.len() + 1));
    let kind = if has(&["edit", "multiedit", "write", "create", "patch", "replace", "insert", "apply"]) {
        "write"
    } else if has(&["read", "view", "open", "cat", "recall"]) {
        "read"
    } else if has(&["grep", "glob", "search", "find", "list", "ls"]) {
        "search"
    } else if let Some(c) = &command {
        if printed_file(c).is_some() {
            "read"
        } else {
            "exec"
        }
    } else {
        "other"
    };
    let path = if kind == "read" && path.is_none() { command.as_deref().and_then(printed_file) } else { path };
    (key, kind, path)
}

/// The command a script's `exec_command({"cmd": …})` runs.
fn cmd_in_script(script: &str) -> Option<String> {
    let at = script.find("\"cmd\":\"").map(|i| i + 7).or_else(|| script.find("cmd:\"").map(|i| i + 5))?;
    let (mut out, mut esc) = (String::new(), false);
    for c in script[at..].chars() {
        match (esc, c) {
            (true, 'n') => out.push('\n'),
            (true, c) => out.push(c),
            (false, '\\') => {}
            (false, '"') => return Some(out),
            (false, c) => out.push(c),
        }
        esc = !esc && c == '\\';
    }
    None
}

/// The first file a patch (`*** Update File: x`) changes.
fn patch_file(patch: &str) -> Option<String> {
    patch.lines().find_map(|l| {
        ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
            .iter()
            .find_map(|h| l.strip_prefix(h))
            .map(str::to_string)
    })
}

/// The file a command prints, when every simple command of it prints a file as it is (`cat`, `sed -n`,
/// `head`, `grep` with a file) or only moves around (`cd`, `echo`): the last file named.
fn printed_file(command: &str) -> Option<String> {
    const PRINTERS: [&str; 12] =
        ["cat", "head", "tail", "sed", "nl", "tac", "less", "more", "grep", "egrep", "fgrep", "rg"];
    const QUIET: [&str; 4] = ["cd", "echo", "printf", "pwd"];
    let mut file = None;
    for piece in command.split([';', '|', '\n']).map(|p| p.trim_start_matches('&').trim()) {
        let words: Vec<&str> = piece.split_whitespace().collect();
        let Some(&first) = words.first() else { continue };
        if QUIET.contains(&first) {
            continue;
        }
        if !PRINTERS.contains(&first) || (first == "sed" && words.iter().any(|w| w.starts_with("-i"))) {
            return None;
        }
        let named = words[1..].iter().rev().find(|w| {
            !w.starts_with('-')
                && !w.starts_with('\'')
                && !w.starts_with('"')
                && w.contains(['/', '.'])
                && !w.starts_with('*')
        });
        let w = named?;
        file = Some(w.trim_matches(['\'', '"']).to_string());
    }
    file
}

/// Whether two paths name the same file: equal, or one is the other with more directories in front.
pub(super) fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim_start_matches("./"), b.trim_start_matches("./"));
    let under = |long: &str, short: &str| long.strip_suffix(short).is_some_and(|h| h.ends_with('/'));
    a == b || under(a, b) || under(b, a)
}

/// Whether the model saw result `mi` in `view`: its message whole, or its line in a text the view
/// brought back.
pub(super) fn shown(view: &Sending, cells: &[String], mi: usize, head: &str) -> bool {
    match cells.get(mi).map(String::as_str) {
        Some("whole") => true,
        Some(_) => head.len() >= 12 && has_line(view, head),
        None => true,
    }
}

impl Keys {
    /// The new messages of a request, from `base` in the history: calls made from `view` (the one `cells`
    /// describe), results after them.
    pub(super) fn see(&mut self, base: usize, new: &[Value], view: &Sending, cells: &[String]) {
        for (k, m) in new.iter().enumerate() {
            for b in blocks_of(m) {
                if b["type"] == "tool_use" {
                    self.calls += 1;
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    let (key, kind, path) = call_key(b["name"].as_str().unwrap_or(""), &b["input"]);
                    let (calls, cand) = (self.calls, &mut self.cand);
                    self.open.retain(|(trig, pk, pf, until)| {
                        if calls > *until {
                            return false;
                        }
                        let by_key = *pk == key;
                        let by_file =
                            kind == "read" && pf.as_deref().zip(path.as_deref()).is_some_and(|(a, b)| same_path(a, b));
                        if by_key || by_file {
                            let c = cand.entry(trig).or_default();
                            c[1] += i64::from(by_key);
                            c[2] += i64::from(!by_key);
                        }
                        !(by_key || by_file)
                    });
                    if let Some(r) = self.last.get(&key) {
                        self.repeats += 1;
                        let fresh = path
                            .as_deref()
                            .is_none_or(|p| !self.written.iter().any(|(f, c)| same_path(f, p) && *c > r.call));
                        let by = self.by_kind.entry(kind).or_default();
                        by[0] += 1;
                        if !shown(view, cells, r.mi, &r.head) {
                            self.out += 1;
                            self.out_err += usize::from(r.error);
                            by[1] += 1;
                            if fresh {
                                by[2] += 1;
                                by[3] += r.tokens;
                            }
                            if let Some((pk, perr, pkind, ppath)) = &self.prev {
                                if *perr {
                                    *self.signals.entry("error").or_default() += 1;
                                }
                                if *pkind == "write"
                                    && kind == "read"
                                    && ppath.as_deref().zip(path.as_deref()).is_some_and(|(a, b)| same_path(a, b))
                                {
                                    *self.signals.entry(if *perr { "write_fail" } else { "write_ok" }).or_default() +=
                                        1;
                                }
                                if self.next_of.get(pk) == Some(&key) {
                                    *self.signals.entry("bigram").or_default() += 1;
                                }
                            }
                        }
                    }
                    if let Some((pk, ..)) = &self.prev {
                        self.next_of.insert(pk.clone(), key.clone());
                    }
                    self.pending.insert(id, (key, kind, path, self.calls));
                } else if b["type"] == "tool_result" {
                    let id = b["tool_use_id"].as_str().unwrap_or("");
                    let Some((key, kind, path, call)) = self.pending.remove(id) else { continue };
                    let out = text_of(&json!({"content": [b.clone()]}));
                    let error = b["is_error"] == true || error_kind(&out, false).is_some();
                    let head: String =
                        out.lines().map(str::trim).find(|l| l.len() >= 12).unwrap_or("").chars().take(120).collect();
                    if let Some(p) = &path {
                        if kind == "write" && !error {
                            self.written.retain(|(f, _)| f != p);
                            self.written.push((p.clone(), call));
                        }
                        if kind == "read" {
                            self.read_of.retain(|(f, _)| f != p);
                            self.read_of.push((p.clone(), key.clone()));
                        }
                        if kind == "write" {
                            if let Some((_, rk)) = self.read_of.iter().rev().find(|(f, _)| same_path(f, p)) {
                                self.armed.push((if error { "write_fail" } else { "write_ok" }, rk.clone()));
                            }
                        }
                    }
                    if let Some(nk) = self.next_of.get(&key) {
                        self.armed.push(("bigram", nk.clone()));
                    }
                    if error {
                        self.armed.push(("error", key.clone()));
                    }
                    self.prev = Some((key.clone(), error, kind, path.clone()));
                    self.last
                        .insert(key, Res { mi: base + k, call, error, head, tokens: (out.len() as i64 + 3) / 4, path });
                }
            }
        }
    }

    /// The view built after the last request, with the state `cells` give each message: the triggers it
    /// armed fire when the result they point to is out of it (and, but for a write that worked, fresh).
    pub(super) fn after(&mut self, view: &Sending, cells: &[String]) {
        self.sent_tokens += view.total();
        for (trig, pk) in std::mem::take(&mut self.armed) {
            let Some(r) = self.last.get(&pk) else { continue };
            let fresh = trig == "write_ok"
                || r.path.as_deref().is_none_or(|p| !self.written.iter().any(|(f, c)| same_path(f, p) && *c > r.call));
            if !fresh || shown(view, cells, r.mi, &r.head) || self.open.iter().any(|(t, k, ..)| *t == trig && *k == pk)
            {
                continue;
            }
            let c = self.cand.entry(trig).or_default();
            c[0] += 1;
            c[3] += r.tokens;
            self.open.push((trig, pk, r.path.clone(), self.calls + 5));
        }
    }

    pub(super) fn json(&self) -> Value {
        json!({
            "calls": self.calls, "repeats": self.repeats, "out": self.out, "out_err": self.out_err,
            "by_kind": self.by_kind, "signals": self.signals, "cand": self.cand, "sent": self.sent_tokens,
        })
    }
}
