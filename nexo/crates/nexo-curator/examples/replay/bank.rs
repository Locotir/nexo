use super::*;

/// `--bank`: one session of the corpus (`examples/corpus.rs`) sent three ways, as one JSON line: the whole
/// history each time (no curator), the requests as the agent sent them, with its own compaction (when the
/// corpus wrote them, in `sent/` next to the file), and the curator's view. For each way: the input cost
/// with a prefix cache, the largest request, the requests that rewrote the past, and the distinctive words
/// the replies took from the history that the way had not sent. Memory and time grow with the session,
/// not with its square: each request is applied to what the one before left, as the file gives it.
/// With `oracle`, a fourth way: the offline floor of `belady`, at the tokens the curator's view spent.
pub(super) fn bank(path: &str, spec: &str, recall: bool, oracle: bool, dump: Option<&str>) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file = std::path::Path::new(path);
    let sent_text = file
        .parent()
        .zip(file.file_name())
        .and_then(|(dir, name)| std::fs::read_to_string(dir.join("sent").join(name)).ok());
    let mut sent_lines = sent_text.as_deref().map(|t| t.lines().filter(|l| !l.trim().is_empty()));
    let mut cur = Curator::new(spec).map_err(|e| format!("bad spec: {e}"))?;
    // `--dump DIR`: the features of what the view holds at 40 requests of the session, and what each unit
    // was used by, for a fit of the need heads outside the replay
    if let Some(dir) = dump {
        let n = text.lines().filter(|l| !l.trim().is_empty()).count();
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("session");
        cur.dump_to(&format!("{dir}/{name}.tsv"), n.div_ceil(40)).map_err(|e| format!("dump: {e}"))?;
    }
    // `--recall`: the host offers the `recall` tool, as nexo does by default
    let offer = |mut t: Vec<String>| {
        if recall && !t.iter().any(|n| n == "recall") {
            t.push("recall".into());
        }
        t
    };
    cur.tools(&offer(vec!["note".into()]));
    let (mut whole, mut sent, mut view) = (Sending::default(), Sending::default(), Sending::default());
    let (mut plain, mut agent, mut curated) = (Way::default(), Way::default(), Way::default());
    let (mut system, mut system_tokens) = (HashSet::<String>::new(), 0i64);
    let (mut requests, mut replies, mut copied) = (0usize, 0usize, 0usize);
    let mut owned: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    // the same, by the rule and the kind of the message (user, assistant, result) that held the word last
    let mut owned_by: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let parse = |line: &str| -> Result<(usize, Vec<Value>, Value), String> {
        let mut rec: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let base = rec["req"]["base"].as_u64().unwrap_or(0) as usize;
        let new = match rec["req"]["new"].take() {
            Value::Array(a) => a,
            _ => Vec::new(),
        };
        Ok((base, new, rec))
    };
    // G12: result texts (200 bytes or more) the history held before, to size the re-reads that follow a compaction
    let (mut seen, mut reread_after, mut reread_before, mut new_after) = (HashSet::<String>::new(), 0i64, 0i64, 0i64);
    let mut errs = Errs::default();
    let (mut keys, mut cells, mut steps) =
        (Keys::default(), Vec::<String>::new(), std::collections::BTreeMap::<String, i64>::new());
    // for the oracle: each request as given, the words the reply to each copied, and the view's tokens
    let (mut records, mut said_at, mut budgets) =
        (Vec::<(usize, Vec<Value>)>::new(), Vec::<Option<Vec<String>>>::new(), Vec::<i64>::new());
    let started = std::time::Instant::now();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (base, new, rec) = parse(line)?;
        if oracle {
            records.push((base, new.clone()));
        }
        keys.see(base, &new, &view, &cells);
        errs.see(&new, &view, sent_text.is_some().then_some(&sent));
        let req = &rec["req"];
        if requests == 0 {
            system_tokens = req["system_tokens"].as_i64().unwrap_or(0);
            system.extend(distinctive_words(req["system"].as_str().unwrap_or("")));
            if let Some(t) = names(&req["tools"]) {
                cur.tools(&offer(t));
            }
        }
        // the reply to the request before, when this one goes on from it
        if requests > 0 && base == whole.msgs.len() && new.first().is_some_and(|m| m["role"] == "assistant") {
            let said: Vec<String> = said_words(&new[0]).into_iter().filter(|w| whole.bag.has(w)).collect();
            replies += 1;
            copied += said.len();
            for (way, sending) in [(&mut agent, &sent), (&mut curated, &view)] {
                let lost = said.iter().filter(|w| !sending.bag.has(w) && !system.contains(*w)).count();
                way.lost += lost;
                if std::ptr::eq(sending, &view) {
                    for w in said.iter().filter(|w| !sending.bag.has(w) && !system.contains(*w)) {
                        let mut best: Option<((u64, &str), usize)> = None;
                        for (mi, ws) in whole.words.iter().enumerate() {
                            if ws.iter().any(|x| x == w) {
                                if let Some(f) = cur.fate(mi, w) {
                                    best = Some(best.map_or((f, mi), |b| b.max((f, mi))));
                                }
                            }
                        }
                        let rule = best.map_or("unowned", |b| b.0 .1);
                        *owned.entry(rule.to_string()).or_default() += 1;
                        let kind = best.map_or("none", |(_, mi)| match &whole.msgs[mi] {
                            m if has_result(m) => "result",
                            m if m["role"] == "user" => "user",
                            _ => "assistant",
                        });
                        *owned_by.entry(format!("{rule}/{kind}")).or_default() += 1;
                    }
                }
                way.lost_replies += usize::from(lost > 0);
            }
            if let Some(last) = said_at.last_mut() {
                *last = Some(said);
            }
        }
        let head = if requests > 0 { system_tokens } else { 0 };
        let compacted = agent.rewrites > 0;
        for m in &new {
            for b in blocks_of(m).iter().filter(|b| b["type"] == "tool_result") {
                let t = text_of(&json!({"content": [b.clone()]}));
                if t.len() < 200 {
                    continue;
                }
                let tok = (t.len() as i64 + 3) / 4;
                let dup = !seen.insert(t);
                if compacted {
                    new_after += tok;
                    reread_after += if dup { tok } else { 0 };
                } else if dup {
                    reread_before += tok;
                }
            }
        }
        let hit = whole.extend(base, new);
        plain.count(whole.total() + system_tokens, hit + head);
        if let Some(line) = sent_lines.as_mut().and_then(Iterator::next) {
            let (base, new, _) = parse(line)?;
            agent.rewrites += usize::from(base < sent.msgs.len());
            let hit = sent.extend(base, new);
            agent.count(sent.total() + system_tokens, hit + head);
        }
        let now = rec["now"].as_f64().unwrap_or(0.0);
        let reply = cur.view(system_tokens as u64, &whole.msgs, now).map_err(|e| format!("view: {e}"))?;
        cells = reply.memory.cells.iter().map(|c| c.state.as_str().to_string()).collect();
        steps = reply.stats.iter().filter(|(k, _)| k.ends_with("_steps")).map(|(k, v)| (k.to_string(), *v)).collect();
        let (hit, rewrote) = view.send(reply.view);
        curated.rewrites += usize::from(rewrote);
        keys.after(&view, &cells);
        curated.count(view.total() + system_tokens, hit + head);
        said_at.push(None);
        budgets.push(view.total());
        requests += 1;
    }
    cur.dump_end();
    let floor = oracle.then(|| belady(&records, &said_at, &budgets, &system, system_tokens));
    Ok(json!({
        "file": file.file_name().and_then(|n| n.to_str()).unwrap_or(path),
        "requests": requests, "messages": whole.msgs.len(), "replies": replies, "copied": copied,
        "reread": {"before": reread_before, "after": reread_after, "new_after": new_after},
        "secs": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "plain": plain.json(),
        "sent": if sent_text.is_some() { agent.json() } else { Value::Null },
        "view": curated.json(),
        "oracle": floor.map_or(Value::Null, |w| w.json()),
        "errs": errs.json(),
        "keys": keys.json(), "steps": steps,
        "own": {"lost": owned, "lost_by": owned_by, "saved": cur.savings().into_iter().collect::<std::collections::BTreeMap<_, _>>()},
    }))
}
