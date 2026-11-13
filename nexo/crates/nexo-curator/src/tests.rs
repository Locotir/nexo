use super::*;

#[test]
fn a_forgotten_word_is_a_number_an_identifier_or_other() {
    for n in ["4373.25", "1372", "10.0", "-8500.0", "1,372"] {
        assert_eq!(word_kind(n), WordKind::Number, "{n}");
    }
    for id in ["tools/audit.py", "SPEC_EXAMPLES", "sulko_dolde1", "40114b", "deadbeef", "1.2.3a", "c1-200", "a.out"] {
        assert_eq!(word_kind(id), WordKind::Identifier, "{id}");
    }
    for o in ["Ledgerpour", "Cargo", "fixture"] {
        assert_eq!(word_kind(o), WordKind::Other, "{o}");
    }
}

#[test]
fn the_tally_splits_by_kind_and_shows_how_much_one_reply_holds() {
    let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut t = LostTally::default();
    t.add(1, &w(&["a/b.rs", "Zorbal"]));
    t.add(2, &[]);
    let table: Vec<String> = (0..8).map(|i| format!("{i}00.5")).chain(["a/b.rs".to_string()]).collect();
    t.add(3, &table);
    assert_eq!((t.words, t.replies), (11, 2));
    assert_eq!((t.numbers, t.identifiers, t.other), (8, 2, 1));
    assert_eq!(t.words, t.numbers + t.identifiers + t.other);
    assert_eq!(t.distinct(), 10, "a/b.rs forgotten in two replies counts once");
    assert_eq!(t.largest, Some((3, 9)));
    assert_eq!(t.without_largest(), 2);
    assert!((t.share_of_largest() - 100.0 * 9.0 / 11.0).abs() < 1e-9);
    assert_eq!(LostTally::default().share_of_largest(), 0.0);
}

/// The map and the counts of a reply come from the map of the reply before and what changed since. Over
/// recorded sessions that leave messages out, cut them, bring them back, leave whole topics out, rewrite
/// the history and start over, they are the ones a map made from nothing gives, and a reply the host kept
/// is not written over.
#[test]
fn the_map_kept_between_requests_is_the_map_made_from_nothing() {
    let (mut back, mut removed, mut cut, mut rewritten, mut apart) = (0, 0, 0, 0, 0);
    let names = ["standing_rc", "topics", "synthetic-g2-t40-s3", "synthetic-g3-t8-s1", "bench-marathon-D"];
    for (spec, name) in ["", "epoch=0", "apart=0;epoch=0"].iter().flat_map(|s| names.iter().map(move |n| (*s, *n))) {
        let path = format!("{}/tests/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR"));
        let mut cur = Curator::new(spec).unwrap();
        let mut msgs: Vec<Value> = Vec::new();
        let mut kept: Option<(Memory, String)> = None;
        for (n, line) in std::fs::read_to_string(path).unwrap().lines().enumerate() {
            let rec: Value = serde_json::from_str(line).unwrap();
            let (req, now) = (&rec["req"], rec["now"].as_f64().unwrap_or(0.0));
            let base = req["base"].as_u64().unwrap() as usize;
            rewritten += usize::from(base < msgs.len());
            msgs.truncate(base);
            msgs.extend(req["new"].as_array().unwrap().iter().cloned());
            if n == 30 {
                cur.reset();
            }
            let was = cur.last.clone();
            let r = cur.view(req["system_tokens"].as_u64().unwrap_or(0), &msgs, now).unwrap();
            assert_eq!(r.memory, cur.eng.memory_whole(), "{spec} {name} line {n}");
            assert_eq!(r.curated, cur.eng.added_whole(), "{spec} {name} line {n}: what `added` kept");
            let walked: Vec<usize> = cur.eng.counted().into_iter().flat_map(|(a, b)| a..b).collect();
            assert_eq!(walked, cur.eng.counted_whole(), "{spec} {name} line {n}: the messages `added` weighs");
            // the counts, by the rule read over every cell
            let cells = r.memory.cells.len();
            let state = |i: usize| r.memory.cells[i].state;
            let old = |i: usize| was.get(i).copied().unwrap_or_default();
            let came = |i: &usize| state(*i) != State::Out && old(*i) == State::Out;
            let left = |i: &usize| {
                state(*i) == State::Out && old(*i) != State::Out || state(*i) == State::Cut && old(*i) == State::Whole
            };
            let counts = ((0..cells).filter(came).count(), (0..was.len().min(cells)).filter(came).count());
            assert_eq!(
                (r.added, r.back, r.removed),
                (counts.0, counts.1, (0..cells).filter(left).count()),
                "{name} {n}"
            );
            assert!(cur.last.iter().copied().eq((0..cells).map(state)), "{spec} {name} line {n}");
            if let Some((map, text)) = &kept {
                assert_eq!(&map.to_string(), text, "{spec} {name} line {n}: the reply before was written over");
            }
            back += r.back;
            removed += r.removed;
            cut += (0..cells).filter(|&i| state(i) == State::Cut).count();
            apart += usize::from(r.memory.zones.iter().any(|z| z.card));
            kept = Some((r.memory.clone(), r.memory.to_string()));
        }
    }
    assert!(
        back > 0 && removed > 0 && cut > 0 && rewritten > 0 && apart > 0,
        "{back} {removed} {cut} {rewritten} {apart}"
    );
}

