//! Seeded property tests: histories shaped as agent sessions are generated from a seed
//! (`tests/support/prop_gen.rs`), fed to the curator one request at a time as the host does, and the view of
//! each call is checked against the invariants the code promises (I1 to I11). A failure is shrunk to the
//! shortest history that still breaks the same invariant, and printed with the line of its regression test.
//! `NEXO_PROP_SEEDS=<n>` runs more seeds than the default.

#[path = "../support/prop_gen.rs"]
mod prop_gen;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use nexo_curator::{Curator, Reply, State};
use prop_gen::{
    case, config, floor, fnv, has_result, norm, recall_text, result_text, role, seed, shape, ts, typed, Case, Opts,
    Rng, TOOLS,
};
use regex::Regex;
use serde_json::{json, Value};

/// Seeds of the default run. Measured: the default run of these tests takes about 1 s in a debug build; four
/// seeds are the floor that still covers the four configs.
const DEFAULT_SEEDS: usize = 12;

/// Seeds the default run skips: each one names its `regress_seed_<hex>` test, ignored until the fix.
const KNOWN: &[(u64, &str)] = &[];

/// While S3 is open (leave's running total), the window may pass by one assistant message.
const S3_OPEN: bool = true;

const FACTS_HEAD: &str = "Background that nexo brought back from earlier in this session (use it only if this step needs it; otherwise do not mention it):";
const RECALL_HEAD: &str = "Recall (lines of earlier messages that this request names";

#[derive(Debug)]
struct Failure {
    inv: &'static str,
    call: usize,
    len: usize,
    msg: String,
}

#[derive(Default, Debug, Clone, Copy)]
struct Tally {
    calls: usize,
    i4_ran: usize,
    i4_skip: usize,
    i6_ran: usize,
    i6_skip: usize,
    i10_ran: usize,
    i10_skip: usize,
}

impl Tally {
    fn add(&mut self, o: &Tally) {
        self.calls += o.calls;
        self.i4_ran += o.i4_ran;
        self.i4_skip += o.i4_skip;
        self.i6_ran += o.i6_ran;
        self.i6_skip += o.i6_skip;
        self.i10_ran += o.i10_ran;
        self.i10_skip += o.i10_skip;
    }
}

// ---- helpers ----

fn blob(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn blobs(blocks: &[Value]) -> String {
    blob(&Value::Array(blocks.to_vec()))
}

fn kind(b: &Value) -> &str {
    b["type"].as_str().unwrap_or("")
}

fn uses(m: &Value) -> Vec<String> {
    norm(m).iter().filter(|b| kind(b) == "tool_use").filter_map(|b| b["id"].as_str().map(str::to_string)).collect()
}

fn results(m: &Value) -> Vec<String> {
    norm(m)
        .iter()
        .filter(|b| kind(b) == "tool_result")
        .filter_map(|b| b["tool_use_id"].as_str().map(str::to_string))
        .collect()
}

/// Every string value under a `text` or `content` key, recursively.
fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                match x {
                    Value::String(s) if k == "text" || k == "content" => out.push(s.clone()),
                    _ => strings(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// Every string value, whatever its key (a `tool_use` input too).
fn all_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Object(o) => o.values().for_each(|x| all_strings(x, out)),
        Value::Array(a) => a.iter().for_each(|x| all_strings(x, out)),
        _ => {}
    }
}

/// Whether `blocks` appear as a contiguous run of the blocks of `msg`, compared by their JSON.
fn run_in(msg: &Value, blocks: &[Value]) -> bool {
    let have: Vec<String> = norm(msg).iter().map(blob).collect();
    let want: Vec<String> = blocks.iter().map(blob).collect();
    want.is_empty() || have.windows(want.len()).any(|w| w == want.as_slice())
}

fn strip_tail(mut blocks: Vec<Value>) -> Vec<Value> {
    let tail = blocks.last().is_some_and(|b| {
        kind(b) == "text" && b["text"].as_str().is_some_and(|t| t.starts_with(FACTS_HEAD) || t.starts_with(RECALL_HEAD))
    });
    if tail {
        blocks.pop();
    }
    blocks
}

struct Res {
    m: Regex,
    shut: Regex,
    born: Regex,
    read_of: Regex,
    recall_n: Regex,
    to_s: Regex,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| Res {
        m: Regex::new(r"\bm(\d{5})\b").unwrap(),
        shut: Regex::new(r"The outputs? (?:is|are) ([^;]*?); recall brings").unwrap(),
        born: Regex::new(r"shortened above \((\d+) lines in full\); recall m(\d{5})").unwrap(),
        read_of: Regex::new(r"the read of (.+?) from before its edit in m(\d{5})").unwrap(),
        recall_n: Regex::new(r"recall m(\d{5}) brings it back as it was").unwrap(),
        to_s: Regex::new(r"Messages 2 to (\d+)").unwrap(),
    })
}

