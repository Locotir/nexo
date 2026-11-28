use super::*;

/// A result the agent got: the call key it answers, the file a read holds, its segments and its tokens.
struct Got {
    key: String,
    kind: &'static str,
    path: Option<String>,
    segs: Vec<String>,
    tokens: i64,
    /// the request at which a view first left it out before the agent acted on it (the `blind` rule)
    left_unused: Option<usize>,
    /// asked for again later (the same key, or a read of the same file, nothing written to it since)
    again: bool,
    /// the call's id, tool name and whole input, the result's text, the request it came back at and
    /// the number of writes of any file seen before it
    id: String,
    name: String,
    input: String,
    text: String,
    req: usize,
    writes_before: usize,
}

/// One repeat in detail (`REPLAY_REREAD=2`): the tool, whether the call's input was the same byte for byte,
/// whether its output came back the same, the requests between the two, whether any file was written in
/// between, and how the earlier result stood in the view the repeat was made from.
pub(super) struct Repeat {
    pub(super) tool: String,
    pub(super) same_input: bool,
    pub(super) identical: Option<bool>,
    pub(super) apart: usize,
    pub(super) wrote_between: bool,
    pub(super) saw: &'static str,
    pub(super) line: usize,
}

/// Re-reads: the calls that ask again for what an earlier call already returned (the same tool with the same
/// input byte for byte, or a read of the same file, with no write of that file in between), and
/// whether the earlier result was in the view the call was made from. Next to it, the results a view left
/// out before the agent acted on them (`blind`): how many of those the agent asked for again later, against
/// the rate for every other result. A replay cannot show what the model would have done with another view,
/// but a repeat made with the earlier result in view was not caused by the curator, and a result left out
/// unused that is never asked for again cost the agent nothing.
#[derive(Default)]
pub(super) struct Rereads {
    got: Vec<Got>,
    /// call id -> (key, kind, file)
    pending: HashMap<String, (String, &'static str, Option<String>)>,
    /// (call id, tool_result position) -> index in `got`
    by_id: HashMap<String, usize>,
    /// file -> how many writes of it were seen (a read after a write is not a repeat of the read before it)
    writes: HashMap<String, usize>,
    /// index in `got` -> the write count of its file when it was read
    stamp: HashMap<usize, usize>,
    seen: usize,
    pub(super) repeats: usize,
    pub(super) out_of_view: usize,
    pub(super) reads: usize,
    pub(super) reads_out: usize,
    pub(super) lines: Vec<String>,
    pub(super) detail: Vec<Repeat>,
    /// call id of a repeat -> (index in `detail`, index in `got` of the result it repeats)
    open: HashMap<String, (usize, usize)>,
    all_writes: usize,
    /// call id -> (tool name, whole input)
    calls: HashMap<String, (String, String)>,
}

impl Rereads {
    /// The messages new since the last request (`history[self.seen..]`), with `view` the view the calls among
    /// them were made from (the previous request's), at request `req` (1-based).
    pub(super) fn see(&mut self, history: &[Value], view: &[Value], req: usize) {
        let shown: HashSet<String> = view.iter().flat_map(segments).collect();
        let from = self.seen.min(history.len());
        for m in &history[from..] {
            for b in blocks_of(m) {
                if b["type"] == "tool_use" {
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    let (key, kind, path) = call_key(b["name"].as_str().unwrap_or(""), &b["input"]);
                    let input = b["input"].to_string();
                    if let Some(gi) = self.earlier(&key, &input, kind, path.as_deref()) {
                        self.repeats += 1;
                        let all_writes = self.all_writes;
                        let g = &mut self.got[gi];
                        self.open.insert(id.clone(), (self.detail.len(), gi));
                        self.detail.push(Repeat {
                            tool: g.name.clone(),
                            same_input: g.input == input,
                            identical: None,
                            apart: req.saturating_sub(g.req),
                            wrote_between: all_writes > g.writes_before,
                            saw: saw(view, &g.id, &g.text),
                            line: self.lines.len(),
                        });
                        g.again = true;
                        let out = 2 * g.segs.iter().filter(|s| shown.contains(*s)).count() < g.segs.len();
                        self.out_of_view += usize::from(out);
                        if kind == "read" {
                            self.reads += 1;
                            self.reads_out += usize::from(out);
                        }
                        self.lines.push(format!(
                            "r{req} asks again {key:.100} ({} tok, {} of the view{})",
                            g.tokens,
                            if out { "out" } else { "in" },
                            g.left_unused.map_or(String::new(), |r| format!(", left out unused at r{r}"))
                        ));
                    }
                    if kind == "write" {
                        self.all_writes += 1;
                        if let Some(p) = &path {
                            *self.writes.entry(file_name(p)).or_default() += 1;
                        }
                    }
                    self.calls
                        .insert(id.clone(), (b["name"].as_str().unwrap_or("").to_string(), b["input"].to_string()));
                    self.pending.insert(id, (key, kind, path));
                } else if b["type"] == "tool_result" {
                    let id = b["tool_use_id"].as_str().unwrap_or("").to_string();
                    let Some((key, kind, path)) = self.pending.remove(&id) else { continue };
                    let one = json!({"role": "user", "content": [b]});
                    let text = result_text(&b);
                    if let Some((di, gi)) = self.open.remove(&id) {
                        self.detail[di].identical = Some(self.got[gi].text == text);
                    }
                    let gi = self.got.len();
                    if let Some(p) = &path {
                        self.stamp.insert(gi, self.writes.get(&file_name(p)).copied().unwrap_or(0));
                    }
                    self.got.push(Got {
                        key,
                        kind,
                        path,
                        segs: segments(&one),
                        tokens: tokens_of(&one),
                        left_unused: None,
                        again: false,
                        id: id.clone(),
                        name: self.calls.get(&id).map(|(n, _)| n.clone()).unwrap_or_default(),
                        input: self.calls.get(&id).map(|(_, i)| i.clone()).unwrap_or_default(),
                        text,
                        req,
                        writes_before: self.all_writes,
                    });
                    self.by_id.insert(id, gi);
                }
            }
        }
        self.seen = history.len();
    }

