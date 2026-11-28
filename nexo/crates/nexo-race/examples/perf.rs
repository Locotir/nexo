//! Wall clock of the curator on a large coding history, when a person asks for it (nothing here runs by default):
//! `cargo run --release -p nexo-race --example perf -- --sizes 1000,10000,50000 --out-kb 16 [--window 128000]
//! [--views 20] [--edit-pct 25] [--files 40] [--seed 1] [--spec SPEC]`.
//!
//! For each size (messages) it prints three lines:
//! - `cold`: ms of one view over the whole history, as a rewrite, a compaction or a failed restore asks for, and the RSS;
//! - `steady`: median and p99 ms over `--views` views, each over the history plus two new messages (the cost of
//!   each model call);
//! - `restore`: ms to hash the history and save the state, to read it back, and the first view after.
//!
//! Tool outputs run from 2 KB to `--out-kb` (16 by default: 50,000 messages are about 400 MB of JSON). With
//! `--out-kb 64` it stresses `page` and the token estimate. The history comes from `tests/support/perf_gen.rs`, the
//! same one `engine::tests::work` counts the work of. The counted work (`cargo test -p nexo-race work`) tells
//! where the growth is; this tells what it costs.

#[path = "../tests/support/perf_gen.rs"]
mod perf_gen;

use std::time::Instant;

use nexo_race::snapshot::{history_hash, message_hash};
use nexo_race::{Race, D_SPEC};
use perf_gen::{Session, Shape};

/// The resident memory of this process in MB (0 where `/proc` has none).
fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb =
        s.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok());
    kb.unwrap_or(0.0) / 1024.0
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let num = |name: &str, default: usize| {
        get(name).map_or(default, |v| v.parse().unwrap_or_else(|_| panic!("{name}: a number")))
    };
    let sizes: Vec<usize> = get("--sizes")
        .unwrap_or_else(|| "1000,10000,50000".into())
        .split(',')
        .map(|s| s.trim().parse().expect("--sizes: numbers"))
        .collect();
    let shape = Shape {
        seed: num("--seed", 1) as u64,
        out_kb: num("--out-kb", 16),
        edit_pct: num("--edit-pct", 25),
        files: num("--files", 40),
        ..Shape::default()
    };
    let (window, views) = (num("--window", 0) as u64, num("--views", 20).max(1));
    let spec = get("--spec").unwrap_or_else(|| D_SPEC.to_string());
    let new_curator = || {
        let mut c = Race::new(&spec).expect("--spec");
        c.window(window);
        c
    };
    println!("size | out KB | window | what | ms | rss MB");
    for n in sizes {
        let mut session = Session::new(shape.clone());
        let mut msgs = Vec::new();
        session.extend(&mut msgs, n);
        let bytes: usize = msgs.iter().map(|m| m.to_string().len()).sum();
        let head = format!("{} | {} | {window}", msgs.len(), shape.out_kb);
        eprintln!("{} messages, {:.0} MB of JSON", msgs.len(), bytes as f64 / 1e6);

        let mut cur = new_curator();
        let t = Instant::now();
        cur.view(0, &msgs, 0.0).expect("view");
        println!("{head} | cold | {:.1} | {:.0}", ms(t), rss_mb());

        let mut times = Vec::new();
        for k in 1..=views {
            msgs.extend(session.step());
            let t = Instant::now();
            cur.view(0, &msgs, k as f64 * 10.0).expect("view");
            times.push(ms(t));
        }
        times.sort_by(f64::total_cmp);
        let p99 = times[(times.len() * 99 / 100).min(times.len() - 1)];
        println!("{head} | steady median/p99 | {:.2} / {p99:.2} | {:.0}", times[times.len() / 2], rss_mb());

        let t = Instant::now();
        let hashes: Vec<u64> = msgs.iter().map(message_hash).collect();
        let saved = cur.save(views as u64, history_hash(&hashes[..cur.covered()])).encode();
        let save = ms(t);
        let t = Instant::now();
        let restored = Race::restore(&spec, &saved, &msgs, &hashes);
        let read = ms(t);
        match restored {
            Ok((mut back, _)) => {
                back.window(window);
                msgs.extend(session.step());
                let t = Instant::now();
                back.view(0, &msgs, (views + 1) as f64 * 10.0).expect("view");
                println!("{head} | restore save/read/view | {save:.1} / {read:.1} / {:.1} | {:.0}", ms(t), rss_mb());
            }
            Err(e) => println!("{head} | restore | failed: {e}"),
        }
    }
}