/// For each visible history message, the index of the view message that holds it.
fn runs(r: &Reply, h: &[Value]) -> Result<Vec<Option<usize>>, String> {
    let cells = &r.memory.cells;
    let mut map = vec![None; h.len()];
    let (mut k, mut prev): (usize, Option<&str>) = (0, None);
    let mut n = 0;
    for i in 0..h.len() {
        if cells[i].state == State::Out {
            continue;
        }
        let ro = role(&h[i]);
        if prev != Some(ro) {
            if prev.is_some() {
                k += 1;
            }
            prev = Some(ro);
            n += 1;
            if r.view.get(k).map(role) != Some(ro) {
                return Err(format!("run {k} has role {ro} but the view message does not"));
            }
        }
        map[i] = Some(k);
    }
    if n != r.view.len() {
        return Err(format!("{n} runs of visible messages, {} view messages", r.view.len()));
    }
    Ok(map)
}

// ---- the invariants ----

fn i1(view: &[Value]) -> Result<(), String> {
    for (k, m) in view.iter().enumerate() {
        let u: BTreeSet<String> = uses(m).into_iter().collect();
        if !u.is_empty() && k + 1 < view.len() {
            let next = &view[k + 1];
            let r: BTreeSet<String> = results(next).into_iter().collect();
            if role(next) != "user" || r != u {
                return Err(format!("view[{k}] calls {u:?} but view[{}] answers {r:?}", k + 1));
            }
        }
        let r: BTreeSet<String> = results(m).into_iter().collect();
        if !r.is_empty() {
            let u: BTreeSet<String> = if k >= 1 { uses(&view[k - 1]).into_iter().collect() } else { BTreeSet::new() };
            if k == 0 || role(&view[k - 1]) != "assistant" || u != r {
                return Err(format!("view[{k}] answers {r:?} but the call before it holds {u:?}"));
            }
        }
    }
    Ok(())
}

fn i2(r: &Reply, h: &[Value], map: &[Option<usize>]) -> Result<(), String> {
    let _ = map;
    let (mut use_at, mut res_at): (BTreeMap<String, usize>, BTreeMap<String, usize>) = Default::default();
    for (i, m) in h.iter().enumerate() {
        for id in uses(m) {
            use_at.insert(id, i);
        }
        for id in results(m) {
            res_at.insert(id, i);
        }
    }
    for (what, ids, at) in [
        ("tool_use", r.view.iter().flat_map(uses).collect::<Vec<_>>(), &use_at),
        ("tool_result", r.view.iter().flat_map(results).collect::<Vec<_>>(), &res_at),
    ] {
        let mut seen = BTreeSet::new();
        let mut last = 0;
        for id in &ids {
            if !seen.insert(id.clone()) {
                return Err(format!("{what} id {id} appears twice in the view"));
            }
            let i = *at.get(id).ok_or_else(|| format!("{what} id {id} is not in the history"))?;
            if i < last {
                return Err(format!("{what} id {id} (m{:05}) comes after a later message", i + 1));
            }
            last = i;
        }
    }
    Ok(())
}