#[test]
fn a_cache_cheaper_than_the_specs_ratio_is_weighed_at_its_price() {
    let mut c = Curator::new("").unwrap();
    c.prices(0.28, 0.028);
    assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
    c.prices(0.3, 0.006);
    assert!((c.eng.ratio() - 0.02).abs() < 1e-9);
    c.reset();
    assert!((c.eng.ratio() - 0.02).abs() < 1e-9, "a reset keeps the prices");
    // a dearer cache, or none, after a switch of model: back to the spec's ratio
    c.prices(0.2, 0.05);
    assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
    c.prices(0.3, 0.006);
    c.prices(1.0, 0.0);
    assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
    // a local model or unknown prices: the spec's ratio
    let mut c = Curator::new("").unwrap();
    c.prices(0.0, 0.0);
    assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
    let mut c = Curator::new("ratio=0.3").unwrap();
    c.prices(0.3, 0.006);
    assert!((c.eng.ratio() - 0.3).abs() < 1e-9);
}

fn refused(spec: &str) -> String {
    params(spec).err().unwrap_or_else(|| panic!("{spec:?} ran"))
}

#[test]
fn every_value_has_its_default_in_d_spec_and_the_docs_show_it() {
    assert!(params("").is_ok());
    let d = parse_params(D_SPEC).unwrap();
    let only: Vec<&str> = ONLY.iter().map(|(k, _)| *k).collect();
    let unsupported = ["outcome", "gwin", "lerr", "jit"];
    for k in INTS.iter().chain(FLOATS).chain(STRS) {
        assert!(d.contains_key(*k) || only.contains(k) || unsupported.contains(k), "{k} has no default in D_SPEC");
    }
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../docs/how-it-works.md")).unwrap();
    assert!(doc.contains(D_SPEC), "docs/how-it-works.md shows another default spec");
}