    /// The latest earlier result a call (`key`, its whole `input`) repeats, if nothing wrote its file since:
    /// the same tool with the same input byte for byte (a grep of another pattern in the same folder, or a
    /// script that shares only its first 160 characters, shares the key but asks something else), or a read
    /// of `path`.
    fn earlier(&self, key: &str, input: &str, kind: &str, path: Option<&str>) -> Option<usize> {
        let gi = self.got.iter().rposition(|g| {
            (g.key == key && g.input == input)
                || (kind == "read"
                    && g.kind == "read"
                    && g.path.as_deref().zip(path).is_some_and(|(a, b)| same_path(a, b)))
        })?;
        let g = &self.got[gi];
        let changed = g.path.as_deref().is_some_and(|p| {
            self.writes.get(&file_name(p)).copied().unwrap_or(0) > self.stamp.get(&gi).copied().unwrap_or(0)
        });
        (!changed && g.kind != "write").then_some(gi)
    }

    /// The results the view built at request `req` leaves out before the agent acted on them (`blind_parts`).
    pub(super) fn left_out(&mut self, history: &[Value], view: &[Value], req: usize) {
        for (mi, bi, tokens, missing) in blind_parts(history, view) {
            if 2 * missing < tokens {
                continue;
            }
            let Some(b) = history.get(mi).map(blocks_of).and_then(|b| b.get(bi).cloned()) else { continue };
            let id = b["tool_use_id"].as_str().unwrap_or("");
            if let Some(&gi) = self.by_id.get(id) {
                self.got[gi].left_unused.get_or_insert(req);
            }
        }
    }

    /// (results left out unused, of them asked for again, other results, of them asked for again), results
    /// under 100 tokens aside as `blind` leaves them.
    pub(super) fn rates(&self) -> (usize, usize, usize, usize) {
        let big = self.got.iter().filter(|g| g.tokens >= 100);
        big.fold((0, 0, 0, 0), |(u, ua, o, oa), g| match g.left_unused {
            Some(_) => (u + 1, ua + usize::from(g.again), o, oa),
            None => (u, ua, o + 1, oa + usize::from(g.again)),
        })
    }
}

/// The text of a tool result block.
fn result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(inner) => inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// How the result of call `id` (whose text was `text`) stood in `view`: `whole` (the same text), `curator`
/// (rebuilt with a `[curator` line), `cut` (shorter, no such line), `other` (changed otherwise), `gone` (no
/// result of that id in the view). `+note` when the view says anywhere that something is stale or should be
/// read again.
fn saw(view: &[Value], id: &str, text: &str) -> &'static str {
    let found = view.iter().flat_map(blocks_of).find(|b| b["type"] == "tool_result" && b["tool_use_id"] == id);
    let note = view.iter().any(|m| {
        let t = m.to_string();
        t.contains("re-read before trusting") || t.contains("stale") || t.contains("changed since")
    });
    let how = match found {
        None => "gone",
        Some(b) => {
            let t = result_text(&b);
            if t == text {
                "whole"
            } else if t.contains("[curator") {
                "curator"
            } else if t.len() < text.len() {
                "cut"
            } else {
                "other"
            }
        }
    };
    match (how, note) {
        ("whole", true) => "whole+note",
        ("curator", true) => "curator+note",
        ("cut", true) => "cut+note",
        ("gone", true) => "gone+note",
        ("other", true) => "other+note",
        _ => how,
    }
}