fn i3(r: &Reply, h: &[Value], map: &[Option<usize>]) -> Result<(), String> {
    let (zones, cells) = (&r.memory.zones, &r.memory.cells);
    if cells.len() != h.len() {
        return Err(format!("{} cells for {} messages", cells.len(), h.len()));
    }
    if zones.is_empty() || &*zones[0].label != "anchor" {
        return Err("zone 0 is not the anchor".into());
    }
    if let Some(i) = cells.iter().position(|c| c.zone >= zones.len() || c.tokens < 1) {
        return Err(format!("cell {i} is {:?}", cells[i]));
    }
    if cells[0].state == State::Out {
        return Err("m00001 is out".into());
    }
    for (k, m) in r.view.iter().enumerate() {
        let whole = map.iter().zip(cells.iter()).all(|(at, c)| *at != Some(k) || c.state == State::Whole);
        if !whole {
            continue;
        }
        let mut s = Vec::new();
        strings(m, &mut s);
        for t in &s {
            for bad in [" output left out here", "here are omitted", "lines omitted"] {
                if t.contains(bad) {
                    return Err(format!("view[{k}] is whole but holds a cutting stub: {bad:?}"));
                }
            }
        }
    }
    Ok(())
}

fn i4(r: &Reply, h: &[Value], cfg: &prop_gen::Config, t: &mut Tally) -> Result<(), String> {
    if cfg.window == 0 {
        return Ok(());
    }
    let budget = cfg.window as i64 - cfg.system as i64;
    if floor(h, h.len()) > budget - 2_000 {
        t.i4_skip += 1;
        return Ok(());
    }
    t.i4_ran += 1;
    let slack = if S3_OPEN {
        r.view.iter().filter(|m| role(m) == "assistant").map(|m| blob(m).len() as i64 / 4).max().unwrap_or(0)
    } else {
        0
    };
    if r.tokens > budget + slack {
        return Err(format!("tokens {} > budget {budget} + slack {slack}", r.tokens));
    }
    Ok(())
}

fn miss(r: &Reply) -> i64 {
    r.stats.get("miss").copied().unwrap_or(0)
}

fn i6(p: &Reply, q: &Reply, t: &mut Tally) -> Result<(), String> {
    if miss(q) != miss(p) {
        t.i6_skip += 1;
        return Ok(());
    }
    t.i6_ran += 1;
    let l = p.view.len();
    if q.view.len() < l {
        return Err(format!("the view went from {l} to {} messages with no miss counted", q.view.len()));
    }
    for i in 0..l {
        let (a, b) = (&p.view[i], &q.view[i]);
        let same = if i + 1 < l {
            blobs(&norm(a)) == blobs(&norm(b))
        } else {
            let head = strip_tail(norm(a));
            let nb: Vec<String> = norm(b).iter().map(blob).collect();
            let na: Vec<String> = head.iter().map(blob).collect();
            nb.len() >= na.len() && nb[..na.len()] == na[..]
        };
        if role(a) != role(b) || !same {
            return Err(format!("view[{i}] changed with no miss counted"));
        }
    }
    Ok(())
}

fn find_result<'a>(h: &'a [Value], id: &str) -> Option<(usize, Value)> {
    h.iter().enumerate().find_map(|(i, m)| {
        norm(m).into_iter().find(|b| kind(b) == "tool_result" && b["tool_use_id"].as_str() == Some(id)).map(|b| (i, b))
    })
}

fn holds_result(h: &[Value], n: usize, id: &str) -> bool {
    n >= 1 && n <= h.len() && results(&h[n - 1]).iter().any(|x| x == id)
}

