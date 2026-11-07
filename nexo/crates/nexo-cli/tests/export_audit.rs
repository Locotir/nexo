//! `nexo export` and the audit log, run through the binary against the mock provider.

use std::path::Path;
use std::process::{Command, Output};

const MOCK: &str =
    "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\"]\n[context]\npolicy = \"plain\"\n";

fn nexo(dir: &Path, config: &str, args: &[&str]) -> Output {
    std::fs::write(dir.join("config.toml"), config).unwrap();
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(args)
        .current_dir(dir)
        .env("NEXO_CONFIG_DIR", dir)
        .env("NEXO_DATA_DIR", dir.join("data"))
        .env("NEXO_CACHE_DIR", dir.join("cache"))
        .env("USER", "tester")
        .output()
        .expect("nexo runs")
}

fn ok(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// A fake key built at run time so no file holds a literal one.
fn fake_key() -> String {
    ["s", "k-abcdefghijklmnopqrstuvwx1234"].concat()
}

fn audit_config(log: &Path) -> String {
    format!("{MOCK}[audit]\nenabled = true\npath = '{}'\n", log.display())
}

fn lines(log: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect()
}

#[test]
fn html_export_is_self_contained_escaped_and_redacted() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "hello file\n").unwrap();
    std::fs::write(d.path().join(".env"), "TOKEN_VALUE=hunter2hunter2\n").unwrap();
    let key = fake_key();
    ok(&nexo(d.path(), MOCK, &["-p", &format!("look <script>alert(1)</script> {key} **bold**")]));
    ok(&nexo(d.path(), MOCK, &["--yolo", "-p", "tool:read {\"path\":\".env\"}"]));
    // the latest session reads a file
    ok(&nexo(d.path(), MOCK, &["-p", "tool:read {\"path\":\"a.txt\"}"]));
    let out = d.path().join("last.html");
    ok(&nexo(d.path(), MOCK, &["export", "--last", "--html", "-o", out.to_str().unwrap()]));
    let html = std::fs::read_to_string(&out).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(!html.contains("<script"), "a script tag in the file");
    assert!(!html.contains("src=") && !html.contains("<link") && !html.contains("@import") && !html.contains("url("));
    assert!(html.contains("prefers-color-scheme:dark") && html.contains("color-scheme"));
    assert!(html.contains("<details class=\"tool\""), "tool calls are collapsible");
    assert!(html.contains("hello file"), "the result is in the file");
    assert!(html.contains("cost") && html.contains("turns"));

    // the first session holds the script text and the key: find it by id
    let list = ok(&nexo(d.path(), MOCK, &["sessions"]));
    let ids: Vec<&str> = list.lines().skip(1).filter_map(|l| l.split_whitespace().next()).collect();
    assert_eq!(ids.len(), 3, "{list}");
    let mut seen_script = false;
    let mut seen_env = false;
    for id in ids {
        let o = d.path().join(format!("{id}.html"));
        ok(&nexo(d.path(), MOCK, &["export", id, "-o", o.to_str().unwrap()]));
        let h = std::fs::read_to_string(&o).unwrap();
        assert!(!h.contains("<script"), "{id}");
        assert!(!h.contains(&key), "key survived in {id}");
        assert!(!h.contains("hunter2hunter2"), ".env content survived in {id}");
        seen_script |= h.contains("&lt;script&gt;alert(1)");
        seen_env |= h.contains("[redacted]") && h.contains(".env");
    }
    assert!(seen_script, "the script text shows escaped");
    assert!(seen_env, "the .env result is replaced");
}

#[test]
fn no_redact_warns_and_keeps_the_key() {
    let d = tempfile::tempdir().unwrap();
    let key = fake_key();
    ok(&nexo(d.path(), MOCK, &["-p", &format!("my key {key}")]));
    let o = nexo(d.path(), MOCK, &["export", "--no-redact", "--json", "-o", "-"]);
    let json = ok(&o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("warning"));
    assert!(json.contains(&key));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["messages"].as_array().unwrap().len() >= 2);
    let red = ok(&nexo(d.path(), MOCK, &["export", "--json", "-o", "-"]));
    assert!(!red.contains(&key));
    let md = ok(&nexo(d.path(), MOCK, &["export", "--markdown", "-o", "-"]));
    assert!(md.contains("## You") && !md.contains(&key));
}

