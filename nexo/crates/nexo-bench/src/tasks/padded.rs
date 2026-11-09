//! Padded revert tasks in three styles (code repo, data project, chat without tools).
//!
//! Like `selective-revert`, the last request cancels the first one. Unlike it, the earlier requests
//! carry pasted material, so the earlier topics weigh more than 8,000 tokens and the curator really
//! leaves them out of the view (below that, nothing is hidden). The material is generated, so it is
//! the same bytes in every run. The scoring is deterministic and needs no model.
//!
//! The first two styles are ordinary `Task`s. The chat has no working directory and is scored on
//! the answers, so it is a `ChatTask` run by [`run_chat`].

use std::path::Path;

use super::{always, files, manifest, Task};
use crate::arm::Arm;
use crate::check::{read, Copy, Outcome, Part};
use crate::run::{self, Bench, Plan, Record};

/// The two styles that work on files.
pub fn tasks() -> Vec<Task> {
    vec![code_task(), data_task()]
}

// ------------------------------------------------------------------ generated material

/// A pick from `list` that depends on `n` and `salt` only.
fn pick<'a>(list: &[&'a str], n: usize, salt: usize) -> &'a str {
    list[(n * 7 + salt * 13 + n / 5) % list.len()]
}

/// A support thread of `lines` lines: dated, signed, about nothing the request needs.
pub(crate) fn thread(salt: usize, lines: usize) -> String {
    const WHO: [&str; 6] = ["mara", "joel", "ines", "tomas", "pilar", "ruben"];
    const SAYS: [&str; 10] = [
        "checked the staging copy, the numbers look the same as last week",
        "the customer wrote again, they still see the old value on the invoice page",
        "can someone confirm which warehouse ships the heavy parcels now",
        "I replayed the nightly export and nothing changed on my side",
        "finance asked for the exact rounding rule, I sent them the spreadsheet",
        "support closed two duplicates of this, linking them here for the record",
        "the carrier changed its contact person, the old mailbox bounces",
        "parking this until the weekly sync, nobody is blocked by it",
        "I added a note to the wiki page, please read it before touching the settings",
        "the regional office wants the same report with the totals split by month",
    ];
    // other vocabulary per salt, so the pasted notes of different requests are not one topic
    const AUDIT: [&str; 6] = [
        "interview with the packaging supervisor lasted forty minutes, notes attached",
        "sampled twelve pallets at the dock, labels matched the manifest",
        "the cold room thermometer was recalibrated before our visit",
        "training records for night staff are incomplete, owner informed",
        "fire exits were clear and signage is current in building two",
        "forklift inspection stickers expire next month, reminder sent",
    ];
    const LOAD: [&str; 6] = [
        "batch accepted, zero rows rejected by the loader",
        "partition rebuilt after a timeout on the staging cluster",
        "checksum of the incoming file matched the sender value",
        "schema drift warning: an extra empty column was ignored",
        "retry number two succeeded, queue depth back to normal",
        "vacuum finished, table size down after compaction",
    ];
    let says: &[&str] = match salt % 3 {
        1 => &SAYS,
        2 => &AUDIT,
        _ => &LOAD,
    };
    (0..lines)
        .map(|i| {
            format!(
                "[2026-02-{:02} {:02}:{:02}] {}: {} (ref {}-{})\n",
                1 + (i + salt) % 27,
                8 + i % 10,
                (i * 17 + salt * 5) % 60,
                pick(&WHO, i, salt),
                pick(says, i, salt),
                salt,
                1000 + i * 3
            )
        })
        .collect()
}

/// A carrier's rate card of `zones` zones: `(zone, base, per kg)`.
fn rates(salt: usize, zones: usize) -> Vec<(String, f64, f64)> {
    (1..=zones)
        .map(|n| {
            let base = 3.0 + ((n * 7 + salt) % 13) as f64 * 0.25;
            let per = 0.9 + ((n * 5 + salt * 3) % 9) as f64 * 0.05;
            (format!("Z{n:03}"), base, per)
        })
        .collect()
}

fn card_text(carrier: &str, salt: usize, zones: usize) -> String {
    let mut out = format!("Rate card of {carrier}, euro per parcel. Cost = base + per_kg * kg.\n");
    for (z, b, p) in rates(salt, zones) {
        out += &format!("{z} | base {b:.2} | per_kg {p:.2}\n");
    }
    out
}

