//! How a message enters the engine: shortened at birth, its edits tracked, its rules taken, the history synced.

use rustc_hash::FxHashMap;
use serde_json::{Map, Value};

use crate::hindsight::UKind;
use crate::json::{est, py_eq, strip_cc};
use crate::learn::MODEL;
use crate::mechanical::{squeeze, MARK};
use crate::msg::{blocks, get_str, key_of};
use crate::rules::{clip, rules_in};
use crate::textops::{self};
use crate::tools::{classify, edit_size, full_write, heredoc_writes, printed_files, prints_files, shell_writes, Kind};

use super::files::{basename, command_of, edited, file_key, forget, head_and_tail, same_file, Wrote, WROTE_TOKENS};
use super::notes::{note_text, NOTE_LINE};
use super::params::Params;
use super::Engine;

/// ``m`` as the model sees it from the moment it arrives. Tool output is shortened (`squeeze`) only when
/// it is the output of a process (`kind_of` its call: a command, an unknown tool): a log repeats itself
/// and its repeats say nothing. What a tool returns of a file or of a search is the material itself
/// (and so is what a command that only prints files returns: `cat`, `sed -n`, `head`, `grep`),
/// where a repeated line is content (an unrolled loop, a row of a table, a run of equal instructions),
/// and it stays as it came: in a measured run, folded repeats in a read of an assembly file cost the
/// answers that depended on them.
/// With `name` (the message's `m#####` number, when the host offers `recall`), a shortened output ends with
/// a line that says how long it was and how to have it back whole. The folds stay the lossless ones either
/// way: with `recall` offered, lossy folds at birth cost 26 % more forgotten words over 656 recorded sessions
/// (a reply copies what the fold dropped before the model thinks to ask for it), for 8 % of the cost.
fn at_birth(m: &Value, p: &Params, kind_of: &dyn Fn(&str) -> Kind, name: Option<usize>) -> Option<Value> {
    let back = |whole: &str, s: std::borrow::Cow<'_, str>| -> String {
        match name {
            Some(k) => format!(
                "{s}\n{MARK} this output is shortened above ({} lines in full); recall m{k:05} brings it back as it was, or lines from-to of it",
                whole.lines().count()
            ),
            None => s.into_owned(),
        }
    };
    if p.birth.is_empty() {
        return None;
    }
    let content = m.get("content")?;
    let with = |c: Value| {
        let mut out = m.clone();
        out["content"] = c;
        out
    };
    let Value::Array(list) = content else { return None };
    let sq = p.stage("squeeze");
    let process = |o: &Map<String, Value>| {
        matches!(kind_of(o.get("tool_use_id").and_then(Value::as_str).unwrap_or("")), Kind::Exec | Kind::Other)
    };
    let mut out = Vec::with_capacity(list.len());
    let mut changed = false;
    for b in list {
        let mut nb: Option<Value> = None;
        if let Value::Object(o) = b {
            let t = o.get("type").and_then(Value::as_str);
            if t == Some("tool_result") && sq && process(o) {
                match o.get("content") {
                    Some(Value::String(c)) => {
                        let s = squeeze(c, false);
                        if *s != **c {
                            let mut x = o.clone();
                            x.insert("content".into(), Value::String(back(c, s)));
                            nb = Some(Value::Object(x));
                        }
                    }
                    Some(Value::Array(parts)) => {
                        let mut any = false;
                        let new: Vec<Value> = parts
                            .iter()
                            .map(|x| {
                                if x.get("type").and_then(Value::as_str) == Some("text") {
                                    if let Some(tx) = x.get("text").and_then(Value::as_str) {
                                        let s = squeeze(tx, false);
                                        if *s != *tx {
                                            any = true;
                                            let mut y = x.clone();
                                            y["text"] = Value::String(back(tx, s));
                                            return y;
                                        }
                                    }
                                }
                                x.clone()
                            })
                            .collect();
                        if any {
                            let mut x = o.clone();
                            x.insert("content".into(), Value::Array(new));
                            nb = Some(Value::Object(x));
                        }
                    }
                    _ => {}
                }
            }
        }
        changed |= nb.is_some();
        out.push(nb.unwrap_or_else(|| b.clone()));
    }
    changed.then(|| with(Value::Array(out)))
}