fn i7(r: &Reply, h: &[Value], recall: bool) -> Result<(), String> {
    let re = res();
    let len = h.len();
    for (k, m) in r.view.iter().enumerate() {
        for b in norm(m) {
            let mut all = Vec::new();
            all_strings(&b, &mut all);
            for s in &all {
                for c in re.m.captures_iter(s) {
                    let n: usize = c[1].parse().unwrap();
                    if n < 1 || n > len {
                        return Err(format!("(a) view[{k}] names m{n:05} but the history has {len} messages"));
                    }
                }
                if !recall && (s.contains("recall brings") || Regex::new(r"recall m\d").unwrap().is_match(s)) {
                    return Err(format!("(f) view[{k}] mentions recall with no recall tool"));
                }
                for c in re.shut.captures_iter(s) {
                    for n in re.m.captures_iter(&c[1]) {
                        let n: usize = n[1].parse().unwrap();
                        if n < 1 || n > len || role(&h[n - 1]) != "user" || !has_result(&h[n - 1]) {
                            return Err(format!(
                                "(e) a shut line of view[{k}] names m{n:05}, which is not a results message"
                            ));
                        }
                    }
                }
                for c in re.read_of.captures_iter(s) {
                    let (path, e): (&str, usize) = (c.get(1).unwrap().as_str(), c[2].parse().unwrap());
                    let ok = e >= 1
                        && e <= len
                        && role(&h[e - 1]) == "assistant"
                        && norm(&h[e - 1]).iter().any(|b| {
                            kind(b) == "tool_use"
                                && (b["input"]["file_path"].as_str() == Some(path)
                                    || b["input"]["command"].as_str().is_some_and(|c| c.contains(path)))
                        });
                    if !ok {
                        return Err(format!("(d) view[{k}] says the read of {path} was before its edit in m{e:05}"));
                    }
                }
            }
            if kind(&b) != "tool_result" {
                continue;
            }
            let id = b["tool_use_id"].as_str().unwrap_or("").to_string();
            let mut inner = Vec::new();
            all_strings(&b["content"], &mut inner);
            let orig = find_result(h, &id);
            for s in &inner {
                for c in re.recall_n.captures_iter(s) {
                    let n: usize = c[1].parse().unwrap();
                    if !holds_result(h, n, &id) {
                        return Err(format!(
                            "(b) the result {id} says recall m{n:05} brings it back, and m{n:05} does not hold it"
                        ));
                    }
                }
                for c in re.born.captures_iter(s) {
                    let want: usize = c[1].parse().unwrap();
                    let (_, o) = orig.as_ref().ok_or_else(|| format!("(c) the result {id} is not in the history"))?;
                    let have = result_text(o).lines().count();
                    if want != have {
                        return Err(format!("(c) the born line of {id} says {want} lines, the output has {have}"));
                    }
                }
            }
        }
    }
    Ok(())
}

fn i8(r: &Reply, h: &[Value], map: &[Option<usize>]) -> Result<(), String> {
    let vis = |i: usize| map[i].is_some();
    if !vis(0) || !run_in(&r.view[0], &norm(&h[0])) {
        return Err("(a) m00001 is not in view[0] as it was".into());
    }
    let t = ts(h, h.len());
    let mut need = vec![("(b)", t)];
    need.extend((t + 1..h.len()).filter(|&i| typed(h, i)).map(|i| ("(c)", i)));
    for (tag, i) in need {
        match map[i] {
            Some(k) if run_in(&r.view[k], &norm(&h[i])) => {}
            Some(_) => return Err(format!("{tag} the ask m{:05} is in the view but changed", i + 1)),
            None => return Err(format!("{tag} the ask m{:05} is out of the view", i + 1)),
        }
    }
    Ok(())
}

fn i9(r: &Reply, h: &[Value]) -> Result<(), String> {
    let s = r.memory.cells[h.len() - 1].state;
    if s == State::Whole {
        Ok(())
    } else {
        Err(format!("the latest message m{:05} is {s:?}", h.len()))
    }
}