/// The cost of `kg` kilos to `zone` on a card, rounded to cents.
fn cost(salt: usize, zones: usize, zone: &str, kg: f64) -> String {
    let (_, b, p) = rates(salt, zones).into_iter().find(|(z, _, _)| z == zone).expect("a zone of the card");
    format!("{:.2}", crate::check::round_to(b + p * kg, 2))
}

/// CSV rows of orders for the data style: `id,customer,region,qty,price,discount,ordered_at`.
fn order_rows(from: usize, n: usize) -> String {
    const REGION: [&str; 4] = ["north", "south", "east", "west"];
    (from..from + n)
        .map(|i| {
            format!(
                "{},c{:03},{},{},{:.2},{:.2},2026-{:02}-{:02}\n",
                i,
                (i * 37) % 120,
                pick(&REGION, i, 1),
                1 + (i * 3) % 9,
                5.0 + ((i * 11) % 40) as f64 * 0.75,
                ((i * 5) % 4) as f64 * 0.05,
                1 + (i / 28) % 12,
                1 + i % 28
            )
        })
        .collect()
}

// ------------------------------------------------------------------ (i) code repo

pub(crate) const LIB: &str = r##"mod shipping;
mod tax;

pub use shipping::shipping;
pub use tax::tax;

/// `x` rounded to cents.
pub(crate) fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
"##;

pub(crate) const SHIPPING: &str = r##"use crate::round2;

/// Shipping cost of a parcel.
pub fn shipping(weight_kg: f64) -> f64 {
    round2(4.5 + 1.2 * weight_kg)
}
"##;

pub(crate) const TAX: &str = r##"use crate::round2;

/// Tax on an amount.
pub fn tax(amount: f64) -> f64 {
    round2(amount * 0.18)
}
"##;

const INVOICE: &str = r##"use crate::{round2, tax};

/// Subtotal, tax and total of some lines.
pub struct Invoice {
    pub subtotal: f64,
    pub tax: f64,
    pub total: f64,
}

/// The invoice of (name, qty, unit price) lines.
pub fn invoice(lines: &[(&str, u32, f64)]) -> Invoice {
    let subtotal = round2(lines.iter().map(|(_, q, p)| *q as f64 * p).sum());
    let t = tax(subtotal);
    Invoice { subtotal, tax: t, total: round2(subtotal + t) }
}
"##;

/// The turns of the code style; the early ones carry pasted context. Each early request works in
/// its own file, as in a real repo, so the curator sees three topics.
pub(crate) fn code_turns() -> Vec<(u32, String)> {
    vec![
        (
            1,
            format!(
                "In src/shipping.rs, change shipping(): parcels heavier than 20 kg now pay a flat 15.0 surcharge on \
                 top of the current formula, and the cost is never below 6.0.\n\nThe ticket thread, for context \
                 only:\n{}",
                thread(1, 60)
            ),
        ),
        (
            1,
            format!(
                "In src/tax.rs, the tax rate is now 21% instead of 18%.\n\nThe audit notes, for context only:\n{}",
                thread(2, 240)
            ),
        ),
        (
            1,
            format!(
                "Add invoice(lines) in a new src/invoice.rs (declare the module in src/lib.rs and re-export it with \
                 `pub use invoice::{{invoice, Invoice}};`): lines is a slice of (name, qty, unit_price) tuples, \
                 `&[(&str, u32, f64)]`; return an Invoice struct with the public f64 fields subtotal (rounded to 2 \
                 decimals), tax (tax() of the subtotal) and total (rounded to 2 decimals). Write a small test for \
                 it in tests/shop.rs and make sure `cargo test` passes.\n\nSome orders from last month, for context \
                 only:\n{}",
                order_rows(1, 320)
            ),
        ),
        (
            1,
            "Management cancelled the change from my first request in this conversation. Undo exactly that change \
             and nothing else. There is no git here, so don't look for it."
                .into(),
        ),
    ]
}

