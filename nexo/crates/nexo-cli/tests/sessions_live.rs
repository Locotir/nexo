//! `nexo sessions live` run as the binary against a temporary data folder with fake heartbeats.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
use nexo_core::mail::Inbound;
use nexo_core::mailbox::{now_ms, Mailbox};

fn nexo(data: &Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(args)
        .env("NEXO_CONFIG_DIR", data.join("config"))
        .env("NEXO_DATA_DIR", data)
        .env("NEXO_CACHE_DIR", data.join("cache"))
        .stdin(Stdio::null())
        .output()
        .expect("nexo runs");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn open(data: &Path, id: &str, name: &str, clock: i64) -> Arc<Mailbox> {
    Mailbox::open_with_clock(&data.join("mail"), id, name, "/work/x", false, Inbound::Auto, Arc::new(move || clock))
        .unwrap()
}

#[test]
fn nothing_live_says_so_and_succeeds() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(nexo(d.path(), &["sessions", "live"]), "no live sessions\n");
    assert_eq!(nexo(d.path(), &["sessions", "live", "--json"]).trim(), "[]");
}

#[test]
fn live_sessions_show_their_state_model_and_jobs_and_stale_ones_do_not_show() {
    let d = tempfile::tempdir().unwrap();
    let now = now_ms();
    let fresh = open(d.path(), "20261115-120000-aaaa", "api work", now);
    fresh.set_idle(true);
    let busy = open(d.path(), "20261115-120001-bbbb", "web work", now);
    busy.set_jobs(&[JobInfo {
        id: 7,
        kind: JobKind::Agent,
        label: "review the diff".into(),
        status: JobStatus::Running,
        elapsed_ms: 0,
        lines: 0,
    }]);
    let _stale = open(d.path(), "20261115-110000-cccc", "stale", now - 60_000);
    let sessions = d.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("20261115-120001-bbbb.jsonl"),
        concat!(
            r#"{"type":"meta","id":"20261115-120001-bbbb","cwd":"/work/x","created":"2026-11-15T12:00:01Z","#,
            r#""provider":"deepseek","model":"deepseek-chat","title":"the web task"}"#,
            "\n",
            r#"{"type":"message","index":0,"ts":"t","message":{"role":"user","content":"SECRET-PROMPT-TEXT"}}"#,
            "\n"
        ),
    )
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&nexo(d.path(), &["sessions", "live", "--json"])).unwrap();
    let list = json.as_array().unwrap();
    assert_eq!(list.len(), 2, "{json}");
    let by = |id: &str| list.iter().find(|e| e["id"] == id).unwrap();
    let (a, b) = (by("20261115-120000-aaaa"), by("20261115-120001-bbbb"));
    assert_eq!((a["state"].as_str(), a["title"].as_str()), (Some("idle"), Some("api work")));
    assert!(a["provider"].is_null() && a["model"].is_null() && a["jobs"].as_array().unwrap().is_empty());
    assert_eq!((b["state"].as_str(), b["title"].as_str()), (Some("busy"), Some("the web task")));
    assert_eq!(
        (b["provider"].as_str(), b["model"].as_str(), b["cwd"].as_str()),
        (Some("deepseek"), Some("deepseek-chat"), Some("/work/x"))
    );
    assert_eq!(b["jobs"][0]["id"], 7);
    assert_eq!(b["jobs"][0]["kind"], "agent");
    assert_eq!(b["jobs"][0]["state"], "running");
    assert_eq!(b["jobs"][0]["title"], "review the diff");
    let mut keys: Vec<&str> = b.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "beat_age_ms",
            "cwd",
            "id",
            "jobs",
            "last_beat_ms",
            "model",
            "pid",
            "provider",
            "started_ms",
            "state",
            "title"
        ]
    );

    let text = nexo(d.path(), &["sessions", "live"]);
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(text.contains("20261115-120001-bbbb busy deepseek/deepseek-chat /work/x"), "{text}");
    assert!(text.contains("\n  job 7 agent running review the diff\n"), "{text}");
    assert!(!text.contains("stale") && !text.contains("SECRET-PROMPT-TEXT"), "{text}");
    assert!(!json.to_string().contains("SECRET-PROMPT-TEXT"));
}
