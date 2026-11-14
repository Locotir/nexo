//! Replays a recorded conversation through the curator, offline, and shows what it keeps and cuts.
//!
//! `cargo run --release -p nexo-curator --features sim --example replay -- FILE [--spec SPEC] [--every N] [--quiet] [--census] [--window N] [--save N FILE]
//!  [--life S] [--gap S] [--pause N:S]`
//!
//! `FILE` is either a golden fixture (`tests/fixtures/*.jsonl`, lines `{"now", "req", "reply"}`) or a
//! `nexo bench` transcript of the plain arm (`realm-plain-0.jsonl`: each line holds the `messages` the model
//! was sent at that step and the `system` prompt). The curator sees the same conversation the model saw,
//! one request per model call, with the settings of `--spec` (over the defaults). For each request it prints
//! the messages and tokens of the raw history, the messages and tokens of the view, the share kept, whether
//! the view ends in a nudge, and what changed in the stats. At the end it prints the anchor the model would
//! read (rules, notes, pins, ledger) and every message the view sends rebuilt rather than as it was.
//!
//! Reasoning blocks are not priced beyond the few characters the engine sizes them at, and are not history
//! (they are not sent back); a reply's own reasoning counts as what it says. Each oracle line ends with the tokens it sends.
//!
//! Two measures help weigh a change before a live run:
//! - forgotten: the distinctive words (names, paths, long numbers) of each reply that the whole history
//!   held but the view the curator built for that request did not. The agent wrote them from material the
//!   curator had left out, so a view that loses more of them risks more wrong answers or reads again.
//! - blind: the tokens of results of the latest request the agent had not yet written about (no text, no `note`,
//!   and no call drawing on a word of the result since it read them) that the view left out, counted by the
//!   share of the result's lines the view lacks anywhere, as a share of all such tokens, summed over the requests.
//!   Every oracle is scored with blind, denied and forgotten words too; the "fair oracle" (digested, lines
//!   used later) is the reference the view's cost is compared with.
//!   The agent writes its next text from them; when it paraphrases, `forgotten` sees no copied word, so a view can
//!   drop what is still being read and look lossless.
//! - short numbers: numbers of three digits and decimals, which `forgotten` does not see, copied by a reply,
//!   held by the history and absent from the view (own line).
//! - the cost is also priced with a provider that keeps prefixes for 3600 s (own line, view and plain).
//! - denied: the tokens of the results the agent has just asked for that the view does not send in their place
//!   (a pointer to a copy above, a note that they were left out), as a share of all such tokens. `blind` finds
//!   the lines anywhere in the view; this asks whether the agent got the reading it asked for.
//! - own replies: the share of the agent's own text replies (with or without tool calls) each view still shows,
//!   summed over the requests, and the same for its latest ten; the other measures look at results, not at
//!   what the model already said.
//! - re-reads: the calls that ask again for what an earlier call returned (the same tool on the same target,
//!   or a read of the same file, nothing written to it since), how many of them were made with the earlier
//!   result out of the view, and how often a result the view left out before the agent acted on it (`blind`)
//!   was asked for again, next to the rate for the other results. A repeat made with the earlier result in
//!   view was not caused by the curator. `REPLAY_REREAD=1` names each one.
//! - repeated result lines: the tokens of result lines that an earlier result in the same request already
//!   holds (lines of 8 characters or more), summed over the requests, for the view and the raw history.
//! - efficiency: how much of the most a view can save it does save, priced with a prefix cache. 0 % is
//!   plain (the whole history every time), 100 % is a floor no view that forgets nothing can go under: at
//!   each request the lines of the request and, for each distinctive word a later reply copies, the smallest
//!   line (cut every 200 bytes) that holds it, every token at the hit price and each line a miss the first
//!   time it is sent. The floor ignores that the cache is a prefix, so it can be out of reach.
//!
//! Costs are priced with a prefix cache: what a request sends unchanged at its start, message by message
//! and then block by block within the first message that changed, is a hit. Without `--life` the cache holds
//! only the last request (a local server with one slot); with `--life S` it holds every request sent in the
//! last `S` seconds, and a request reads the most any of them starts with. `--gap S` spaces the requests
//! `S` seconds apart instead of the times the file holds. `--pause N:S` adds a pause of `S` seconds after every `N`
//! requests, as when the person comes back to a session later. The provider's answer (what it read, of all) goes
//! back to the curator after each request, as a host does.
//!
//! `--window N` gives the curator a window of N tokens (system prompt and tools included), as a small local
//! model has. `--save N FILE` writes the view of request N (the messages, as JSON) to FILE.
//!
//! `--bank FILE...` reads sessions of the corpus (`examples/corpus.rs`) and prints one JSON line for each:
//! the whole history, the agent's own requests (with its compaction) and the curator's view, side by side.
//! It skips the slower measures, so that hundreds of long sessions fit in memory and time.
//! Its `errs` counts failed calls that fail again on the same entity for the same cause, and whether the first
//! failure was still in the view the repeat was made from.
//!
//! The replay shows what the curator keeps and cuts at each step of a conversation that already happened.
//! It cannot show how the model would have reacted to a smaller view (whether it calls `note`, whether it
//! repeats a read): that needs the live benchmark, `nexo bench`.