fn code_task() -> Task {
    Task {
        id: "padded-revert-code",
        trap: "selective-revert across three files with 8k+ tokens of pasted context, so the curator hides the first topic",
        group: "padded",
        files: files(&[
            ("Cargo.toml", &manifest("shop", "")),
            ("src/lib.rs", LIB),
            ("src/shipping.rs", SHIPPING),
            ("src/tax.rs", TAX),
        ]),
        turns: code_turns(),
        check: |wd| Outcome::share(code_parts(wd)),
        solution: |wd| {
            let put = |rel: &str, text: &str| std::fs::write(wd.join(rel), text).expect("a file");
            put("src/lib.rs", &LIB.replace("mod tax;\n", "mod invoice;\nmod tax;\n").replace("pub use tax::tax;\n", "pub use invoice::{invoice, Invoice};\npub use tax::tax;\n"));
            put("src/tax.rs", &TAX.replace("0.18", "0.21"));
            put("src/invoice.rs", INVOICE);
        },
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

fn code_probe() -> String {
    let ship: Vec<String> = [1.0f64, 10.0, 20.0, 21.0, 35.0]
        .iter()
        .map(|w| format!("({w:?}, {:?})", crate::check::round_to(4.5 + 1.2 * w, 2)))
        .collect();
    format!(
        r##"fn near(a: f64, b: f64) -> bool {{
    (a - b).abs() < 1e-9
}}

#[test]
fn hidden_shipping() {{
    for (w, want) in [{}] {{
        assert!(near(shop::shipping(w), want), "shipping({{w}}) = {{}}, want {{want}}", shop::shipping(w));
    }}
}}

#[test]
fn hidden_tax() {{
    assert!(near(shop::tax(100.0), 21.0), "tax(100) = {{}}", shop::tax(100.0));
}}

#[test]
fn hidden_invoice() {{
    let inv = shop::invoice(&[("a", 2, 10.0), ("b", 1, 6.0)]);
    assert!(near(inv.subtotal, 26.0) && near(inv.tax, 5.46) && near(inv.total, 31.46),
        "invoice: {{}} {{}} {{}}", inv.subtotal, inv.tax, inv.total);
}}
"##,
        ship.join(", ")
    )
}

fn code_parts(wd: &Path) -> Vec<Part> {
    if !wd.is_dir() {
        return vec![(false, "the working directory is gone".into())];
    }
    match Copy::of(wd) {
        Ok(c) => c.hidden(
            "zz_hidden_padded_shop",
            &code_probe(),
            &[
                ("hidden_shipping", "shipping() is not back to the original"),
                ("hidden_tax", "tax(100) is not 21.0 (the second change was undone too)"),
                ("hidden_invoice", "invoice() is wrong"),
            ],
        ),
        Err(e) => vec![(false, format!("cannot copy the working directory: {e}"))],
    }
}

// ------------------------------------------------------------------ (ii) data project

const TRANSFORM: &str = "-- Monthly revenue per region\n\
SELECT\n  region,\n  strftime('%Y-%m', ordered_at) AS month,\n  SUM(qty * price) AS revenue\n\
FROM orders\nGROUP BY region, month\nORDER BY month, region;\n";

const CSV_HEAD: &str = "id,customer,region,qty,price,discount,ordered_at\n";

fn data_orders() -> String {
    format!("{CSV_HEAD}{}", order_rows(1, 24))
}