#[test]
fn bad_values_are_refused() {
    assert_eq!(refused("eoq=-1"), "eoq takes a whole number of 0 or more, not \"-1\"");
    assert_eq!(refused("protect=lots"), "protect takes a whole number of 0 or more, not \"lots\"");
    for v in ["nan", "inf", "-0.5", "1e999", ""] {
        assert!(refused(&format!("margin={v}")).starts_with("margin takes a number of 0 or more"), "{v}");
    }
    assert_eq!(refused("eoq"), "\"eoq\" is not key=value");
    // settings no engine reads any more are unknown, not taken and ignored
    for k in ["budget", "horizon", "shortlist", "miss", "terms", "rank", "cold"] {
        assert_eq!(refused(&format!("{k}=1")), format!("unknown setting {k:?}"));
    }
    assert_eq!(refused("rule=lru"), "rule is not supported by the built-in curator");
    assert_eq!(refused("target=9000;eoq=0;floor=0"), "target needs a floor above 0");
    assert_eq!(refused("target=9000;cap=5000"), "target is above cap");
    assert_eq!(refused("cap=90000;hard=80000"), "cap is above hard");
    assert_eq!(refused("birth=squeez"), "birth has no stage \"squeez\"");
    assert_eq!(refused("birth=squeeze+telegraf"), "birth has no stage \"telegraf\"");
    assert!(params("birth=").is_ok_and(|p| p.birth.is_empty()), "an empty birth runs no stage");
    // the values these settings take still run, written as they were
    assert!(params(" rule=rent ; recall=0;trust=1.0;margin=1_000;; ").is_ok());
    assert!(params("target=0;cap=0;floor=0;eoq=0").is_ok(), "no target needs no floor");
}

