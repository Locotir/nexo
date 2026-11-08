//! The cheap model behind the supervisor, with a stand-in judge (no network): it is asked about an unanswered
//! question once, its answer goes out through the stop hook, a broken answer means no nudge, and a live session
//! of this program gets its nudge as a message of its own.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nexo_agent::supervise::model::{Judge, Judged};
use nexo_agent::supervise::socket::{ask, HookKind, Request, HOOK_TIMEOUT};
use nexo_agent::supervise::{audit::Audit, NexoSource, Options, Panel, Policy, Setup};
use nexo_core::agent::{AgentEvent, TurnEnd};
use serde_json::{json, Value};

struct Stand {
    reply: String,
    calls: AtomicUsize,
    inputs: Mutex<Vec<String>>,
}

impl Judge for Stand {
    fn judge(&self, _system: &str, input: &str) -> Result<Judged, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().unwrap().push(input.to_string());
        Ok(Judged { text: self.reply.clone(), cost_usd: 0.002 })
    }
    fn model(&self) -> String {
        "stand-in".into()
    }
}

fn stand(reply: &str) -> Arc<Stand> {
    Arc::new(Stand { reply: reply.into(), calls: AtomicUsize::new(0), inputs: Mutex::new(Vec::new()) })
}

const POLICY: &str = r#"
interval_secs = 0
scan_hours = 100000
[model]
provider = "x"
name = "y"
"#;

struct Rig {
    _dir: tempfile::TempDir,
    sock: PathBuf,
    state_dir: PathBuf,
    panel: Panel,
}

fn rig(judge: Arc<Stand>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs").join("proj");
    std::fs::create_dir_all(&logs).unwrap();
    let line = json!({"type":"assistant","timestamp":"2026-10-01T00:00:00Z","cwd":"/work/app",
        "message":{"stop_reason":"end_turn","content":[{"type":"text",
        "text": format!("Done with the parser. Shall I continue with the tests? key {}", secret())}]}});
    std::fs::write(logs.join("sess-1.jsonl"), format!("{line}\n")).unwrap();
    let sock = dir.path().join("sup.sock");
    let state_dir = dir.path().join("state");
    let setup = Setup {
        logs_root: Some(dir.path().join("logs")),
        scan_hours: 100_000,
        list_cmd: None,
        list_every: 10,
        poll_every_ms: 100,
    };
    let opts = Options { socket: Some(sock.clone()), dir: Some(state_dir.clone()), must_listen: true };
    let panel = Panel::start_with(&setup, Policy::parse(POLICY).unwrap(), NexoSource::default(), &opts).unwrap();
    panel.set_judge(judge);
    Rig { _dir: dir, sock, state_dir, panel }
}

fn stop(r: &Rig, active: bool) -> Value {
    let p = json!({"session_id": "sess-1", "cwd": "/work/app", "stop_hook_active": active});
    ask(&r.sock, &Request::Hook { kind: HookKind::Stop, payload: p }, HOOK_TIMEOUT).unwrap().out
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(8) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("timed out: {what}");
}

#[test]
fn the_model_text_goes_out_through_the_stop_hook_and_is_asked_once() {
    let j = stand(r#"{"action":"nudge","text":"Yes, run the tests next."}"#);
    let r = rig(j.clone());
    let audit = Audit::new(r.state_dir.join("audit.jsonl"));
    wait_for("the model answered", || audit.read().iter().any(|l| l.action == "model_nudge"));
    let out = stop(&r, false);
    assert_eq!(out["decision"], "block", "{out}");
    assert_eq!(out["reason"], "Yes, run the tests next.");
    let lines = audit.read();
    let sent = lines.iter().find(|l| l.action == "nudge").unwrap();
    assert_eq!(sent.rule, "model");
    let asked = lines.iter().find(|l| l.action == "model_nudge").unwrap();
    assert!(asked.cost_usd > 0.0);
    assert_eq!(j.calls.load(Ordering::SeqCst), 1, "the same state is asked once");
    let seen = j.inputs.lock().unwrap().join("\n");
    assert!(seen.contains("Shall I continue"), "{seen}");
    assert!(!seen.contains(&secret()), "a secret reached the model: {seen}");
    assert!(r.panel.policy_lines().iter().any(|l| l.contains("model this hour") && l.contains("· 1 of")));
}

#[test]
fn a_broken_answer_sends_no_model_nudge() {
    let j = stand("sure, go for it!");
    let r = rig(j.clone());
    let audit = Audit::new(r.state_dir.join("audit.jsonl"));
    wait_for("the model answered", || audit.read().iter().any(|l| l.action == "model_wait"));
    assert_eq!(stop(&r, false), json!({}), "wait: the session is left alone");
}

fn nexo_session(src: &NexoSource, text: &str, at: i64) {
    src.observe(&AgentEvent::SessionChanged { session_id: "s1".into(), title: "Main".into() }, at);
    src.observe(&AgentEvent::TurnStart { turn: 1 }, at);
    src.observe(&AgentEvent::TextDelta { text: text.into() }, at + 1);
    src.observe(&AgentEvent::TurnEnd { reason: TurnEnd::Done }, at + 2);
}

#[test]
fn a_live_session_with_an_open_question_gets_evidence_for_its_check_in_once() {
    let src = NexoSource::default();
    let mut s = Setup::none().supervisor(Policy::parse("interval_secs = 0").unwrap(), src.clone());
    nexo_session(&src, "Shall I go on with the plan?", 1000);
    s.tick(1000 + 400);
    let sent = src.take_nudges();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].key, "s1");
    assert!(!sent[0].manual, "an automatic nudge is not the person's message");
    assert!(!sent[0].text.contains("[supervisor]"), "{}", sent[0].text);
    s.tick(1000 + 410);
    assert!(src.take_nudges().is_empty(), "the cool-down holds the next one back");
}

#[test]
fn a_live_session_that_finished_its_work_is_not_woken() {
    let src = NexoSource::default();
    let mut s = Setup::none().supervisor(Policy::parse("interval_secs = 0").unwrap(), src.clone());
    nexo_session(&src, "All done, tests pass.", 1000);
    s.tick(1000 + 3 * 3600);
    assert!(src.take_nudges().is_empty());
}

#[test]
fn a_paused_live_session_gets_nothing() {
    let src = NexoSource::default();
    let mut s = Setup::none().supervisor(Policy::parse("interval_secs = 0").unwrap(), src.clone());
    nexo_session(&src, "Shall I go on?", 1000);
    s.tick(1100);
    s.set_pause(None, true, 1100);
    s.tick(1000 + 400);
    assert!(src.take_nudges().is_empty());
}

fn secret() -> String {
    format!("sk-{}", "a1B2c3D4".repeat(5))
}

#[test]
fn a_message_typed_in_the_panel_stays_the_persons_own() {
    let src = NexoSource::default();
    let mut s = Setup::none().supervisor(Policy::parse("interval_secs = 0").unwrap(), src.clone());
    nexo_session(&src, "All done.", 1000);
    s.tick(1010);
    s.queue_manual("s1", "please also run the linter", 1020);
    let sent = src.take_nudges();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].manual);
    assert_eq!(sent[0].text, "please also run the linter");
}
