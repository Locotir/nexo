//! Fits and judges the need heads outside the replay, on what `replay --bank --dump DIR` wrote: for each
//! session, the features of every unit the view held at some of its requests, the shipped heads' guesses
//! for it, and at the end the replies each unit came to be used by.
//!
//! `needfit DIR [--runs BANK.jsonl] [--base model.json] [--out model.json] [--horizons 1,4,16,64,256,0]
//! [--epochs N] [--l2 X] [--bins N] [--old] [--caps 0.25,0.5]`
//!
//! Sessions split as `judge.sh` does (sum of the bytes of the file name, 3 ways: 0 is test, the rest dev).
//! Each head is an additive logistic model over binned features (the shape `learn::Head` reads), fitted
//! on the dev rows by cyclic Newton steps per bin with an L2 penalty. For each horizon: log loss, AUC and
//! calibration on dev and test, old heads next to new, and the regret against Belady at the same cap:
//! at each dumped request, the units kept under a share of the tokens held, ranked by the guess per token,
//! against those kept knowing the next use.

// the fits walk features and rows by index over several parallel tables
#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};

use nexo_curator::{K_OLD, NEED_FEATURES as NAMES};

const NF: usize = NAMES.len();
/// The feature that says how many steps ago the unit was made.
const CREATED: usize = 10;

struct Row {
    session: u32,
    t: u32,
    tokens: i64,
    /// Steps to the next reply that draws on the unit (`u32::MAX`: none).
    next: u32,
    p_turn: f32,
    p_keep: f32,
    x: Vec<f32>,
}

#[derive(Default)]
struct Segment {
    xs: Vec<(u32, usize, i64, f32, f32, Vec<f32>)>,
    uses: HashMap<usize, Vec<usize>>,
    steps: Vec<usize>,
}

