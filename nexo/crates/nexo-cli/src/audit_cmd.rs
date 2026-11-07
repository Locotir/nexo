//! `nexo audit report`: sums the local audit log (see docs/AUDIT.md) into a table.

use std::path::PathBuf;

use clap::Subcommand;
use nexo_agent::audit::{files, report, since_stamp, By, Row};
use nexo_core::Config;

#[derive(Subcommand, Debug)]
pub enum AuditCmd {
    /// Sum the cost and the calls of the audit log.
    Report {
        /// Only lines newer than this: 7d, 12h, 30m, 2w or a date like 2026-11-01.
        #[arg(long, value_name = "SPAN")]
        since: Option<String>,
        /// Group by user, model or day.
        #[arg(long, default_value = "user", value_parser = ["user", "model", "day"])]
        by: String,
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Read this log file instead of the configured one.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
}

pub fn run(config: &Config, what: &AuditCmd) -> anyhow::Result<i32> {
    let AuditCmd::Report { since, by, json, file } = what;
    let by = By::parse(by).ok_or_else(|| anyhow::anyhow!("--by is user, model or day"))?;
    let since = since
        .as_deref()
        .map(|s| since_stamp(s, time::OffsetDateTime::now_utc()))
        .transpose()
        .map_err(|e| anyhow::anyhow!(e))?;
    let logs = match file {
        Some(f) => vec![f.clone()],
        None => files(&config.audit),
    };
    if logs.is_empty() && !*json {
        println!("no audit log yet; turn it on with [audit] enabled = true (docs/AUDIT.md)");
    }
    let rows = report(&logs, since.as_deref(), by);
    if *json {
        let total = total(&rows);
        let out = serde_json::json!({
            "rows": rows.iter().map(row_json).collect::<Vec<_>>(),
            "total": row_json(&total),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else if !rows.is_empty() {
        print_table(&rows, by);
    } else if !logs.is_empty() {
        println!("no lines in the log for that span");
    }
    Ok(0)
}

fn total(rows: &[Row]) -> Row {
    let mut t = Row { key: "total".into(), ..Row::default() };
    for r in rows {
        t.calls += r.calls;
        t.requests += r.requests;
        t.tokens_in += r.tokens_in;
        t.tokens_out += r.tokens_out;
        t.tokens_cached += r.tokens_cached;
        t.cost_usd += r.cost_usd;
    }
    t
}

fn row_json(r: &Row) -> serde_json::Value {
    serde_json::json!({
        "key": r.key,
        "tool_calls": r.calls,
        "requests": r.requests,
        "tokens_in": r.tokens_in,
        "tokens_out": r.tokens_out,
        "tokens_cached": r.tokens_cached,
        "cost_usd": r.cost_usd,
    })
}

fn print_table(rows: &[Row], by: By) {
    let head = match by {
        By::User => "user",
        By::Model => "model",
        By::Day => "day",
    };
    let width = rows.iter().map(|r| r.key.chars().count()).chain([head.len(), 5]).max().unwrap_or(5);
    println!(
        "{head:<width$}  {:>6}  {:>8}  {:>10}  {:>10}  {:>10}  {:>10}",
        "calls", "requests", "in", "out", "cached", "cost"
    );
    let all = total(rows);
    for r in rows.iter().chain(std::iter::once(&all)) {
        println!(
            "{:<width$}  {:>6}  {:>8}  {:>10}  {:>10}  {:>10}  {:>10}",
            r.key,
            r.calls,
            r.requests,
            r.tokens_in,
            r.tokens_out,
            r.tokens_cached,
            format!("${:.4}", r.cost_usd)
        );
    }
}