/// Replays every fixture: a view that sends a message of the previous view otherwise (or no longer)
/// counts the tokens from that message on under `miss`, the latest message sent too: an output sent
/// whole as the latest one goes as a pointer from the next request on (`miss_fresh`).
#[test]
fn every_rewrite_of_a_sent_message_counts_its_miss() {
    // the tail (standing facts, lines the request names) is not history: it is built again on each call, goes
    // after the history that is cached and costs only itself
    let without_tail = |m: &Value| -> Value {
        let mut m = m.clone();
        if let Some(Value::Array(bs)) = m.get_mut("content") {
            bs.retain(|b| {
                let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                !(t.starts_with("Background that nexo brought back") || t.starts_with("Recall (lines of earlier"))
            });
            // a text that was a plain string became a list of blocks to carry the tail: the same text again
            if let [b] = bs.as_slice() {
                if let Some(t) = b.get("text").filter(|_| b.as_object().is_some_and(|o| o.len() == 2)) {
                    m["content"] = t.clone();
                }
            }
        }
        m
    };
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let mut names: Vec<_> =
        std::fs::read_dir(dir).unwrap().filter_map(|e| Some(e.ok()?.path())).filter(|p| p.is_file()).collect();
    names.sort();
    let mut bad = Vec::new();
    for path in names {
        let text = std::fs::read_to_string(&path).unwrap();
        let mut cur = Curator::new("").unwrap();
        let (mut msgs, mut prev, mut missed): (Vec<Value>, Vec<Value>, i64) = (Vec::new(), Vec::new(), 0);
        for (n, line) in text.lines().enumerate() {
            let rec: Value = serde_json::from_str(line).unwrap();
            let req = &rec["req"];
            let Some(new) = req.get("new") else {
                cur.answer(req, 0.0);
                (msgs, prev, missed) = (Vec::new(), Vec::new(), 0);
                continue;
            };
            let base = req["base"].as_u64().unwrap() as usize;
            // an edited history is a change of the host's, not of the view
            let edited = base < msgs.len();
            msgs.truncate(base);
            msgs.extend(new.as_array().unwrap().iter().cloned());
            let send = json!({"op": "view", "system_tokens": req.get("system_tokens").cloned().unwrap_or(Value::Null), "messages": msgs});
            let got = cur.answer(&send, 0.0);
            let Some(Value::Array(view)) = got.get("view") else { continue };
            let view: Vec<Value> = view.iter().map(&without_tail).collect();
            let now = got["stats"].get("miss").and_then(Value::as_i64).unwrap_or(0);
            if !edited {
                if let Some(p) = (0..prev.len()).find(|&p| view.get(p) != Some(&prev[p])) {
                    // (a message that joined the one before it in the view changes from its first block that differs,
                    // or from the first it gained: a request typed right after an output joins that output)
                    let behind: i64 = match view.get(p) {
                        Some(v) => {
                            let (new, old) = (msg::blocks(v.get("content")), msg::blocks(prev[p].get("content")));
                            let same = new.len().min(old.len());
                            let b = (0..same).find(|&b| new[b] != old[b]).unwrap_or(same);
                            new[b..].iter().map(json::est).sum::<i64>()
                                + view[p + 1..].iter().map(json::est).sum::<i64>()
                        }
                        None => 0,
                    };
                    if ((now - missed) as f64) < 0.9 * behind as f64 - 50.0 {
                        bad.push(format!(
                            "{} line {n}: changed at {p}, {} missed of {behind}",
                            path.display(),
                            now - missed
                        ));
                    }
                }
            }
            (prev, missed) = (view.clone(), now);
        }
    }
    assert!(bad.is_empty(), "{} uncounted:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn a_text_paged_again_keeps_its_cut_while_the_overflow_grows_a_little() {
    let long: String = (0..4000).map(|k| format!("line {k} of the pasted log\n")).collect();
    let paged = |over: i64| {
        let mut view = vec![json!({"role": "user", "content": long.clone()})];
        assert!(!page(&mut view, over).is_empty());
        view
    };
    // the next request adds a few hundred tokens past the window: the paged text is sent as before
    assert_eq!(paged(10_000), paged(10_300));
    assert_eq!(paged(10_000), paged(10_050));
}

#[test]
fn a_first_line_longer_than_what_is_kept_is_paged_by_characters() {
    let long = format!("{}\nsecond line\nthird line", "x".repeat(200_000));
    let mut view = vec![json!({"role": "user", "content": long})];
    assert!(!page(&mut view, 20_000).is_empty());
    let text = view[0]["content"].as_str().unwrap();
    assert!(text.contains("characters") && text.contains("third line"), "{text}");
    assert!(text.len() > 100_000, "{}", text.len());
}

#[test]
fn comma_is_not_a_pair_separator() {
    let p = params("birth=squeeze,squeeze;eoq=9000").unwrap();
    assert_eq!((p.birth.as_slice(), p.eoq), (&["squeeze".to_string()][..], 9000));
    assert_eq!(refused("birth=squeeze,telegraph"), "birth has no stage \"telegraph\"");
    assert_eq!(refused("eoq=1,ledger=2"), "eoq takes a whole number of 0 or more, not \"1,ledger=2\"");
    // a cap without a view after the rewrite takes none
    assert_eq!(params("target=9000;cap=9000").unwrap().cap, 9000);
}

/// A text the window pages, sent paged otherwise in the next request, is a miss from where it is.
#[test]
fn a_text_paged_otherwise_than_in_the_last_request_counts_its_miss() {
    let paste: String = (0..2000).map(|k| format!("row {k:05} value {} ok\n", k * 7)).collect();
    let mut msgs = vec![json!({"role": "user", "content": format!("Why does this log stop?\n{paste}")})];
    let mut cur = Curator::new("").unwrap();
    cur.window(9000);
    let miss = |r: &Reply| r.stats.get("miss").copied().unwrap_or(0);
    let first = cur.view(0, &msgs, 0.0).unwrap();
    let mut prev = (first.view.clone(), miss(&first));
    let mut changed = 0;
    for k in 0..6 {
        msgs.push(
            json!({"role": "assistant", "content": format!("Step {k}. {}", "Reading the rows again. ".repeat(80))}),
        );
        msgs.push(json!({"role": "user", "content": format!("Go on with part {k}.")}));
        let r = cur.view(0, &msgs, 0.0).unwrap();
        if let Some(p) = (0..prev.0.len()).find(|&p| r.view.get(p) != Some(&prev.0[p])) {
            changed += 1;
            let behind: i64 = r.view[p..].iter().map(json::est).sum();
            assert!(
                (miss(&r) - prev.1) as f64 >= 0.9 * behind as f64 - 50.0,
                "request {k}: changed at {p}, {} of {behind}",
                miss(&r) - prev.1
            );
        }
        prev = (r.view.clone(), miss(&r));
    }
    assert!(changed > 0, "the paged text never changed");
}