fn i10(r: &Reply, h: &[Value], t: &mut Tally) -> Result<(), String> {
    let mut all = Vec::new();
    for m in &r.view {
        all_strings(m, &mut all);
    }
    if all.iter().any(|s| s.contains("left out here: the whole text does not fit")) {
        t.i10_skip += 1;
        return Ok(());
    }
    t.i10_ran += 1;
    let re = res();
    let first = r.view[0].clone();
    let mut texts0 = Vec::new();
    all_strings(&first, &mut texts0);
    let s_map: usize =
        texts0.iter().find_map(|s| re.to_s.captures(s).map(|c| c[1].parse::<usize>().unwrap())).unwrap_or(1);
    let (mut named, mut spans): (BTreeSet<usize>, Vec<(usize, usize)>) = Default::default();
    for s in &all {
        for c in re.shut.captures_iter(s) {
            let seg = &c[1];
            let mut ns: Vec<usize> = re.m.captures_iter(seg).map(|x| x[1].parse().unwrap()).collect();
            if seg.contains(" more, to m") && ns.len() >= 2 {
                let z = ns.pop().unwrap();
                spans.push((*ns.last().unwrap(), z));
            }
            named.extend(ns);
        }
    }
    for i in 1..h.len() {
        if r.memory.cells[i].state != State::Out || i < s_map {
            continue;
        }
        let j = if role(&h[i]) == "user" { i } else { i + 1 };
        let n = j + 1;
        if !named.contains(&n) && !spans.iter().any(|&(a, z)| n > a && n <= z) {
            return Err(format!("m{:05} is out (map covers to {s_map}) and no shut line names m{n:05}", i + 1));
        }
    }
    Ok(())
}

fn i11(r: &Reply, h: &[Value]) -> Result<(), String> {
    let Some(a) = (0..h.len()).rev().find(|&i| role(&h[i]) == "assistant") else { return Ok(()) };
    let have: BTreeSet<String> = r.view.iter().flat_map(norm).map(|b| blob(&b)).collect();
    for b in norm(&h[a]) {
        if matches!(kind(&b), "thinking" | "redacted_thinking") && !have.contains(&blob(&b)) {
            return Err(format!("the reasoning of the latest reply m{:05} is not in the view", a + 1));
        }
    }
    Ok(())
}

// ---- driver ----

fn make(cfg: &prop_gen::Config) -> Curator {
    let mut c = Curator::new("").expect("default spec");
    if cfg.window > 0 {
        c.window(cfg.window);
        let tools: Vec<String> = TOOLS.iter().map(|s| s.to_string()).collect();
        c.tools(&tools);
    }
    c
}

fn run(case: &Case, tally: &mut Tally) -> Result<(), Failure> {
    let cfg = &case.config;
    let (mut a, mut b) = (make(cfg), make(cfg));
    let mut prev: Option<Reply> = None;
    for (c, &len) in case.calls.iter().enumerate() {
        let h = &case.history[..len];
        let now = 1000.0 + 20.0 * c as f64;
        let fail = |inv: &'static str, msg: String| Failure { inv, call: c, len, msg };
        let ra = a.view(cfg.system, h, now).map_err(|e| fail("I0 view", e))?;
        let rb = b.view(cfg.system, h, now).map_err(|e| fail("I0 view", e))?;
        tally.calls += 1;
        if format!("{ra:?}") != format!("{rb:?}") {
            return Err(fail("I5 determinism", "two curators with the same setup answered differently".into()));
        }
        let map = runs(&ra, h).map_err(|e| fail("I2 order", e))?;
        i1(&ra.view).map_err(|e| fail("I1 pairing", e))?;
        i2(&ra, h, &map).map_err(|e| fail("I2 order", e))?;
        i3(&ra, h, &map).map_err(|e| fail("I3 memory", e))?;
        i4(&ra, h, cfg, tally).map_err(|e| fail("I4 window", e))?;
        if let (Some(p), true) = (&prev, c >= 2) {
            i6(p, &ra, tally).map_err(|e| fail("I6 changes", e))?;
        }
        i7(&ra, h, cfg.recall).map_err(|e| fail("I7 recall names", e))?;
        i8(&ra, h, &map).map_err(|e| fail("I8 asks", e))?;
        i9(&ra, h).map_err(|e| fail("I9 latest", e))?;
        if cfg.recall && cfg.window > 0 {
            i10(&ra, h, tally).map_err(|e| fail("I10 map", e))?;
        }
        i11(&ra, h).map_err(|e| fail("I11 reasoning", e))?;
        prev = Some(ra);
    }
    Ok(())
}

