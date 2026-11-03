//! Honest scale bench: does the curator's time per request stay flat as the history grows, on more than one
//! shape of session? `cargo run --release -p nexo-curator --example scale -- [--gens e8,a,b,c]
//! [--cps 1000,5000,20000] [--tail 100] [--stride 10] [--dense-until 5000] [--no-ingest]`.
//! `--dump FILE N` writes the `e8` session of N requests as JSON lines for `examples/bench.rs` and stops.
//! Requests are fed one by one as the host does (a call per user message); past `--dense-until` only every
//! `--stride`-th request calls the curator, except the `--tail` requests before each checkpoint, which are timed.
//! Columns: `curate` is `Curator::view`; `+reply` adds serializing the memory reply (O(n) bytes by protocol);
//! `ingest` is one call with the whole history, per 1,000 messages.

#[path = "../tests/support/scale_gen.rs"]
mod scale_gen;

use scale_gen::{run, Gen, Plan, KINDS};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let (path, n) = (&args[i + 1], args[i + 2].parse::<usize>().expect("N"));
        let mut g = Gen::new("e8");
        while g.requests < n {
            g.advance();
        }
        let text: String = g.msgs.iter().map(|m| m.to_string() + "\n").collect();
        std::fs::write(path, text).expect("write");
        return;
    }
    let nums = |s: String| s.split(',').filter_map(|x| x.parse::<usize>().ok()).collect::<Vec<_>>();
    let gens: Vec<String> = get("--gens")
        .map_or_else(|| KINDS.iter().map(|s| s.to_string()).collect(), |s| s.split(',').map(str::to_string).collect());
    let plan = Plan {
        checkpoints: get("--cps").map_or(vec![1000, 5000, 20000], nums),
        tail: get("--tail").and_then(|s| s.parse().ok()).unwrap_or(100),
        stride: get("--stride").and_then(|s| s.parse().ok()).unwrap_or(10),
        dense_until: get("--dense-until").and_then(|s| s.parse().ok()).unwrap_or(5000),
        ingest: !args.iter().any(|a| a == "--no-ingest"),
    };
    println!("| gen | requests | messages | curate ms median | curate ms p99 | +reply ms median | reply KB | RSS MB | ingest ms/1k msgs | cpu ms/request | view |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    for k in &gens {
        for r in run(k, &plan) {
            println!(
                "| {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.0} | {:.0} | {:.1} | {:.2} | {} |",
                r.kind,
                r.requests,
                r.messages,
                r.median_ms,
                r.p99_ms,
                r.with_reply_ms,
                r.reply_kb,
                r.rss_mb,
                r.ingest_ms_per_1k,
                r.cpu_ms,
                r.view
            );
        }
    }
}