#[test]
fn audit_writes_a_line_per_call_and_request_and_the_report_sums() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "hello file\n").unwrap();
    let log = d.path().join("audit.jsonl");
    ok(&nexo(d.path(), &audit_config(&log), &["-p", "tool:read {\"path\":\"a.txt\"}"]));
    let l = lines(&log);
    let tools: Vec<_> = l.iter().filter(|v| v["kind"] == "tool").collect();
    let reqs: Vec<_> = l.iter().filter(|v| v["kind"] == "request").collect();
    assert_eq!(tools.len(), 1, "{l:?}");
    assert_eq!(tools[0]["tool"], "read");
    assert_eq!(tools[0]["user"], "tester");
    assert_eq!(tools[0]["status"], "ok");
    assert_eq!(tools[0]["decision"], "allowed");
    assert_eq!(tools[0]["by"], "rule");
    assert!(tools[0]["args"].as_str().unwrap().contains("a.txt"));
    assert!(tools[0]["session"].as_str().unwrap().len() > 8 && tools[0]["duration_ms"].is_u64());
    assert_eq!(reqs.len(), 2, "one request per model call: {l:?}");
    assert_eq!(reqs[0]["provider"], "mock");
    assert!(reqs[0]["tokens_in"].is_u64() && reqs[0]["cost_usd"].is_f64());

    // the report over a hand-written log sums exactly
    let sample = d.path().join("sample.jsonl");
    let rows = [
        r#"{"ts":"2026-11-06T10:00:00Z","kind":"tool","user":"ana","provider":"p","model":"m"}"#,
        r#"{"ts":"2026-11-06T10:00:01Z","kind":"tool","user":"ana","provider":"p","model":"m"}"#,
        r#"{"ts":"2026-11-06T10:00:02Z","kind":"request","user":"ana","provider":"p","model":"m","tokens_in":100,"tokens_out":10,"tokens_cached":50,"cost_usd":0.25}"#,
        r#"{"ts":"2026-11-07T10:00:02Z","kind":"request","user":"bob","provider":"p","model":"n","tokens_in":1,"tokens_out":2,"tokens_cached":0,"cost_usd":0.5}"#,
    ];
    std::fs::write(&sample, rows.join("\n") + "\n").unwrap();
    let s = sample.to_str().unwrap();
    let json = ok(&nexo(d.path(), MOCK, &["audit", "report", "--file", s, "--by", "user", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["total"]["tool_calls"], 2);
    assert_eq!(v["total"]["requests"], 2);
    assert_eq!(v["total"]["cost_usd"], 0.75);
    assert_eq!(v["rows"][0]["key"], "bob", "sorted by cost");
    assert_eq!(v["rows"][1]["tool_calls"], 2);
    let by_day = ok(&nexo(d.path(), MOCK, &["audit", "report", "--file", s, "--by", "day"]));
    assert!(by_day.contains("2026-11-06") && by_day.contains("2026-11-07") && by_day.contains("$0.7500"), "{by_day}");
    let by_model =
        ok(&nexo(d.path(), MOCK, &["audit", "report", "--file", s, "--by", "model", "--since", "2026-11-07"]));
    assert!(by_model.contains("p/n") && !by_model.contains("p/m"), "{by_model}");
}

#[test]
fn audit_off_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "x\n").unwrap();
    ok(&nexo(d.path(), MOCK, &["-p", "tool:read {\"path\":\"a.txt\"}"]));
    assert!(!d.path().join("data/audit").exists(), "no audit folder when it is off");
    let log = d.path().join("off.jsonl");
    let cfg = format!("{MOCK}[audit]\nenabled = false\npath = '{}'\n", log.display());
    ok(&nexo(d.path(), &cfg, &["-p", "tool:read {\"path\":\"a.txt\"}"]));
    assert!(!log.exists());
}

#[test]
fn the_default_log_is_one_file_a_month() {
    let d = tempfile::tempdir().unwrap();
    ok(&nexo(d.path(), &format!("{MOCK}[audit]\nenabled = true\n"), &["-p", "hi"]));
    let files: Vec<_> = std::fs::read_dir(d.path().join("data/audit")).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let name = files[0].file_name().to_string_lossy().to_string();
    assert!(name.len() == "2026-11.jsonl".len() && name.ends_with(".jsonl"), "{name}");
}

#[test]
fn a_refused_call_is_logged_as_denied() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(".env"), "A=1\n").unwrap();
    let log = d.path().join("audit.jsonl");
    // nobody can be asked in a headless run, so the read of a .env file is refused
    let _ = nexo(d.path(), &audit_config(&log), &["-p", "tool:read {\"path\":\".env\"}"]);
    let l = lines(&log);
    let t = l.iter().find(|v| v["kind"] == "tool").expect("a tool line");
    assert_eq!(t["status"], "denied", "{t}");
    assert_eq!(t["args"], "[redacted]", "a .env path keeps no arguments: {t}");
}