// ---- shrinking ----

fn shrink(case: Case, fails: &dyn Fn(&Case) -> Option<Failure>) -> (Case, Failure) {
    let first = fails(&case).expect("shrink needs a failing case");
    let inv = first.inv;
    let mut best = case.clone();
    best.history.truncate(first.len);
    best.calls.retain(|&l| l <= first.len);
    let mut cur = match fails(&best) {
        Some(f) => f,
        None => return (case, first),
    };
    let mut runs = 0;
    loop {
        let mut changed = false;
        let mut i = best.history.len();
        while i > 1 {
            i -= 1;
            if i >= best.history.len() {
                continue;
            }
            let h = &best.history;
            let has_use = !uses(&h[i]).is_empty();
            let mut gone = vec![false; h.len()];
            if role(&h[i]) == "assistant" && has_use && i + 1 < h.len() {
                gone[i] = true;
                gone[i + 1] = true;
            } else if role(&h[i]) == "user" && has_result(&h[i]) {
                continue;
            } else {
                gone[i] = true;
            }
            for (k, m) in h.iter().enumerate() {
                if norm(m).is_empty() {
                    gone[k] = true;
                }
            }
            let kept: Vec<usize> = (0..h.len()).filter(|&k| !gone[k]).collect();
            if kept.windows(2).any(|w| role(&h[w[0]]) == "assistant" && role(&h[w[1]]) == "assistant") {
                continue;
            }
            let mut calls: Vec<usize> =
                best.calls.iter().map(|&l| l - gone[..l].iter().filter(|&&g| g).count()).filter(|&l| l > 0).collect();
            calls.dedup();
            let cand = Case {
                seed: best.seed,
                config: best.config.clone(),
                history: kept.iter().map(|&k| h[k].clone()).collect(),
                calls,
            };
            runs += 1;
            if runs > 2_000 {
                return (best, cur);
            }
            if let Some(f) = fails(&cand) {
                if f.inv == inv {
                    best = cand;
                    cur = f;
                    changed = true;
                }
            }
        }
        if !changed {
            return (best, cur);
        }
    }
}

fn report(case: &Case, f: &Failure) -> String {
    let c = &case.config;
    let hex = format!("{:016x}", case.seed);
    format!(
        "{} failed at call {} (len {}): {}\nseed 0x{hex}  config {} ({}/{}, recall {})  calls {:?}\n{}\nregression: #[test] #[ignore = \"bug: {} ...\"] fn regress_seed_{hex}() {{ check_seed(0x{hex}).unwrap(); }}",
        f.inv,
        f.call,
        f.len,
        f.msg,
        c.name,
        c.window,
        c.system,
        if c.recall { "on" } else { "off" },
        case.calls,
        shape(&case.history).join("\n"),
        f.inv,
    )
}

fn check_seed(seed: u64) -> Result<Tally, String> {
    let case = case(seed, &Opts::default());
    let mut t = Tally::default();
    match run(&case, &mut t) {
        Ok(()) => Ok(t),
        Err(_) => {
            let (c, f) = shrink(case, &|c| run(c, &mut Tally::default()).err());
            Err(report(&c, &f))
        }
    }
}