/// A file's last two path parts, so `./src/a.rs`, `/tmp/x/src/a.rs` and `src/a.rs` count as one file.
fn file_name(p: &str) -> String {
    let parts: Vec<&str> = p.trim_start_matches("./").rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, name: &str, input: Value) -> Value {
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]})
    }

    fn result(id: &str, text: &str) -> Value {
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": text}]})
    }

    #[test]
    fn a_read_of_an_unchanged_file_is_a_repeat_and_one_after_a_write_is_not() {
        let body = (0..60).map(|i| format!("fn store_item_{i}() {{}}")).collect::<Vec<_>>().join("\n");
        let mut h = vec![json!({"role": "user", "content": "fix the store"})];
        h.push(call("a", "read", json!({"path": "src/store.rs"})));
        h.push(result("a", &body));
        let mut rr = Rereads::default();
        rr.see(&h, &[], 1);
        h.push(call("b", "read", json!({"path": "./src/store.rs"})));
        h.push(result("b", &body));
        let view = h[..3].to_vec();
        rr.see(&h, &view, 2);
        assert_eq!((rr.repeats, rr.reads, rr.reads_out), (1, 1, 0), "{:?}", rr.lines);
        h.push(call("c", "edit", json!({"path": "src/store.rs", "old_string": "a", "new_string": "b"})));
        h.push(result("c", "ok"));
        h.push(call("d", "read", json!({"path": "src/store.rs"})));
        h.push(result("d", &body));
        rr.see(&h, &h[..1], 3);
        assert_eq!(rr.repeats, 1, "a read after an edit of the file is new: {:?}", rr.lines);
    }

    #[test]
    fn a_search_for_another_pattern_in_the_same_folder_is_not_a_repeat_and_its_output_is_compared() {
        let mut h = vec![json!({"role": "user", "content": "find the handlers"})];
        h.push(call("a", "grep", json!({"pattern": "alder_handler", "path": "src"})));
        h.push(result("a", "src/alder.rs:3: fn alder_handler()"));
        let mut rr = Rereads::default();
        rr.see(&h, &[], 1);
        h.push(call("b", "grep", json!({"pattern": "birch_handler", "path": "src"})));
        h.push(result("b", "src/birch.rs:9: fn birch_handler()"));
        let view = h.clone();
        rr.see(&h, &view, 2);
        assert_eq!(rr.repeats, 0, "same folder, another pattern: {:?}", rr.lines);
        h.push(call("c", "grep", json!({"pattern": "alder_handler", "path": "src"})));
        h.push(result("c", "src/alder.rs:3: fn alder_handler()"));
        let view = h[..h.len() - 2].to_vec();
        rr.see(&h, &view, 3);
        assert_eq!(rr.repeats, 1, "{:?}", rr.lines);
        let d = &rr.detail[0];
        assert_eq!(
            (d.tool.as_str(), d.same_input, d.identical, d.apart, d.saw),
            ("grep", true, Some(true), 2, "whole")
        );
    }

    #[test]
    fn a_result_left_out_unused_and_asked_again_is_counted() {
        let body = (0..60).map(|i| format!("row ledger_entry_{i} value {}", i * 7)).collect::<Vec<_>>().join("\n");
        let mut h = vec![json!({"role": "user", "content": "check the ledger"})];
        h.push(call("a", "bash", json!({"command": "cat ledger.txt"})));
        h.push(result("a", &body));
        let mut rr = Rereads::default();
        rr.see(&h, &[], 1);
        let view = vec![h[0].clone(), h[1].clone(), result("a", "[out]")];
        rr.left_out(&h, &view, 1);
        h.push(call("b", "bash", json!({"command": "cat ledger.txt"})));
        h.push(result("b", &body));
        rr.see(&h, &view, 2);
        assert_eq!((rr.repeats, rr.out_of_view), (1, 1));
        assert_eq!(rr.rates(), (1, 1, 1, 0));
    }
}