/// The turns of the data style.
pub(crate) fn data_turns() -> Vec<(u32, String)> {
    vec![
        (
            1,
            format!(
                "In sql/transform.sql, revenue must now be net of the discount column: use SUM(qty * price * \
                 (1 - discount)) AS revenue instead of SUM(qty * price).\n\nA sample of the nightly export, for \
                 context only:\n{}",
                order_rows(100, 60)
            ),
        ),
        (
            1,
            format!(
                "Add a column channel to data/orders.csv, value `web` in every row (header included as `channel`), \
                 and add channel to the SELECT and to the GROUP BY of sql/transform.sql.\n\nThe load log, for \
                 context only:\n{}",
                thread(3, 240)
            ),
        ),
        (
            1,
            format!(
                "Create sql/top_customers.sql: the 10 customers with the most orders (customer, COUNT(*) AS \
                 orders), most first, with LIMIT 10.\n\nMore of the export, for context only:\n{}",
                order_rows(300, 320)
            ),
        ),
        (1, "Finance dropped the idea. Undo the change from my first request and nothing else.".into()),
    ]
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `sql` (squashed) selects `channel` and groups by region, month and channel, in any order.
fn channel_kept(sql: &str) -> bool {
    let up = sql.to_uppercase();
    let list = |from: &str, to: &[&str]| -> Vec<String> {
        let Some(start) = up.find(from).map(|i| i + from.len()) else { return vec![] };
        let end = to.iter().filter_map(|t| up[start..].find(t)).min().map_or(up.len(), |e| start + e);
        up[start..end].split(',').map(|s| s.trim().to_string()).collect()
    };
    let select = list("SELECT ", &[" FROM "]);
    let mut group = list("GROUP BY ", &[" ORDER BY", " HAVING", " LIMIT", ";"]);
    group.sort();
    select.iter().any(|s| s == "CHANNEL") && group == ["CHANNEL", "MONTH", "REGION"]
}

fn data_parts(wd: &Path) -> Vec<Part> {
    let sql = squash(&read(wd, "sql/transform.sql"));
    let csv = read(wd, "data/orders.csv");
    let top = squash(&read(wd, "sql/top_customers.sql"));
    let mut lines = csv.lines();
    let head = lines.next().unwrap_or("");
    let rows: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
    vec![
        (sql.contains("SUM(qty * price) AS revenue"), "revenue is not the original SUM(qty * price) again".into()),
        (!sql.contains("discount"), "transform.sql still mentions discount".into()),
        (channel_kept(&sql), "channel left transform.sql (the second change was undone too)".into()),
        (
            head == "id,customer,region,qty,price,discount,ordered_at,channel"
                && rows.len() == 24
                && rows.iter().all(|r| r.ends_with(",web")),
            "data/orders.csv lost the channel column".into(),
        ),
        (
            top.contains("COUNT(*)") && top.contains("GROUP BY customer") && top.contains("LIMIT 10"),
            "sql/top_customers.sql is missing or wrong".into(),
        ),
    ]
}

fn data_solution(wd: &Path) {
    let sql = TRANSFORM
        .replace("  region,\n", "  region,\n  channel,\n")
        .replace("GROUP BY region, month", "GROUP BY region, month, channel");
    let csv: String = data_orders()
        .lines()
        .enumerate()
        .map(|(i, l)| if i == 0 { format!("{l},channel\n") } else { format!("{l},web\n") })
        .collect();
    for (rel, text) in [
        ("sql/transform.sql", sql.as_str()),
        ("data/orders.csv", csv.as_str()),
        (
            "sql/top_customers.sql",
            "SELECT customer, COUNT(*) AS orders\nFROM orders\nGROUP BY customer\nORDER BY orders DESC\nLIMIT 10;\n",
        ),
    ] {
        let p = wd.join(rel);
        std::fs::create_dir_all(p.parent().expect("a parent")).expect("a folder");
        std::fs::write(p, text).expect("a file");
    }
}

fn data_task() -> Task {
    Task {
        id: "padded-revert-data",
        trap:
            "undo the first of three edits to a SQL transform and a CSV, with 8k+ tokens of pasted exports in between",
        group: "padded",
        files: files(&[("data/orders.csv", &data_orders()), ("sql/transform.sql", TRANSFORM)]),
        turns: data_turns(),
        check: |wd| Outcome::share(data_parts(wd)),
        solution: data_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

// ------------------------------------------------------------------ (iii) chat without tools

/// A conversation with no tools and no files, scored on what the model says.
pub struct ChatTask {
    pub id: &'static str,
    pub trap: &'static str,
    /// `(session, user message)`.
    pub turns: Vec<(u32, String)>,
    /// Grades the last assistant text of each turn.
    pub check: fn(&[String]) -> Outcome,
    /// A set of answers that scores 1.
    pub reference: fn() -> Vec<String>,
}

const CHAT_ZONES: usize = 400;
const CHAT_CODES: usize = 600;
const CHAT_SKUS: usize = 450;

/// A 7 kg parcel to Z042 on the carrier's rate card.
fn first_total() -> String {
    cost(1, CHAT_ZONES, "Z042", 7.0)
}

/// Units of currency code C233 for 380 euro.
fn money_text() -> (String, String) {
    let mut table = String::from("Exchange table. Units of each currency for 1 euro.\n");
    let mut want = 0.0;
    for n in 1..=CHAT_CODES {
        let r = 0.35 + ((n * 37) % 997) as f64 / 41.0;
        table += &format!("C{n:03} | {r:.4}\n");
        if n == 233 {
            want = r;
        }
    }
    (table, format!("{:.2}", crate::check::round_to(380.0 * want, 2)))
}

/// Total stock of SKU-0123 in the two warehouses.
fn stock_text() -> (String, String) {
    let mut table = String::from("Stock by warehouse.\n");
    let mut want = 0;
    for n in 1..=CHAT_SKUS {
        let (a, b) = (1100 + (n * 53) % 4000, 1300 + (n * 71) % 3000);
        table += &format!("SKU-{n:04} | north {a} | south {b}\n");
        if n == 123 {
            want = a + b;
        }
    }
    (table, want.to_string())
}

/// The answers of the two later topics.
fn later_totals() -> [String; 2] {
    [money_text().1, stock_text().1]
}

/// The chat tasks, which run only when named: they are no `Task` (no files) and no group has them.
pub fn chat_tasks() -> Vec<ChatTask> {
    vec![chat_task()]
}

pub fn chat_task() -> ChatTask {
    ChatTask {
        id: "padded-revert-chat",
        trap: "no tools: go back to the answer of the first question after two long topics of pasted tables",
        turns: vec![
            (1, "Quick session: I will ask a few unrelated questions, one number each. No need to explain.".into()),
            (
                1,
                format!(
                    "Here is the rate card of carrier Norvik.\n\n{}\nWhat does a 7 kg parcel to zone Z042 cost with \
                     Norvik? Give the total in euro with two decimals.",
                    card_text("Norvik", 1, CHAT_ZONES)
                ),
            ),
            (
                1,
                format!(
                    "Different matter, I am planning a trip abroad.\n\n{}\nHow many units of currency C233 do I get \
                     for 380 euro? Two decimals.",
                    money_text().0
                ),
            ),
            (
                1,
                format!(
                    "Now the warehouse count for the quarterly review.\n\n{}\nHow many units of SKU-0123 are in \
                     stock across both warehouses?",
                    stock_text().0
                ),
            ),
            (
                1,
                "Go back to the first of those questions. Give me that answer again, only the number \
                 with two decimals."
                    .into(),
            ),
        ],
        check: chat_check,
        reference: || {
            let [b, c] = later_totals();
            vec!["Go ahead.".into(), first_total(), b, c, first_total()]
        },
    }
}

fn chat_check(answers: &[String]) -> Outcome {
    let last = answers.last().map(String::as_str).unwrap_or("");
    if last.trim().is_empty() {
        return Outcome::fail("no answer to the last question");
    }
    let [b, c] = later_totals();
    Outcome::share(vec![
        (last.contains(&first_total()), format!("the last answer does not give {}", first_total())),
        (!last.contains(&b) && !last.contains(&c), "the last answer gives the number of another topic".into()),
        (last.trim().len() <= 40, "the last answer is not just the number".into()),
    ])
}

/// Runs a chat once with `arm`: no tools offered, graded on the answers. Err when it cannot run.
pub async fn run_chat(bench: &Bench, task: &ChatTask, arm: &Arm, rep: u32, budget_left: f64) -> Result<Record, String> {
    let dir = tempfile::Builder::new().prefix("nexo-bench-chat-").tempdir().map_err(|e| e.to_string())?;
    let wd = dir.path().canonicalize().map_err(|e| e.to_string())?;
    let plan = Plan {
        name: task.id.into(),
        wd,
        seal: None,
        tools: Some(Vec::new()),
        confine: true,
        system_prompt: None,
        turns: task.turns.clone(),
        max_steps: bench.max_steps,
        followup: None,
    };
    let driven = run::drive(bench, &plan, arm, rep, budget_left).await?;
    let out = (task.check)(&driven.answers);
    Ok(run::record(bench, arm, rep, task.id, &driven, out.score, out.ok, out.detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_may_sit_anywhere_in_the_select_and_group_by() {
        let old = |s: &str| s.contains("channel,") && s.contains("GROUP BY region, month, channel");
        let last = "SELECT region, strftime('%Y-%m', ordered_at) AS month, SUM(qty * price) AS revenue, channel \
                    FROM orders GROUP BY region, channel, month ORDER BY month, region;";
        assert!(!old(last) && channel_kept(last));
        let usual = squash(
            &TRANSFORM
                .replace("  region,\n", "  region,\n  channel,\n")
                .replace("GROUP BY region, month", "GROUP BY region, month, channel"),
        );
        assert!(channel_kept(&usual));
        assert!(!channel_kept(&squash(TRANSFORM)));
        assert!(!channel_kept("SELECT region, month, channel FROM orders GROUP BY region, month;"));
    }
}