fn seeds() -> usize {
    match std::env::var("NEXO_PROP_SEEDS") {
        Ok(v) => v.parse().unwrap_or_else(|_| panic!("NEXO_PROP_SEEDS must be a number, got {v:?}")),
        Err(_) => DEFAULT_SEEDS,
    }
}

// ---- tests ----

#[test]
fn invariants_hold_on_seeded_histories() {
    let mut total = Tally::default();
    for k in 0..seeds() as u64 {
        let s = seed(k);
        if KNOWN.iter().any(|(x, _)| *x == s) {
            continue;
        }
        match check_seed(s) {
            Ok(t) => total.add(&t),
            Err(report) => panic!("{report}"),
        }
    }
    eprintln!("invariants: {total:?}");
    assert!(total.i4_ran > 0, "I4 never ran: {total:?}");
    assert!(total.i6_ran > 0, "I6 never ran: {total:?}");
    assert!(total.i10_ran > 0, "I10 never ran: {total:?}");
}

/// Pin of the generator's output for `seed(0)`: any change to the generator changes it, and then every `KNOWN`
/// seed must be shrunk again and pinned again.
const GEN_PIN: u64 = 0;

#[test]
fn generator_is_deterministic() {
    let a = serde_json::to_string(&case(seed(0), &Opts::default()).history).unwrap();
    let b = serde_json::to_string(&case(seed(0), &Opts::default()).history).unwrap();
    assert_eq!(a, b);
    let pin = fnv(a.as_bytes());
    eprintln!("generator pin: {pin}");
    assert_eq!(pin, GEN_PIN, "paste the printed pin into GEN_PIN");
    assert_eq!(config(seed(3)).name, "C0");
}

#[test]
fn shrinker_keeps_a_failure() {
    let has_grep = |c: &Case| {
        c.history.iter().enumerate().any(|(i, m)| {
            role(m) == "assistant"
                && i + 1 < c.history.len()
                && norm(m).iter().any(|b| kind(b) == "tool_use" && b["name"] == "Grep")
        })
    };
    let k = (0..64).find(|&k| has_grep(&case(seed(k), &Opts::default()))).expect("a seed with a Grep call");
    let c = case(seed(k), &Opts::default());
    let fails = |c: &Case| {
        has_grep(c).then(|| Failure {
            inv: "FAKE",
            call: c.calls.len() - 1,
            len: *c.calls.last().unwrap(),
            msg: "fake".into(),
        })
    };
    let (small, f) = shrink(c, &fails);
    assert_eq!(f.inv, "FAKE");
    assert_eq!(small.history.len(), 3, "{}", shape(&small.history).join("\n"));
}

// ---- suspected bugs (see SPEC section 9) ----

fn user(content: Value) -> Value {
    json!({"role": "user", "content": content})
}

fn assistant(content: Value) -> Value {
    json!({"role": "assistant", "content": content})
}

fn result(id: &str, out: &str) -> Value {
    json!({"type": "tool_result", "tool_use_id": id, "content": out})
}

fn config_files(n: usize, chars: usize) -> Vec<Value> {
    let mut h = vec![user(json!("Compare the twelve config files."))];
    let uses: Vec<Value> = (0..12)
        .map(|k| json!({"type": "tool_use", "id": format!("c{k}"), "name": "Read", "input": {"file_path": format!("conf/c{k}.toml")}}))
        .collect();
    h.push(assistant(json!(uses)));
    let outs: Vec<Value> = (0..n)
        .map(|k| {
            let mut s = String::new();
            let mut l = 0;
            while s.len() < chars {
                s.push_str(&format!("key_{k:02}_{l:02} = \"{:016x}\"\n", fnv(format!("{k}-{l}").as_bytes())));
                l += 1;
            }
            result(&format!("c{k}"), &s)
        })
        .collect();
    h.push(user(json!(outs)));
    h
}