impl Engine {
    fn add(&mut self, m: &Value) {
        let raw = strip_cc(m);
        // the kind of the call each result answers: the calls are in the message before it; a command
        // that only prints files returns the material, as a read does, and so does a tool given a URL (a page)
        let kinds: FxHashMap<String, Kind> = blocks(self.last_raw.get("content"))
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|b| {
                let (kind, _, command) = classify(get_str(b, "name"), b.get("input"));
                let prints = kind == Kind::Exec && command.as_deref().is_some_and(prints_files);
                let page = kind == Kind::Other && b.get("input").and_then(|x| x.get("url")).is_some();
                (get_str(b, "id").to_string(), if prints || page { Kind::Read } else { kind })
            })
            .collect();
        let name = self.recall_tool.then(|| self.tr.msgs.len() + 1);
        let born = at_birth(&raw, &self.p, &|id| kinds.get(id).copied().unwrap_or(Kind::Other), name);
        let it;
        let msg = match born {
            Some(b) => {
                it = est(&b);
                let saved = (est(&raw) - it).max(0);
                self.bump("birth_saved", saved);
                self.tag_event("birth", saved);
                self.own.born.insert(self.tr.msgs.len());
                b
            }
            None => {
                it = est(&raw);
                if !self.p.birth.is_empty() {
                    self.bump("birth_saved", 0);
                }
                raw.clone()
            }
        };
        if self.p.rules != 0 {
            self.take_rules(&raw);
        }
        if raw.get("role").and_then(Value::as_str) == Some("assistant") {
            let noted = blocks(raw.get("content")).iter().filter_map(note_text).any(|t| NOTE_LINE.is_match(&t));
            self.since_note = if noted { 0 } else { self.since_note + 1 };
            if noted {
                // the request for notes was answered: the result that carried it goes back to how it was sent
                for old in std::mem::take(&mut self.nudged) {
                    self.built.remove(&old);
                    self.unnudged = Some(self.unnudged.map_or(old, |o| o.min(old)));
                }
            }
            self.since_nudge = self.since_nudge.saturating_add(1);
        }
        self.last_raw = raw;
        self.tr.add(msg, &mut self.int);
        self.sizes.push(it);
        self.raw_tokens += it;
        let new = self.fz.extend(&self.tr, &mut self.int);
        self.track_edits(new.clone());
        self.asked_back(new.clone());
        if self.p.pin != 0 {
            for i in new.clone() {
                let u = &self.tr.units[i];
                if u.kind == UKind::User {
                    self.asked.extend(u.words.iter().copied());
                } else if u.kind == UKind::Result {
                    let id = key_of(self.tr.block(i).get("tool_use_id"));
                    let old = self.replaced.remove(&id).unwrap_or_default();
                    // a whole write that failed did not write the file: what it replaced is still there
                    let failed = self.fz.info.get(&i).is_some_and(|x| x.error && x.kind == Kind::Write);
                    if failed {
                        self.wrote.retain(|_, w| w.call != id);
                        for w in old {
                            self.wrote.insert(file_key(&w.path), w);
                        }
                        self.wrote.sort_by(|_, a, _, b| a.mi.cmp(&b.mi));
                    } else {
                        for w in &old {
                            forget(&mut self.pool, w);
                        }
                    }
                    // what the call changes counts once its result says it did
                    let held = self.pending_writes.remove(&id).unwrap_or_default();
                    if !self.fz.info.get(&i).is_some_and(|x| x.error) {
                        for (path, mi, size) in held {
                            match size {
                                Some(size) => edited(&mut self.wrote, &path, mi, size),
                                None => {
                                    self.written.insert(file_key(&path), mi);
                                }
                            }
                        }
                    }
                } else if u.kind == UKind::Call {
                    let info = self.fz.info.get(&i);
                    let kind = info.map_or(Kind::Other, |x| x.kind);
                    let mut whole: Vec<(String, String)> = Vec::new();
                    if kind == Kind::Write {
                        let b = self.tr.block(i);
                        let path = info.and_then(|x| x.path.clone()).unwrap_or_default();
                        if !basename(&path).is_empty() {
                            match full_write(get_str(b, "name"), b.get("input")) {
                                Some(text) => whole.push((path, text.to_string())),
                                None => {
                                    let size = edit_size(b.get("input"));
                                    let call = key_of(b.get("id"));
                                    self.pending_writes.entry(call).or_default().push((path, u.msg, Some(size)));
                                }
                            }
                        }
                    }
                    let written: Vec<String> = if kind == Kind::Write {
                        info.and_then(|x| x.path.clone()).filter(|p| !p.is_empty()).into_iter().collect()
                    } else if kind == Kind::Exec {
                        let b = self.tr.block(i);
                        shell_writes(
                            b.get("input").and_then(|x| x.get("command")).and_then(Value::as_str).unwrap_or(""),
                        )
                    } else {
                        Vec::new()
                    };
                    for w in written {
                        if kind == Kind::Exec {
                            edited(&mut self.wrote, &w, u.msg, 0);
                            self.written.insert(file_key(&w), u.msg);
                        } else {
                            let call = key_of(self.tr.block(i).get("id"));
                            self.pending_writes.entry(call).or_default().push((w, u.msg, None));
                        }
                    }
                    if kind == Kind::Exec {
                        // a file written whole by a here-document is as much a write as one by a write tool
                        let b = self.tr.block(i);
                        whole.extend(
                            heredoc_writes(&command_of(b)).into_iter().filter(|(p, _)| !basename(p).is_empty()),
                        );
                    }
                    let call = key_of(self.tr.block(i).get("id"));
                    for (path, text) in whole {
                        // the write replaces every earlier whole write of the file; they wait for its result
                        let gone: Vec<String> = self.wrote.keys().filter(|k| same_file(k, &path)).cloned().collect();
                        for k in gone {
                            if let Some(old) = self.wrote.shift_remove(&k) {
                                self.replaced.entry(call.clone()).or_default().push(old);
                            }
                        }
                        let text = head_and_tail(&text, WROTE_TOKENS);
                        let w = Wrote { mi: u.msg, call: call.clone(), path, text, edited: None };
                        self.wrote.insert(file_key(&w.path), w);
                    }
                }
            }
        }
        let k = self.tr.steps.len();
        while self.born.len() < new.end {
            self.born.push(k);
        }
        self.goal.update(&self.tr, &self.fz, MODEL.link.as_ref(), MODEL.link_share);
        self.vis = None;
        if self.p.dedup > 0 {
            self.find_dups(new);
        }
    }

    /// Keeps the edits and writes of files among the units that just arrived (`new`), and whether each
    /// came back without an error (`Params.stale`).
    fn track_edits(&mut self, new: std::ops::Range<usize>) {
        if self.p.stale == 0 {
            return;
        }
        for i in new {
            let u = &self.tr.units[i];
            let info = self.fz.info.get(&i);
            let b = self.tr.block(i);
            if u.kind == UKind::Call {
                let id = key_of(b.get("id"));
                let paths: Vec<String> = match info.map(|x| x.kind) {
                    Some(Kind::Write) => info.and_then(|x| x.path.clone()).into_iter().collect(),
                    Some(Kind::Exec) => {
                        let command = command_of(b);
                        let mut ps = shell_writes(&command);
                        for (p, _) in heredoc_writes(&command) {
                            if !ps.contains(&p) {
                                ps.push(p);
                            }
                        }
                        ps
                    }
                    _ => Vec::new(),
                };
                for p in paths.into_iter().filter(|p| !p.is_empty()) {
                    self.edits.push((p, u.msg, id.clone()));
                }
                if info.is_some_and(|x| x.kind == Kind::Exec) {
                    let read = printed_files(&command_of(b));
                    if !read.is_empty() {
                        self.shell_read.insert(id, read);
                    }
                }
            } else if u.kind == UKind::Result {
                let id = key_of(b.get("tool_use_id"));
                if self.edits.iter().any(|e| e.2 == id) {
                    self.edit_ok.insert(id.clone(), !info.is_some_and(|x| x.error));
                }
                if let Some(read) = self.shell_read.remove(&id) {
                    self.shell_reads.insert(i, read);
                }
            }
        }
    }

    /// The rules in message ``m`` as it arrives (before it is cut, and whatever topic it belongs to):
    /// the lines of the person's text that read as one, and the agent's lines beginning with `Rule:` (in its
    /// text or in a `note` call).
    fn take_rules(&mut self, m: &Value) {
        let mi = self.tr.msgs.len();
        let bs = blocks(m.get("content"));
        let texts: Vec<String> = bs.iter().filter_map(note_text).collect();
        let text = texts.join("\n");
        match m.get("role").and_then(Value::as_str) {
            Some("user") => {
                // a turn is a message the person wrote, not one that carries tool results back
                let results = bs.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
                if !results && !text.trim().is_empty() {
                    self.rules.turn += 1;
                }
                for r in rules_in(&text) {
                    self.rules.add(mi, r, false);
                }
            }
            Some("assistant") => {
                for c in NOTE_LINE.captures_all(&text) {
                    let span = |g: usize| c[g].map_or("", |(x, y)| &text[x..y]);
                    if span(1) == "Rule" {
                        self.rules.add(mi, clip(&textops::squash_ws(span(2))), true);
                    }
                }
            }
            _ => {}
        }
    }

    /// Takes in the conversation as the harness sends it: new messages are added; an edited history
    /// starts the engine over, with the rules of the session carried over.
    pub fn sync(&mut self, msgs: &[Value]) {
        let n = self.tr.msgs.len();
        let rewritten = n > 0 && (msgs.len() < n || !py_eq(&strip_cc(&msgs[n - 1]), &self.last_raw));
        if rewritten {
            let rules = std::mem::take(&mut self.rules).carried();
            // what the host set (window, tools, time) is not part of the history, and neither is what was
            // learned of the provider: it survives the restart
            let (budget, note_tool, recall_tool) = (self.budget, self.note_tool, self.recall_tool);
            let (now, life, read_seen) = (self.now, (self.life_lo, self.life_hi), self.read_seen);
            // the units of the history so far are complete: what they were used by is written out
            self.dump_uses();
            let dump = self.dump.take();
            *self = Engine::new(self.p.clone());
            self.dump = dump;
            self.rules = rules;
            self.budget = budget;
            self.note_tool = note_tool;
            self.recall_tool = recall_tool;
            (self.now, (self.life_lo, self.life_hi), self.read_seen) = (now, life, read_seen);
        }
        // a rewritten history is read anew: the rules found in it are the carried ones, where they were;
        // what the person wrote after the last reply comes with the rewrite and is new; with no reply at all
        // (a summary), only the newest message is, and a rewrite of one message is all summary
        let n = self.tr.msgs.len();
        let last_reply = msgs.iter().rposition(|m| m.get("role").and_then(Value::as_str) == Some("assistant"));
        let fresh_from = last_reply.map_or(msgs.len().saturating_sub(1).max(1), |r| r + 1);
        for (k, m) in msgs.iter().enumerate().skip(n) {
            self.rules.replay = rewritten && k < fresh_from;
            self.add(m);
        }
        self.rules.replay = false;
    }
}
