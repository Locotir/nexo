//! Offline interactive monitor UI fixture, using a real shell producer, not a model.
//! Run `cargo run -p nexo-tui --example monitor_interactive`; open `/panel`, then `m`.
//! This exercises event delivery and the production terminal loop, not monitor registration.
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use nexo_core::agent::{channels, AgentCommand, AgentEvent, MonitorStatus};
use nexo_core::jobs::{JobInfo, JobKind, JobStatus, JobTail};
use nexo_core::permission::PermissionMode;
use nexo_tui::TuiOptions;

const SCRIPT: &str = "i=0; while [ $i -lt 90 ]; do printf 'MONITOR_EVENT_%03d %s WRAP_END\\n' \"$i\" 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789'; i=$((i+1)); sleep 1; done";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (handle, mut ends) = channels();
    let events = ends.events.clone();
    let cwd = std::env::current_dir()?;
    events.send(AgentEvent::Ready {
        session_id: "monitor-interactive-fixture".into(),
        provider: "mock".into(),
        model: "no-model".into(),
        cwd: cwd.clone(),
        permission_mode: PermissionMode::Ask,
    })?;
    let mut child = Command::new("sh").args(["-c", SCRIPT]).stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let job = JobInfo {
        id: 73,
        kind: JobKind::Shell,
        label: SCRIPT.into(),
        status: JobStatus::Running,
        elapsed_ms: 0,
        lines: 0,
        held: true,
    };
    events.send(AgentEvent::Jobs { jobs: vec![job.clone()] })?;
    events.send(AgentEvent::Monitors { ids: vec![73], states: vec![(73, MonitorStatus::Listening)] })?;
    let output_events = events.clone();
    let producer = std::thread::spawn(move || {
        let mut job = job;
        let mut tail = Vec::new();
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            tail.push(line);
            job.lines += 1;
            if tail.len() > 200 {
                tail.remove(0);
            }
            if output_events
                .send(AgentEvent::JobTails {
                    tails: vec![JobTail { id: 73, text: tail.join("\n"), ended_ago_ms: None }],
                })
                .is_err()
            {
                break;
            }
        }
        job.status = JobStatus::Done { ok: true, how: "exit 0".into() };
        let _ = output_events.send(AgentEvent::Jobs { jobs: vec![job] });
        let _ = output_events.send(AgentEvent::Monitors { ids: vec![73], states: vec![(73, MonitorStatus::Paused)] });
    });
    tokio::spawn(async move {
        let mut log = std::env::var_os("MONITOR_COMMAND_LOG").and_then(|path| std::fs::File::create(path).ok());
        while let Some(command) = ends.commands.recv().await {
            if let Some(log) = &mut log {
                let _ = writeln!(log, "{command:?}");
            }
            if matches!(command, AgentCommand::Shutdown) {
                let _ = events.send(AgentEvent::Shutdown);
                break;
            }
        }
    });
    let result = nexo_tui::run(
        handle,
        TuiOptions {
            cwd,
            provider: "mock".into(),
            model: "no-model".into(),
            animations: false,
            hints: false,
            complete_words: false,
            ..Default::default()
        },
    )
    .await;
    let _ = child.kill();
    let _ = child.wait();
    let _ = producer.join();
    result
}