#[test]
fn s1a_last_results_under_2000_chars_pass_the_window() {
    let mut c = Curator::new("").unwrap();
    c.window(4_000);
    let r = c.view(1_000, &config_files(12, 1_900), 1000.0).unwrap();
    assert!(r.tokens <= 3_000, "tokens {}", r.tokens);
}

#[test]
fn s1b_image_alone_passes_the_window() {
    let mut rng = Rng::new(7);
    let img = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": prop_gen::alpha(&mut rng, 300)}});
    let mut c = Curator::new("").unwrap();
    c.window(2_000);
    let h = vec![user(json!([{"type": "text", "text": "Describe this screenshot."}, img]))];
    let r = c.view(1_000, &h, 1000.0).unwrap();
    assert!(r.tokens <= 1_000, "tokens {}", r.tokens);
}

#[test]
fn s1c_more_than_eight_long_texts_pass_the_window() {
    let mut c = Curator::new("").unwrap();
    c.window(4_000);
    let r = c.view(1_000, &config_files(12, 2_400), 1000.0).unwrap();
    assert!(r.tokens <= 3_000, "tokens {}", r.tokens);
}

#[test]
fn s2_typed_text_in_results_orphans_a_tool_result() {
    let mut rng = Rng::new(11);
    let mut h = vec![
        user(json!("Fix A in src/a.rs.")),
        assistant(
            json!([{"type": "tool_use", "id": "toolu_00001", "name": "Read", "input": {"file_path": "src/a.rs"}}]),
        ),
        user(json!([result("toolu_00001", &(0..20).map(|k| format!("line {k} of a")).collect::<Vec<_>>().join("\n"))])),
        assistant(json!([{"type": "text", "text": "Fixed."}])),
        user(json!("Now fix B in src/b.rs.")),
        assistant(
            json!([{"type": "tool_use", "id": "toolu_00005", "name": "Bash", "input": {"command": "cargo test -q"}}]),
        ),
        user(json!([result("toolu_00005", "ok"), {"type": "text", "text": "also check C"}])),
    ];
    for k in 0..30 {
        let (a, b) = (format!("toolu_{:05}", 100 + 2 * k), prop_gen::filler(&mut rng, 1_000));
        h.push(assistant(json!([{"type": "text", "text": b}, {"type": "tool_use", "id": a, "name": "Bash", "input": {"command": "true"}}])));
        h.push(user(json!([result(&format!("toolu_{:05}", 100 + 2 * k), "ok")])));
    }
    let mut c = Curator::new("").unwrap();
    c.window(6_000);
    for len in (1..=h.len()).filter(|&l| role(&h[l - 1]) == "user") {
        let r = c.view(500, &h[..len], 1000.0 + len as f64).unwrap();
        if let Err(e) = i1(&r.view) {
            panic!("len {len}: {e}");
        }
    }
}

#[test]
fn s3_leave_miscount_passes_the_window() {
    let mut over: Vec<(usize, usize, i64)> = Vec::new();
    for x in (500..=1_900).step_by(100) {
        let mut rng = Rng::new(x as u64);
        let mut c = Curator::new("ledger=0").unwrap();
        c.window(6_000);
        let mut h = vec![user(json!("Refactor the parser."))];
        for step in 1..=20 {
            let text = |rng: &mut Rng, n| json!({"type": "text", "text": prop_gen::filler(rng, n)});
            let id = format!("s{step}");
            h.push(assistant(json!([
                text(&mut rng, 1_500),
                text(&mut rng, 1_500),
                text(&mut rng, x),
                {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "cargo test -q"}}
            ])));
            h.push(user(json!([result(&id, "test result: ok. 3 passed")])));
            let r = c.view(500, &h, 1000.0 + step as f64).unwrap();
            if let Err(e) = i1(&r.view) {
                panic!("X {x} step {step}: {e}");
            }
            if r.tokens > 5_500 {
                over.push((x, step, r.tokens));
            }
        }
    }
    assert!(over.is_empty(), "(X, step, tokens) over the budget of 5500: {over:?}");
}