use std::collections::{HashMap, HashSet};

use nexo_curator::optimum::{optimum, Cost, Piece};
use nexo_curator::sim::events::{denied, forgotten, latest_request, segments, wrote};
use nexo_curator::sim::input::{
    fixture_requests, names, reply_to, said_of, said_words, text_of, transcript_requests, Request,
};
use nexo_curator::sim::price::{blocks_of, cached, grown, tokens_of, Cache};
use nexo_curator::{distinctive_words, Curator};
use serde_json::{json, Value};

mod bank;
mod belady;
mod call_keys;
mod chunks;
mod failed_calls;
mod lost_words;
mod oracle;
mod own_replies;
mod plain_counts;
mod priced_pieces;
mod rereads;
mod sending;
use bank::*;
use belady::*;
use call_keys::*;
use chunks::*;
use failed_calls::*;
use lost_words::*;
use oracle::*;
use own_replies::*;
use plain_counts::*;
use priced_pieces::*;
use rereads::*;
use sending::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--bank") {
        let mut spec = String::new();
        let mut files = Vec::new();
        let (mut recall, mut oracle, mut dump) = (false, false, None);
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--bank" => {}
                "--recall" => recall = true,
                "--oracle" => oracle = true,
                "--dump" => dump = it.next().cloned(),
                "--spec" => spec = it.next().cloned().unwrap_or_default(),
                _ => files.push(a.clone()),
            }
        }
        for f in files {
            let line =
                bank(&f, &spec, recall, oracle, dump.as_deref()).unwrap_or_else(|e| json!({"file": f, "error": e}));
            println!("{line}");
        }
        return;
    }
    let mut path = None;
    let mut spec = String::new();
    let mut every = 1usize;
    let mut quiet = false;
    let mut census = false;
    let mut window = 0u64;
    let mut save: Option<(usize, String)> = None;
    let (mut life, mut gap, mut pause): (f64, Option<f64>, Option<(usize, f64)>) = (0.0, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--spec" => spec = it.next().cloned().unwrap_or_default(),
            "--every" => every = it.next().and_then(|n| n.parse().ok()).unwrap_or(1).max(1),
            "--quiet" => quiet = true,
            "--census" => census = true,
            "--window" => window = it.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            "--save" => {
                let n = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                save = it.next().map(|f| (n, f.clone()));
            }
            "--life" => life = it.next().and_then(|n| n.parse().ok()).unwrap_or(0.0),
            "--gap" => gap = it.next().and_then(|n| n.parse().ok()),
            "--pause" => {
                pause = it
                    .next()
                    .and_then(|p| p.split_once(':'))
                    .and_then(|(n, s)| Some((n.parse::<usize>().ok()?.max(1), s.parse().ok()?)))
            }
            _ => path = Some(a.clone()),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: replay FILE.jsonl [--spec SPEC] [--every N] [--quiet] [--census] [--window N] [--save N FILE] [--life S] [--gap S] [--pause N:S]");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let first: Value =
        text.lines().find(|l| !l.trim().is_empty()).and_then(|l| serde_json::from_str(l).ok()).unwrap_or_default();
    let requests = if first.get("req").is_some() { fixture_requests(&text) } else { transcript_requests(&text) };
    if requests.is_empty() {
        eprintln!("{path}: no requests found (expected a golden fixture or a nexo bench transcript)");
        std::process::exit(1);
    }
    let mut cur = Curator::new(&spec).unwrap_or_else(|e| {
        eprintln!("bad spec: {e}");
        std::process::exit(2);
    });
    cur.window(window);
    println!("{} requests from {path}", requests.len());
    println!(" req  msgs  raw≈tok  view  viewtok  kept  nudge stats changed");
    let mut prev_stats: Vec<(String, i64)> = Vec::new();
    let mut last_view: Vec<Value> = Vec::new();
    let mut last_msgs: Vec<Value> = Vec::new();
    let (mut sum_raw, mut sum_view) = (0i64, 0i64);
    // a prefix cache, message by message: what a view it holds sent unchanged at the start is a hit
    let (mut prev_view, mut sum_miss, mut sum_hit, mut per_view) = (Vec::<Value>::new(), 0i64, 0i64, Vec::new());
    let (mut cache, mut plain_cache) = (Cache::new(life, 0), Cache::new(life, 0));
    // the same two with a provider that keeps prefixes for an hour (one slot or `--life` shows the other end)
    let (mut long_cache, mut long_plain) = (Cache::new(3600.0, 0), Cache::new(3600.0, 0));
    let (mut long_miss, mut long_hit, mut long_plain_miss, mut long_plain_hit) = (0i64, 0i64, 0i64, 0i64);
    let (mut short_lost, mut short_copied) = (0usize, 0usize);
    let (mut replies, mut lost_replies, mut lost_words, mut lost_first) = (0, 0, 0, Vec::<String>::new());
    let mut tally = nexo_curator::LostTally::default();
    let mut copied_words = 0;
    let (mut blind_tokens, mut open_tokens) = (0i64, 0i64);
    let (mut denied_tokens, mut asked_tokens) = (0i64, 0i64);
    let (mut own_kept, mut own_all, mut own_recent_kept, mut own_recent_all) = (0usize, 0usize, 0usize, 0usize);
    let mut rereads = Rereads::default();
    // the working window: the largest view, the largest sticky set, and where each cut block was first cut
    let (mut max_view, mut max_sticky) = (0i64, 0i64);
    let mut first_cut = std::collections::BTreeMap::<(usize, usize), usize>::new();
    let (mut rep_view, mut rep_raw) = (0i64, 0i64);
    // the shapes of the views sent so far: a view the model was already sent comes back the same way
    let (mut shapes, mut revisits, mut first_revisits) = (HashSet::<String>::new(), 0usize, Vec::<String>::new());
    // the same cache with the whole history sent each time: the plain policy
    let (mut plain_miss, mut plain_hit) = (0i64, 0i64);
    // requests whose view changed a message the previous view had sent: each one pays the rest of the view again
    let mut rewrites = 0usize;
    let (mut census_view, mut census_plain) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
    for (i, r) in requests.iter().enumerate() {
        if let Some(t) = &r.tools {
            cur.tools(t);
        }
        let at = gap.map_or(r.now, |g| g * i as f64) + pause.map_or(0.0, |(n, s)| (i / n) as f64 * s);
        // the calls new in this request were made from the view of the request before
        rereads.see(&r.messages, &last_view, i + 1);
        let reply = cur.view(r.system_tokens, &r.messages, at).expect("a view");
        rereads.left_out(&r.messages, &reply.view, i + 1);
        if let Ok(spec) = std::env::var("REPLAY_RAW") {
            if let Some((n, file)) = spec.split_once(':') {
                if n.parse::<usize>().ok() == Some(i + 1) {
                    std::fs::write(file, Value::Array(r.messages.clone()).to_string()).expect("the history is written");
                }
            }
        }
        if let Some((n, file)) = &save {
            if *n == i + 1 {
                std::fs::write(file, Value::Array(reply.view.clone()).to_string()).expect("the view is written");
            }
        }
        if let Some(said) = reply_to(&requests, i) {
            let (lost, held) = forgotten(said, &r.messages, &reply.view, &r.system);
            let (lost_short, held_short) = forgotten_short(said, &r.messages, &reply.view, &r.system);
            short_lost += lost_short;
            short_copied += held_short;
            copied_words += held;
            tally.add(i + 1, &lost);
            replies += 1;
            if !lost.is_empty() {
                lost_replies += 1;
                lost_words += lost.len();
                // REPLAY_LOST=all lists every forgotten word, not just the first ones
                let all = std::env::var("REPLAY_LOST").is_ok_and(|v| v == "all");
                let room = if all { usize::MAX } else { 12usize.saturating_sub(lost_first.len()).min(3) };
                lost_first.extend(lost.iter().take(room).map(|w| format!("r{} {w}", i + 1)));
            }
        }
        let (missing, open) = blind(&r.messages, &reply.view);
        // REPLAY_BLIND=1 names each request that left out a result the agent had not yet written about
        if missing > 0 && std::env::var("REPLAY_BLIND").is_ok_and(|v| v == "1") {
            println!("r{} blind {missing} of {open} tokens", i + 1);
        }
        // REPLAY_BLIND=2 names each result left out and the rule that took the first line the view lacks
        if missing > 0 && std::env::var("REPLAY_BLIND").is_ok_and(|v| v == "2") {
            let shown: HashSet<String> = reply.view.iter().flat_map(segments).collect();
            for (mi, bi, tokens, lack) in blind_parts(&r.messages, &reply.view).into_iter().filter(|p| p.3 > 0) {
                let one = json!({"content": [blocks_of(&r.messages[mi]).get(bi).cloned().unwrap_or_default()]});
                let first = segments(&one).into_iter().find(|s| !shown.contains(s)).unwrap_or_default();
                let word = distinctive_words(&first).into_iter().next().unwrap_or_default();
                // `kept`: a copy of the word is still sent, so the line came reformatted (a fold at birth), not left out
                let rule = cur.fate(mi, &word).map_or("?", |f| if f.1 == "kept" { "none, reformatted" } else { f.1 });
                println!("r{} blind m{mi} {lack} of {tokens} tokens, rule {rule}: {first:.80}", i + 1);
            }
        }
        blind_tokens += missing;
        open_tokens += open;
        max_view = max_view.max(reply.tokens + cur.head_tokens() as i64);
        // REPLAY_VIEW=N names each request whose view (head included) passes N tokens
        if let Some(n) = std::env::var("REPLAY_VIEW").ok().and_then(|v| v.parse::<i64>().ok()) {
            let seen = reply.tokens + cur.head_tokens() as i64;
            if seen > n {
                println!("r{} view {seen} tokens, sticky {}", i + 1, cur.sticky_tokens());
            }
        }
        max_sticky = max_sticky.max(cur.sticky_tokens());
        for b in cur.cut_blocks() {
            first_cut.entry(b).or_insert(i);
        }
        let (missing, open) = denied(&r.messages, &reply.view);
        denied_tokens += missing;
        asked_tokens += open;
        let ((k, a), (rk, ra)) = own_replies(&r.messages, &reply.view, 10);
        (own_kept, own_all, own_recent_kept, own_recent_all) =
            (own_kept + k, own_all + a, own_recent_kept + rk, own_recent_all + ra);
        // REPLAY_OWN=1 names each request whose view left out one of the agent's latest ten replies
        if rk < ra && std::env::var("REPLAY_OWN").is_ok_and(|v| v == "1") {
            println!("r{} own replies: {k} of {a} kept, {rk} of the latest {ra}", i + 1);
        }
        if !shapes.insert(shape(&reply.view)) {
            revisits += 1;
            if first_revisits.len() < 6 {
                first_revisits.push(format!("r{}", i + 1));
            }
        }
        rep_view += repeated_tokens(&reply.view);
        rep_raw += repeated_tokens(&r.messages);
        let raw: i64 = r.messages.iter().map(tokens_of).sum();
        // the view in the unit the history is counted in: the engine's own estimate (`reply.tokens`) also
        // counts each message's wrapper, so a view the curator left untouched would read as larger
        let view_tok: i64 = reply.view.iter().map(tokens_of).sum();
        sum_raw += raw;
        sum_view += view_tok;
        let hit = cache.send(&reply.view, r.system_tokens as i64, at);
        sum_hit += hit;
        // counted as the plain cost is, so that a view equal to the history costs the same
        let all = reply.view.iter().map(tokens_of).sum::<i64>() + r.system_tokens as i64;
        sum_miss += all - hit;
        per_view.push((all - hit, hit));
        // what the provider answers: the tokens it read from its cache, of all
        cur.cached(hit as u64, all as u64);
        rewrites +=
            usize::from(reply.view.iter().zip(&prev_view).take_while(|(a, b)| a == b).count() < prev_view.len());
        prev_view = reply.view.clone();
        let hit = plain_cache.send(&r.messages, r.system_tokens as i64, at);
        plain_hit += hit;
        plain_miss += raw + r.system_tokens as i64 - hit;
        let all_view = reply.view.iter().map(tokens_of).sum::<i64>() + r.system_tokens as i64;
        let hit = long_cache.send(&reply.view, r.system_tokens as i64, at);
        long_hit += hit;
        long_miss += all_view - hit;
        let hit = long_plain.send(&r.messages, r.system_tokens as i64, at);
        long_plain_hit += hit;
        long_plain_miss += raw + r.system_tokens as i64 - hit;
        // REPLAY_MISS=1 prints the tokens each request misses in the cache, this view and plain
        if std::env::var("REPLAY_MISS").is_ok_and(|v| v == "1") {
            println!("r{} miss view {} plain {}", i + 1, per_view[i].0, raw + r.system_tokens as i64 - hit);
        }
        let nudge = reply.view.last().map(text_of).is_some_and(|t| t.contains("[curator] Call the `note` tool"));
        let stats: Vec<(String, i64)> = reply.stats.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let mut changed = Vec::new();
        for (k, v) in &stats {
            let before = prev_stats.iter().find(|(pk, _)| pk == k).map_or(0, |(_, pv)| *pv);
            if *v != before {
                changed.push(format!("{k} {before}→{v}"));
            }
        }
        prev_stats = stats;
        if !quiet && (i % every == 0 || i + 1 == requests.len() || !changed.is_empty()) {
            let kept = if raw > 0 { 100.0 * view_tok as f64 / raw as f64 } else { 100.0 };
            println!(
                "{:>4} {:>5} {:>8} {:>5} {:>8} {:>4.0}%  {:5} {}",
                i + 1,
                r.messages.len(),
                raw,
                reply.view.len(),
                view_tok,
                kept,
                if nudge { "yes" } else { "" },
                changed.join(", ")
            );
        }
        if census {
            nexo_curator::sim::census::add(&mut census_view, &reply.view);
            nexo_curator::sim::census::add(&mut census_plain, &r.messages);
        }
        last_view = reply.view;
        last_msgs = r.messages.clone();
    }
    if census {
        let file = std::path::Path::new(&path).file_name().map_or(path.clone(), |f| f.to_string_lossy().into_owned());
        println!("{}", json!({"file": file, "requests": requests.len(), "view": census_view, "plain": census_plain}));
    }
    println!();
    println!(
        "total over all requests: raw≈{sum_raw} tokens, view {sum_view} tokens ({:.0}% kept)",
        if sum_raw > 0 { 100.0 * sum_view as f64 / sum_raw as f64 } else { 100.0 }
    );
    println!(
        "prefix cache: miss {sum_miss} tokens, hit {sum_hit} tokens, input cost {:.4} $ at {}/{} $ per M, {rewrites} rewrites",
        dollars(sum_miss, sum_hit),
        prices().0,
        prices().1
    );
    let (low_miss, low_hit) = bound(&requests, false);
    let (once_miss, once_hit) = bound(&requests, true);
    let (plain, now, low) = (dollars(plain_miss, plain_hit), dollars(sum_miss, sum_hit), dollars(low_miss, low_hit));
    println!(
        "prefix cache kept 3600 s: input cost {:.4} $ (plain {:.4} $)",
        dollars(long_miss, long_hit),
        dollars(long_plain_miss, long_plain_hit)
    );
    println!(
        "efficiency: {:.1}% of the most a view can save (plain {plain:.4} $, this view {now:.4} $, least with nothing forgotten {low:.4} $)",
        if plain > low { 100.0 * (plain - now) / (plain - low) } else { 100.0 }
    );
    let share = |part: i64, whole: i64| if whole > 0 { 100.0 * part as f64 / whole as f64 } else { 0.0 };
    let mut fair = None;
    let mut fair_sent = 0;
    let mut fair_optimum = None;
    for (chunk, digest, what) in [
        (0, Digest::No, "whole results"),
        (1, Digest::No, "results down to the lines used later"),
        (1, Digest::Text, "results the agent has written about down to the lines used later"),
        (1, Digest::Evidence, "results the agent has written or acted on down to the lines used later"),
    ] {
        let (or, per, loss) = [false, true]
            .map(|b| oracle(&requests, b, chunk, digest))
            .map(|(m, h, per, loss)| (dollars(m, h), per, loss))
            .into_iter()
            .fold((f64::MAX, Vec::new(), Loss::default()), |a, b| if b.0 < a.0 { b } else { a });
        if digest == Digest::Text {
            fair = Some(or);
            fair_sent = loss.sent;
        }
        if digest == Digest::Text && std::env::var("REPLAY_GAP").is_ok_and(|v| v == "1") {
            // the requests where this view costs most over the fair oracle, in millionths of a dollar
            let mut gaps: Vec<(usize, f64, f64)> = per_view
                .iter()
                .zip(&per)
                .enumerate()
                .map(|(i, (&(vm, vh), &(om, oh)))| (i + 1, 1e6 * dollars(vm, vh), 1e6 * dollars(om, oh)))
                .collect();
            gaps.sort_by(|a, b| (b.1 - b.2).total_cmp(&(a.1 - a.2)));
            for (i, v, o) in gaps.iter().take(12) {
                println!("r{i} gap {:.0} µ$ (view {v:.0}, fair oracle {o:.0})", v - o);
            }
        }
        println!(
            "oracle that cuts {what}: {or:.4} $; this view reaches {:.1}% of what it saves over plain; blind {} of {} tokens ({:.1}%), denied {} of {} ({:.1}%), forgotten {} words; sends {} tokens",
            if plain > or { 100.0 * (plain - now) / (plain - or) } else { 100.0 },
            loss.blind.0,
            loss.blind.1,
            share(loss.blind.0, loss.blind.1),
            loss.denied.0,
            loss.denied.1,
            share(loss.denied.0, loss.denied.1),
            loss.forgotten,
            loss.sent
        );
        let Some(pieces) = optimum_pieces(&requests, chunk, digest) else {
            println!("  optimum, same rules: not defined, the history is rewritten between requests");
            continue;
        };
        let (lo, hi) = priced_optimum(&pieces, requests.len());
        let usd = |x: f64| x / 1e6;
        println!(
            "  optimum, same rules: {:.4} $ to {:.4} $ (rent {:.4} + rewrites {:.4}, {} cuts{}); the oracle above is {:.1} % over it",
            usd(lo.total),
            usd(hi.total),
            usd(lo.rent),
            usd(lo.rewrite),
            lo.cuts,
            if pieces.coarse { ", coarse" } else { "" },
            if lo.total > 0.0 { 100.0 * (or - usd(lo.total)) / usd(lo.total) } else { 0.0 }
        );
        if digest == Digest::Text {
            fair_optimum = Some((lo, hi, pieces.coarse));
        }
    }
    if let Some(fair) = fair {
        println!(
            "fair oracle (reference: digested, lines used later): {fair:.4} $; this view {now:.4} $ is {:+.4} $ ({:+.1}%) from it; sends {fair_sent} tokens",
            now - fair,
            if fair > 0.0 { 100.0 * (now - fair) / fair } else { 0.0 }
        );
        if let Some((lo, hi, coarse)) = fair_optimum {
            let (m, h) = prices();
            let (rent, rewrite) = (h * (sum_miss + sum_hit) as f64 / 1e6, (m - h) * sum_miss as f64 / 1e6);
            let low = lo.total / 1e6;
            println!(
                "exact optimum (reference): {low:.4} $ to {:.4} $; this view ({now:.4} $ = rent {rent:.4} + rewrites {rewrite:.4}) is {:+.1} % from it{}",
                hi.total / 1e6,
                if low > 0.0 { 100.0 * (now - low) / low } else { 0.0 },
                if coarse { " (coarse)" } else { "" }
            );
        }
        let kinds = plain_kinds(&requests);
        let sum: f64 = kinds.iter().sum();
        let pct = |k: usize| if sum > 0.0 { 100.0 * kinds[k] / sum } else { 0.0 };
        println!(
            "plain mode sends {:.0} tokens: system and tools {:.1} %, user {:.1} %, assistant text {:.1} %, assistant tool arguments {:.1} %, tool results {:.1} %",
            sum,
            pct(0),
            pct(1),
            pct(2),
            pct(3),
            pct(4)
        );
    }
    if std::env::var("REPLAY_CHUNKS").is_ok_and(|v| v == "1") {
        // how much of the saving is left when a cut keeps or drops whole runs of lines, not single lines
        for chunk in [2, 4, 8, 16, 32, 64] {
            let or = [false, true]
                .map(|b| {
                    let (m, h, _, _) = oracle(&requests, b, chunk, Digest::No);
                    dollars(m, h)
                })
                .into_iter()
                .fold(f64::MAX, f64::min);
            println!("oracle that cuts results down to the runs of {chunk} lines used later: {or:.4} $");
        }
    }
    if let Ok(out) = std::env::var("REPLAY_PREDICT") {
        // what predicts that a run of result lines is used later: rows for a study, appended to a file
        use std::io::Write;
        let (rows, texts) = chunk_rows(&requests);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&out).expect("REPLAY_PREDICT file");
        for row in &rows {
            writeln!(f, "{row}").expect("REPLAY_PREDICT write");
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{out}.texts"))
            .expect("REPLAY_PREDICT texts file");
        for (k, t) in &texts {
            writeln!(f, "{}", json!({"k": k.to_string(), "text": t})).expect("REPLAY_PREDICT write");
        }
        println!("predict: {} rows to {out}, {} texts to {out}.texts", rows.len(), texts.len());
    }
    let once = dollars(once_miss, once_hit);
    println!(
        "efficiency, each new message seen once: {:.1}% (least {once:.4} $)",
        if plain > once { 100.0 * (plain - now) / (plain - once) } else { 100.0 }
    );
    println!(
        "forgotten: {lost_words} words in {lost_replies} of {replies} replies, of {copied_words} words the replies took from the history{}",
        if lost_first.is_empty() { String::new() } else { format!(" (first: {})", lost_first.join(", ")) }
    );
    if let Some((r, n)) = tally.largest {
        println!(
            "forgotten by kind: {} numbers, {} ids/paths, {} other; {} distinct; largest reply r{r} holds {n} ({:.0}%), {} outside it",
            tally.numbers,
            tally.identifiers,
            tally.other,
            tally.distinct(),
            tally.share_of_largest(),
            tally.without_largest()
        );
    } else {
        println!("forgotten by kind: 0 numbers, 0 ids/paths, 0 other; 0 distinct");
    }
    println!(
        "forgotten short numbers (3 digits, decimals): {short_lost} of {short_copied} the replies took from the history"
    );
    println!(
        "blind: {blind_tokens} of {open_tokens} tokens of results not yet written about or acted on were left out ({:.1}%)",
        share(blind_tokens, open_tokens)
    );
    // lines of a block a window cut took out that a reply written after the cut cites word for word
    let mut cited = 0usize;
    for (&(mi, bi), &at) in &first_cut {
        let Some(block) = last_msgs.get(mi).map(blocks_of).and_then(|b| b.get(bi).cloned()) else { continue };
        let text = text_of(&json!({"content": [block]}));
        let from = requests[at].messages.len();
        let replies: Vec<String> = last_msgs
            .iter()
            .skip(from)
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            .map(text_of)
            .collect();
        let lines: HashSet<&str> = text.lines().map(str::trim).filter(|l| l.chars().count() >= 16).collect();
        let hit: Vec<&&str> = lines.iter().filter(|l| replies.iter().any(|r| r.contains(**l))).collect();
        // REPLAY_CITED=1 names the lines of a cut block that a later reply cites
        if std::env::var("REPLAY_CITED").is_ok_and(|v| v == "1") {
            for l in &hit {
                println!("cited after the cut at r{}: m{mi} b{bi}: {l}", at + 1);
            }
        }
        cited += hit.len();
    }
    println!(
        "window: max view {max_view} tokens, sticky set at most {max_sticky} tokens, {} blocks cut, sticky lines dropped (lines of a cut block cited by a later reply): {cited}",
        first_cut.len()
    );
    println!(
        "denied: {denied_tokens} of {asked_tokens} tokens of the results just asked for were not sent in their place ({:.1}%)",
        share(denied_tokens, asked_tokens)
    );
    println!(
        "own replies: {own_kept} of {own_all} of the agent's text replies were in the views ({:.1}%); of the latest ten, {own_recent_kept} of {own_recent_all} ({:.1}%)",
        share(own_kept as i64, own_all as i64),
        share(own_recent_kept as i64, own_recent_all as i64)
    );
    let (unused, unused_again, other, other_again) = rereads.rates();
    println!(
        "re-reads: {} calls asked again for an earlier result (same tool and target, or a read of the same file, unchanged), {} of them with it out of the view; reads {}, {} out; results left out before the agent acted on them: {unused_again} of {unused} asked for again ({:.1}%), other results: {other_again} of {other} ({:.1}%)",
        rereads.repeats,
        rereads.out_of_view,
        rereads.reads,
        rereads.reads_out,
        share(unused_again as i64, unused as i64),
        share(other_again as i64, other as i64)
    );
    // REPLAY_REREAD=1 names each call that asked again
    if std::env::var("REPLAY_REREAD").is_ok_and(|v| v == "1") {
        for l in &rereads.lines {
            println!("{l}");
        }
    }
    println!(
        "revisits: {revisits} requests sent a view already sent before, call ids and the counts in curator lines aside{}",
        if first_revisits.is_empty() { String::new() } else { format!(" (first: {})", first_revisits.join(", ")) }
    );
    println!(
        "repeated result lines: view {rep_view} tokens ({:.0}% of the view), raw history {rep_raw} tokens ({:.0}%)",
        share(rep_view, sum_view),
        share(rep_raw, sum_raw)
    );
    println!(
        "final stats: {}",
        json!(prev_stats.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>())
    );

    // the last view: which messages go as they are, which are rebuilt, and the anchor
    let raw_index: HashMap<String, usize> = last_msgs.iter().enumerate().map(|(i, m)| (m.to_string(), i)).collect();
    let mut plain = 0;
    let mut rebuilt = Vec::new();
    for (j, v) in last_view.iter().enumerate() {
        if raw_index.contains_key(&v.to_string()) {
            plain += 1;
        } else {
            rebuilt.push((j, v));
        }
    }
    println!();
    println!(
        "last view: {} of {} messages sent as they are, {} rebuilt by the curator",
        plain,
        last_msgs.len(),
        rebuilt.len()
    );
    for (j, v) in &rebuilt {
        let t = text_of(v);
        let role = v["role"].as_str().unwrap_or("?");
        let one_line: String = t.chars().take(110).map(|c| if c == '\n' { ' ' } else { c }).collect();
        println!("  view[{j}] {role} {}tok: {one_line}", tokens_of(v));
    }
    if let Some(anchor) = last_view.first().filter(|v| !raw_index.contains_key(&v.to_string())) {
        println!();
        println!("===== anchor (the first message of the last view, rebuilt by the curator) =====");
        println!("{}", text_of(anchor));
    } else if let Some(first) = last_view.first() {
        // the anchor is folded into the first message: print the part that is not the original text
        let orig = last_msgs.first().map(text_of).unwrap_or_default();
        let now = text_of(first);
        if now.len() > orig.len() {
            println!();
            println!("===== added to the first message by the curator =====");
            println!("{}", &now[orig.len()..]);
        }
    }
}

