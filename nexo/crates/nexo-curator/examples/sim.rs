//! The session simulator: `sim events <input>... --spec S [--price live|legacy] [--life S] [--slots K] [--json]`.
//!
//! Each input is a golden fixture or a `nexo bench` transcript (told apart by the first line). Every request
//! goes through a curator with the spec; the events are the places where the view lacks content the recorded
//! trajectory depends on (`cited`, `denied`, `blind`, see `nexo_curator::sim::events`), and the cost is the
//! replay's: a prefix cache with one slot (`--life S` keeps every request of the last S seconds, `--slots K`
//! the last K) priced with `--price` (`live`: deepseek-flash 0.3/0.006/1.2 $ per M, `legacy`: the replay's
//! 0.28/0.028, input only).

use nexo_curator::sim::events::{run, Run};
use nexo_curator::sim::input::load;
use nexo_curator::sim::price::Prices;
use serde_json::{json, Value};

fn usage() -> ! {
    eprintln!("usage: sim events INPUT... --spec SPEC [--price live|legacy] [--life S] [--slots K] [--json]");
    std::process::exit(2);
}

fn count(r: &Run, kind: &str) -> usize {
    r.events.iter().filter(|e| e.kind.name() == kind).count()
}

fn row(path: &str, r: &Run) -> Value {
    json!({
        "input": path, "requests": r.requests,
        "events": { "cited": count(r, "cited"), "denied": count(r, "denied"), "blind": count(r, "blind") },
        "forgotten": r.forgotten, "replies": r.replies, "copied": r.copied,
        "miss": r.miss, "hit": r.hit, "out": r.out,
        "blind_tokens": [r.blind.0, r.blind.1], "denied_tokens": [r.denied.0, r.denied.1],
        "input_cost": r.input_cost, "cost": r.cost,
        "list": r.events.iter().map(|e| json!({
            "t": e.t, "mi": e.mi, "kind": e.kind.name(), "s_e": e.s_e, "tool": e.tool.name(),
            "s_rest": e.s_rest, "words": e.words,
        })).collect::<Vec<_>>(),
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("events") {
        usage();
    }
    let (mut inputs, mut spec, mut price, mut life, mut slots, mut json_out) =
        (Vec::new(), String::new(), "live".to_string(), 0.0f64, 0usize, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--spec" => spec = args.next().unwrap_or_else(|| usage()),
            "--price" => price = args.next().unwrap_or_else(|| usage()),
            "--life" => life = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--slots" => slots = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--json" => json_out = true,
            _ if a.starts_with("--") => usage(),
            _ => inputs.push(a),
        }
    }
    let Some(prices) = Prices::named(&price) else { usage() };
    if inputs.is_empty() {
        usage();
    }
    let mut pooled = Run::default();
    for path in &inputs {
        let session = load(path).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
        let r = run(&session.requests, &spec, prices, life, slots).unwrap_or_else(|e| {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        });
        if json_out {
            println!("{}", row(path, &r));
        } else {
            println!(
                "{path}: {} requests, events {} (cited {}, denied {}, blind {}), forgotten {} words in {} replies, miss {} hit {}, cost {:.6} $",
                r.requests,
                r.events.len(),
                count(&r, "cited"),
                count(&r, "denied"),
                count(&r, "blind"),
                r.forgotten,
                r.replies,
                r.miss,
                r.hit,
                r.cost
            );
        }
        pooled.requests += r.requests;
        pooled.replies += r.replies;
        pooled.forgotten += r.forgotten;
        pooled.copied += r.copied;
        pooled.miss += r.miss;
        pooled.hit += r.hit;
        pooled.out += r.out;
        pooled.blind = (pooled.blind.0 + r.blind.0, pooled.blind.1 + r.blind.1);
        pooled.denied = (pooled.denied.0 + r.denied.0, pooled.denied.1 + r.denied.1);
        pooled.input_cost += r.input_cost;
        pooled.cost += r.cost;
        pooled.events.extend(r.events);
    }
    if json_out {
        println!("{}", row("total", &pooled));
    } else {
        println!(
            "total: {} requests, events {} (cited {}, denied {}, blind {}), forgotten {} words, cost {:.6} $ ({:.6} $ input)",
            pooled.requests,
            pooled.events.len(),
            count(&pooled, "cited"),
            count(&pooled, "denied"),
            count(&pooled, "blind"),
            pooled.forgotten,
            pooled.cost,
            pooled.input_cost
        );
    }
}
