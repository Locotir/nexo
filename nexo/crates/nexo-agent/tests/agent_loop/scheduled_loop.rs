//! Session scheduler exercised through the real command and event channels.
use super::*;

fn loop_cmd(input: &str) -> AgentCommand {
    AgentCommand::Loop { input: input.into() }
}

async fn notice_containing(r: &mut Running, needle: &str) {
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains(needle))).await;
}

#[tokio::test]
async fn create_list_cancel_while_held() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Interrupt);
    r.send(loop_cmd("60s check things"));
    notice_containing(&mut r, "loop #1 scheduled").await;
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s: check things").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert!(client.requests().is_empty(), "Esc must hold even an already due loop");
    r.send(loop_cmd("cancel 1"));
    notice_containing(&mut r, "loop #1 cancelled").await;
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "no active loops").await;
    r.send(loop_cmd("cancel 1"));
    notice_containing(&mut r, "loop #1 not found").await;
    r.send(AgentCommand::Submit { text: "resume manually".into(), attachments: vec![] });
    r.turn().await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(client.requests().len(), 1, "cancellation prevents a later scheduled run");
    r.stop().await;
}

#[tokio::test]
async fn busy_turn_defers_loop_and_failed_switches_preserve_it() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Hang(vec![]), Reply::Events(text("scheduled answer"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "human".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TurnStart { .. })).await;
    r.send(loop_cmd("60s scheduled work"));
    notice_containing(&mut r, "loop #1 scheduled").await;
    r.send(AgentCommand::Resume { id: "nonexistent-session".into() });
    notice_containing(&mut r, "finish or interrupt the turn before resuming").await;
    r.send(AgentCommand::Fork { index: None });
    notice_containing(&mut r, "finish or interrupt the turn before forking").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(client.requests().len(), 1, "no overlap while the human turn hangs");
    r.send(AgentCommand::Interrupt);
    r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    r.send(AgentCommand::Resume { id: "nonexistent-session".into() });
    // FIFO list is a barrier after the failed resume, independently of its error text.
    r.send(loop_cmd("list"));
    notice_containing(&mut r, "loop #1 every 60s: scheduled work").await;
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(client.requests().len(), 1, "Esc prevents the due loop starting after interruption");
    r.send(loop_cmd("cancel 1"));
    notice_containing(&mut r, "cancelled").await;
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn scheduled_prompt_runs_the_blocking_submit_hook() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("user_prompt_submit", None, "echo 'loop denied by policy' >&2; exit 2"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.send(loop_cmd("60s forbidden work"));
    notice_containing(&mut r, "loop #1: running scheduled prompt").await;
    notice_containing(&mut r, "loop denied by policy").await;
    assert!(client.requests().is_empty(), "a denied scheduled prompt must not call the model");
    r.stop().await;
}

#[tokio::test]
async fn due_loop_runs_after_busy_turn_finishes_without_overlap() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("human answer")), Reply::Events(text("loop answer"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    // A queued command is processed before the scheduler tick.
    r.send(AgentCommand::Submit { text: "human".into(), attachments: vec![] });
    r.send(loop_cmd("60s scheduled work"));
    r.turn().await;
    r.turn().await;
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].messages.iter().any(|m| m.text().contains("human")));
    assert!(requests[1].messages.iter().any(|m| m.text().contains("scheduled work")));
    r.stop().await;
}

#[tokio::test]
async fn human_created_loop_retains_person_provenance() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("done"))]);
    let mut r = start(client, config(PermissionMode::Auto), None, d.path()).await;
    r.send(loop_cmd("60s human scheduled work"));
    let events = r.turn().await;
    let msg = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::History { message, .. } if message.text().contains("human scheduled work") => {
                Some(message.text())
            }
            _ => None,
        })
        .expect("scheduled history");
    assert!(msg.contains(nexo_core::agent::PERSON_MARK), "{msg}");
    r.stop().await;
}