/// A view with what a model cannot tell apart left out: call ids, and the numbers in the curator's own lines
/// (a count that went up by one). A model sent a view of the same shape as one before, at a fixed
/// temperature, takes the same step again; a history that only ever grows never does this.
fn shape(view: &[Value]) -> String {
    fn walk(v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.iter()
                    .filter(|(k, _)| *k != "id" && *k != "tool_use_id")
                    .map(|(k, x)| (k.clone(), walk(x)))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(walk).collect()),
            Value::String(s) if s.contains("[curator]") => Value::String(
                s.lines()
                    .map(|l| {
                        if l.contains("[curator]") {
                            l.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect()
                        } else {
                            l.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            x => x.clone(),
        }
    }
    view.iter().map(|m| walk(m).to_string()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(call: Value) -> Vec<Value> {
        let listing = (0..40).map(|i| format!("quillmark_module_{i}.rs")).collect::<Vec<_>>().join("\n");
        vec![
            json!({"role": "user", "content": "list the files"}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "ls", "input": {"p": "."}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": listing}]}),
            call,
        ]
    }

    fn dropped(h: &[Value]) -> Vec<Value> {
        let mut view = h.to_vec();
        view[2] = json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "[out]"}]});
        view
    }

    #[test]
    fn blind_is_zero_when_a_later_call_drew_on_the_result() {
        let credited = history(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "b", "name": "cat", "input": {"path": "quillmark_module_7.rs"}}]}));
        let h = credited;
        assert_eq!(blind(&h, &dropped(&h)), (0, 0));
        let unrelated = history(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "b", "name": "cat", "input": {"path": "somewhere_else.rs"}}]}));
        let (missing, open) = blind(&unrelated, &dropped(&unrelated));
        assert!(open > 0 && missing == open, "an unread result left out is blind: {missing} of {open}");
        assert_eq!(blind(&unrelated, &unrelated), (0, open));
    }

    #[test]
    fn short_numbers_are_three_digits_and_decimals_standing_alone() {
        let found = short_numbers("paid 250 coins, rate 0.75, v1.2 and abc123 and 1234 and 12");
        let mut found: Vec<_> = found.into_iter().collect();
        found.sort();
        assert_eq!(found, ["0.75", "250"]);
    }
}