fn parse_session(path: &std::path::Path, session: u32, rows: &mut Vec<Row>) -> std::io::Result<()> {
    let f = BufReader::new(std::fs::File::open(path)?);
    let mut seg = Segment::default();
    for line in f.lines() {
        let line = line?;
        let mut it = line.split('\t');
        match it.next() {
            Some("x") => {
                let t: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let i: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let _mi = it.next();
                let tokens: i64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let pt: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let pk: f32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let x: Vec<f32> = it.map(|v| v.parse().unwrap_or(0.0)).collect();
                if x.len() == NF {
                    seg.xs.push((t, i, tokens, pt, pk, x));
                }
            }
            Some("u") => {
                let i: usize = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let _msg = it.next();
                let _kind = it.next();
                let _tokens = it.next();
                let uses: Vec<usize> = it
                    .next()
                    .unwrap_or("")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                seg.uses.insert(i, uses);
            }
            Some("s") => {
                seg.steps = it.next().unwrap_or("").split(',').filter_map(|s| s.parse().ok()).collect();
            }
            Some("e") => {
                let seg = std::mem::take(&mut seg);
                for (t, i, tokens, pt, pk, x) in seg.xs {
                    let t_us = t as usize;
                    let k = seg.steps.partition_point(|&m| m < t_us);
                    let next = seg
                        .uses
                        .get(&i)
                        .and_then(|us| us.iter().find(|&&m| m >= t_us))
                        .map(|&m| (seg.steps.partition_point(|&s| s < m) - k + 1) as u32)
                        .unwrap_or(u32::MAX);
                    rows.push(Row { session, t, tokens, next, p_turn: pt, p_keep: pk, x });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The class of a session by its requests, as `classes.sh` names them.
fn class_of(requests: usize) -> &'static str {
    match requests {
        0..=5 => "xs",
        6..=20 => "s",
        21..=80 => "m",
        81..=300 => "l",
        301..=1000 => "xl",
        _ => "xxl",
    }
}

const CLASSES: [&str; 6] = ["xs", "s", "m", "l", "xl", "xxl"];

fn is_test(name: &str) -> bool {
    name.bytes().map(|b| b as u64).sum::<u64>() % 3 == 0
}

struct Head {
    edges: Vec<Vec<f32>>,
    w: Vec<Vec<f64>>,
    b: f64,
}

impl Head {
    fn bin(&self, f: usize, v: f32) -> usize {
        self.edges[f].partition_point(|&e| e <= v)
    }

    fn z(&self, x: &[f32]) -> f64 {
        let mut z = self.b;
        for f in 0..NF {
            z += self.w[f][self.bin(f, x[f])];
        }
        z
    }

    fn prob(&self, x: &[f32]) -> f64 {
        sigmoid(self.z(x))
    }

    fn json(&self, h: f64) -> Value {
        json!({"h": h, "edges": self.edges, "w": self.w, "b": self.b})
    }
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z.clamp(-30.0, 30.0)).exp())
}

/// Quantile edges of each feature over the rows (binary features get one edge).
fn edges_of(rows: &[&Row], bins: usize) -> Vec<Vec<f32>> {
    let mut out = Vec::with_capacity(NF);
    let step = (rows.len() / 200_000).max(1);
    for f in 0..NF {
        let mut vals: Vec<f32> = rows.iter().step_by(step).map(|r| r.x[f]).collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        vals.dedup();
        if vals.len() <= 1 {
            out.push(vec![0.5]);
            continue;
        }
        if vals.len() <= bins {
            // one bin per value: the edges sit between them
            let e: Vec<f32> = vals.windows(2).map(|w| (w[0] + w[1]) / 2.0).collect();
            out.push(e);
            continue;
        }
        let all: Vec<f32> = {
            let mut v: Vec<f32> = rows.iter().step_by(step).map(|r| r.x[f]).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v
        };
        let mut e: Vec<f32> = Vec::new();
        for q in 1..bins {
            let v = all[(all.len() * q / bins).min(all.len() - 1)];
            if e.last().is_none_or(|&l| v > l) && v > all[0] {
                e.push(v);
            }
        }
        if e.is_empty() {
            e.push((vals[0] + vals[1]) / 2.0);
        }
        out.push(e);
    }
    out
}

/// Fits one head on `rows` with labels `y` by cyclic Newton steps per bin.
fn fit(rows: &[&Row], y: &[u8], edges: Vec<Vec<f32>>, epochs: usize, l2: f64) -> Head {
    let n = rows.len();
    let bins: Vec<Vec<u8>> =
        (0..NF).map(|f| rows.iter().map(|r| edges[f].partition_point(|&e| e <= r.x[f]) as u8).collect()).collect();
    let pos = y.iter().filter(|&&v| v == 1).count() as f64;
    let share = (pos + 0.5) / (n as f64 + 1.0);
    let mut head =
        Head { w: edges.iter().map(|e| vec![0.0; e.len() + 1]).collect(), edges, b: (share / (1.0 - share)).ln() };
    let mut z: Vec<f64> = vec![head.b; n];
    for _ in 0..epochs {
        for f in 0..NF {
            let nb = head.w[f].len();
            let (mut g, mut h) = (vec![0.0f64; nb], vec![0.0f64; nb]);
            for r in 0..n {
                let p = sigmoid(z[r]);
                let k = bins[f][r] as usize;
                g[k] += y[r] as f64 - p;
                h[k] += p * (1.0 - p);
            }
            for k in 0..nb {
                let d = (g[k] - l2 * head.w[f][k]) / (h[k] + l2);
                head.w[f][k] += d;
                if d != 0.0 {
                    for r in 0..n {
                        if bins[f][r] as usize == k {
                            z[r] += d;
                        }
                    }
                }
            }
        }
        // the bias
        let (mut g, mut h) = (0.0, 0.0);
        for r in 0..n {
            let p = sigmoid(z[r]);
            g += y[r] as f64 - p;
            h += p * (1.0 - p);
        }
        let d = g / (h + 1e-9);
        head.b += d;
        z.iter_mut().for_each(|v| *v += d);
    }
    head
}

struct Eval {
    n: usize,
    pos: usize,
    logloss: f64,
    auc: f64,
    ece: f64,
}

fn evaluate(p: &[f64], y: &[u8]) -> Eval {
    let n = p.len();
    let pos = y.iter().filter(|&&v| v == 1).count();
    let mut ll = 0.0;
    for (pi, &yi) in p.iter().zip(y) {
        let q = pi.clamp(1e-6, 1.0 - 1e-6);
        ll -= if yi == 1 { q.ln() } else { (1.0 - q).ln() };
    }
    // AUC with ties: ranks averaged
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| p[a].partial_cmp(&p[b]).unwrap_or(std::cmp::Ordering::Equal));
    let mut rank_sum = 0.0;
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && p[idx[j + 1]] == p[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for &r in &idx[i..=j] {
            if y[r] == 1 {
                rank_sum += avg;
            }
        }
        i = j + 1;
    }
    let neg = n - pos;
    let auc = if pos == 0 || neg == 0 {
        0.5
    } else {
        (rank_sum - pos as f64 * (pos as f64 + 1.0) / 2.0) / (pos as f64 * neg as f64)
    };
    // expected calibration error over 10 bins of p
    let (mut sp, mut sy, mut cnt) = ([0.0f64; 10], [0.0f64; 10], [0usize; 10]);
    for (pi, &yi) in p.iter().zip(y) {
        let k = ((pi * 10.0) as usize).min(9);
        sp[k] += pi;
        sy[k] += yi as f64;
        cnt[k] += 1;
    }
    let ece: f64 = (0..10).filter(|&k| cnt[k] > 0).map(|k| (sp[k] - sy[k]).abs()).sum::<f64>() / n.max(1) as f64;
    Eval { n, pos, logloss: ll / n.max(1) as f64, auc, ece }
}

/// Regret against Belady at a cap: over the groups (session, request), the units with a use ahead that
/// each ranking leaves out, keeping units in its order while they fit in `cap` of the tokens held.
struct Regret {
    belady: usize,
    belady_w: usize,
    by_p: usize,
    by_p_density: usize,
    old_keep_density: usize,
    old_turn_density: usize,
    /// The window: the newest kept first, as `leave` does.
    fifo: usize,
    groups: usize,
}

fn regret(groups: &[Vec<usize>], rows: &[&Row], p: &[f64], cap: f64) -> Regret {
    let mut out = Regret {
        belady: 0,
        belady_w: 0,
        by_p: 0,
        by_p_density: 0,
        old_keep_density: 0,
        old_turn_density: 0,
        fifo: 0,
        groups: 0,
    };
    for g in groups {
        let total: i64 = g.iter().map(|&r| rows[r].tokens).sum();
        let budget = (total as f64 * cap) as i64;
        let lost = |order: &[usize]| -> usize {
            let mut used = 0i64;
            let mut lost = 0usize;
            for &r in order {
                if used + rows[r].tokens <= budget {
                    used += rows[r].tokens;
                } else if rows[r].next != u32::MAX {
                    lost += 1;
                }
            }
            lost
        };
        let mut o: Vec<usize> = g.clone();
        o.sort_by_key(|&r| (rows[r].next, rows[r].tokens));
        out.belady += lost(&o);
        o.sort_by_key(|&r| (rows[r].next as i64).saturating_mul(rows[r].tokens.max(1)));
        out.belady_w += lost(&o);
        o.sort_by(|&a, &b| p[b].partial_cmp(&p[a]).unwrap_or(std::cmp::Ordering::Equal));
        out.by_p += lost(&o);
        let dens = |v: f64, r: usize| v / rows[r].tokens.max(1) as f64;
        o.sort_by(|&a, &b| dens(p[b], b).partial_cmp(&dens(p[a], a)).unwrap_or(std::cmp::Ordering::Equal));
        out.by_p_density += lost(&o);
        o.sort_by(|&a, &b| {
            dens(rows[b].p_keep as f64, b)
                .partial_cmp(&dens(rows[a].p_keep as f64, a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.old_keep_density += lost(&o);
        o.sort_by(|&a, &b| {
            dens(rows[b].p_turn as f64, b)
                .partial_cmp(&dens(rows[a].p_turn as f64, a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.old_turn_density += lost(&o);
        o.sort_by(|&a, &b| rows[a].x[CREATED].partial_cmp(&rows[b].x[CREATED]).unwrap_or(std::cmp::Ordering::Equal));
        out.fifo += lost(&o);
        out.groups += 1;
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut dir = None;
    let mut runs: Option<String> = None;
    let mut base = "crates/nexo-curator/data/model.json".to_string();
    let mut out: Option<String> = None;
    let mut horizons: Vec<f64> = vec![1.0, 4.0, 16.0, 64.0, 256.0, 0.0];
    let mut epochs = 30usize;
    let mut l2 = 2.0f64;
    let mut bins = 8usize;
    let mut caps: Vec<f64> = vec![0.5];
    let mut only_old: Vec<usize> = Vec::new();
    let mut drop: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => runs = it.next().cloned(),
            "--base" => base = it.next().cloned().unwrap_or(base),
            "--out" => out = it.next().cloned(),
            "--horizons" => {
                horizons = it.next().map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect()).unwrap_or(horizons)
            }
            "--epochs" => epochs = it.next().and_then(|v| v.parse().ok()).unwrap_or(epochs),
            "--l2" => l2 = it.next().and_then(|v| v.parse().ok()).unwrap_or(l2),
            "--bins" => bins = it.next().and_then(|v| v.parse().ok()).unwrap_or(bins),
            "--caps" => caps = it.next().map(|s| s.split(',').filter_map(|v| v.parse().ok()).collect()).unwrap_or(caps),
            // the first K_OLD features only (what the shipped heads see), to tell the data from the features
            "--old" => only_old = (0..K_OLD).collect(),
            "--drop" => drop = it.next().map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default(),
            _ => dir = Some(a.clone()),
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: needfit DIR [--runs BANK.jsonl] [--base model.json] [--out model.json] [--horizons ..] [--epochs N] [--l2 X] [--bins N] [--caps ..] [--old] [--drop a,b]");
        std::process::exit(2);
    };
    // the class of each session, from the bank's lines
    let mut requests: HashMap<String, usize> = HashMap::new();
    if let Some(r) = &runs {
        if let Ok(text) = std::fs::read_to_string(r) {
            for line in text.lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let (Some(f), Some(n)) = (v["file"].as_str(), v["requests"].as_u64()) {
                        requests.insert(f.to_string(), n as usize);
                    }
                }
            }
        }
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&dir).expect("dump dir").filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(file) = name.strip_suffix(".tsv") else { continue };
        let session = names.len() as u32;
        names.push(file.to_string());
        if let Err(err) = parse_session(&e.path(), session, &mut rows) {
            eprintln!("{name}: {err}");
        }
    }
    let class: Vec<&'static str> = names.iter().map(|n| class_of(requests.get(n).copied().unwrap_or(0))).collect();
    let test: Vec<bool> = names.iter().map(|n| is_test(n)).collect();
    eprintln!("{} rows from {} sessions ({} test)", rows.len(), names.len(), test.iter().filter(|&&t| t).count());
    // features left out: their column is zeroed
    let dropped: Vec<usize> = (0..NF)
        .filter(|&f| drop.iter().any(|d| d == NAMES[f]) || (!only_old.is_empty() && !only_old.contains(&f)))
        .collect();
    for r in &mut rows {
        for &f in &dropped {
            r.x[f] = 0.0;
        }
    }
    let dev: Vec<&Row> = rows.iter().filter(|r| !test[r.session as usize]).collect();
    let tst: Vec<&Row> = rows.iter().filter(|r| test[r.session as usize]).collect();
    // groups (session, request) of the test rows, for the regret
    let mut groups_of: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (k, r) in tst.iter().enumerate() {
        groups_of.entry((r.session, r.t)).or_default().push(k);
    }
    let mut heads: Vec<Value> = Vec::new();
    let label = |r: &Row, h: f64| -> u8 { u8::from(if h <= 0.0 { r.next != u32::MAX } else { r.next as f64 <= h }) };
    println!(
        "{:>5} {:>4} {:>8} {:>7} {:>7} {:>6} {:>6} | {:>7} {:>6} {:>6} | {:>7} {:>6}",
        "h", "set", "n", "pos%", "ll", "auc", "ece", "ll_old", "auc_k", "auc_t", "ll_pri", "ratio"
    );
    for &h in &horizons {
        let y_dev: Vec<u8> = dev.iter().map(|r| label(r, h)).collect();
        let edges = edges_of(&dev, bins);
        let head = fit(&dev, &y_dev, edges, epochs, l2);
        for (set, rs, ys) in
            [("dev", &dev, y_dev.clone()), ("test", &tst, tst.iter().map(|r| label(r, h)).collect::<Vec<u8>>())]
        {
            let p: Vec<f64> = rs.iter().map(|r| head.prob(&r.x)).collect();
            let ev = evaluate(&p, &ys);
            // the shipped heads: the keep head against ever, the turn head against the short horizons
            let old: Vec<f64> = rs.iter().map(|r| if h <= 0.0 { r.p_keep as f64 } else { r.p_turn as f64 }).collect();
            let eo = evaluate(&old, &ys);
            let pk: Vec<f64> = rs.iter().map(|r| r.p_keep as f64).collect();
            let pt: Vec<f64> = rs.iter().map(|r| r.p_turn as f64).collect();
            let (ak, at) = (evaluate(&pk, &ys).auc, evaluate(&pt, &ys).auc);
            let prior = ev.pos as f64 / ev.n.max(1) as f64;
            let ll_prior = -(prior.max(1e-6).ln() * prior + (1.0 - prior).max(1e-6).ln() * (1.0 - prior));
            println!(
                "{:>5} {:>4} {:>8} {:>6.2}% {:>7.4} {:>6.3} {:>6.3} | {:>7.4} {:>6.3} {:>6.3} | {:>7.4} {:>6.3}",
                h,
                set,
                ev.n,
                100.0 * prior,
                ev.logloss,
                ev.auc,
                ev.ece,
                eo.logloss,
                ak,
                at,
                ll_prior,
                ev.logloss / ll_prior
            );
            if set == "test" {
                for c in CLASSES {
                    let sel: Vec<usize> = (0..rs.len()).filter(|&k| class[rs[k].session as usize] == c).collect();
                    if sel.len() < 50 {
                        continue;
                    }
                    let pc: Vec<f64> = sel.iter().map(|&k| p[k]).collect();
                    let yc: Vec<u8> = sel.iter().map(|&k| ys[k]).collect();
                    let oc: Vec<f64> = sel.iter().map(|&k| old[k]).collect();
                    let (e, o) = (evaluate(&pc, &yc), evaluate(&oc, &yc));
                    println!(
                        "        {c:>4} {:>8} {:>6.2}% {:>7.4} {:>6.3} {:>6.3} | {:>7.4} {:>6.3}",
                        e.n,
                        100.0 * e.pos as f64 / e.n.max(1) as f64,
                        e.logloss,
                        e.auc,
                        e.ece,
                        o.logloss,
                        o.auc
                    );
                }
            }
        }
        // the regret against Belady at each cap, on the test groups, by class
        let p_test: Vec<f64> = tst.iter().map(|r| head.prob(&r.x)).collect();
        for &cap in &caps {
            for c in CLASSES {
                let gs: Vec<Vec<usize>> =
                    groups_of.values().filter(|g| class[tst[g[0]].session as usize] == c).cloned().collect();
                if gs.is_empty() {
                    continue;
                }
                let rg = regret(&gs, &tst, &p_test, cap);
                if rg.belady == 0 && rg.by_p_density == 0 {
                    continue;
                }
                println!(
                    "  regret h={h} cap={cap} {c:>3}: groups {} belady {} belady_w {} new_p {} new_p/tok {} old_keep/tok {} old_turn/tok {} fifo {}",
                    rg.groups, rg.belady, rg.belady_w, rg.by_p, rg.by_p_density, rg.old_keep_density, rg.old_turn_density, rg.fifo
                );
            }
        }
        // what each feature is worth: the dev log loss without it
        if h <= 0.0 || h == 4.0 {
            let base_ll = evaluate(&dev.iter().map(|r| head.prob(&r.x)).collect::<Vec<f64>>(), &y_dev).logloss;
            let mut gains: Vec<(f64, usize)> = (0..NF)
                .map(|f| {
                    let p: Vec<f64> =
                        dev.iter().map(|r| sigmoid(head.z(&r.x) - head.w[f][head.bin(f, r.x[f])])).collect();
                    (evaluate(&p, &y_dev).logloss - base_ll, f)
                })
                .collect();
            gains.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let top: Vec<String> = gains.iter().take(14).map(|(g, f)| format!("{}:{:.4}", NAMES[*f], g)).collect();
            println!("  worth h={h}: {}", top.join(" "));
        }
        heads.push(head.json(h));
    }
    if let Some(out) = out {
        let mut model: Value =
            serde_json::from_str(&std::fs::read_to_string(&base).expect("base model")).expect("base model json");
        model["need"] = Value::Array(heads);
        model["need_names"] = json!(NAMES.to_vec());
        model["need_trained_on"] = json!({"rows": dev.len(), "sessions": test.iter().filter(|&&t| !t).count(), "horizons": horizons, "epochs": epochs, "l2": l2, "bins": bins, "dropped": dropped.iter().map(|&f| NAMES[f]).collect::<Vec<_>>()});
        let mut f = std::fs::File::create(&out).expect("model out");
        f.write_all(serde_json::to_string(&model).expect("json").as_bytes()).expect("write");
        eprintln!("wrote {out}");
    }
}
