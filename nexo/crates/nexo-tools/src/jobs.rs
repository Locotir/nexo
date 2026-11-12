//! `jobs`: the session's background jobs (commands started with `bash` and
//! `background: true`, sub-agents sent with `task` and `background: true`):
//! list them, read what one printed, wait for it, stop it.

use std::time::Duration;

use async_trait::async_trait;
use nexo_core::jobs::{duration, JobInfo, JobKind};
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::util::{int_arg, one_line, str_arg};

/// Longest a read waits for a job to end.
pub const MAX_WAIT_S: u64 = 600;

pub struct Jobs;

fn row(j: &JobInfo) -> String {
    let kind = match j.kind {
        JobKind::Shell => "shell",
        JobKind::Agent => "agent",
    };
    format!(
        "{:>3}  {:<5}  {:<14}  {:>10}  {:>6} lines  {}",
        j.id,
        kind,
        j.status.word(),
        duration(j.elapsed_ms),
        j.lines,
        one_line(&j.label, 80)
    )
}

#[async_trait]
impl Tool for Jobs {
    fn name(&self) -> &str {
        "jobs"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "jobs".into(),
            description: "The background jobs of this session (`bash` commands and `task` sub-agents \
                started with `background: true`). \
                action \"list\": every job and its state. action \"output\": what job `id` printed since \
                the last read, and its state; with `wait` (seconds, at most 600) it first waits for the \
                job to end. action \"kill\": stop job `id`."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "output", "kill"], "description": "What to do. Default list."},
                    "id": {"type": "integer", "description": "The job, for output and kill."},
                    "wait": {"type": "integer", "minimum": 0, "maximum": 600, "description": "Seconds to wait for the job to end before reading (output only). Default 0."}
                }
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "List background jobs, read a job's new output, or kill one.",
            params: &[("wait", "Seconds to wait (output)")],
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn summary(&self, args: &Value) -> String {
        let id = int_arg(args, "id").map(|i| format!(" {i}")).unwrap_or_default();
        match str_arg(args, "action").unwrap_or("list") {
            "output" => match int_arg(args, "wait").filter(|w| *w > 0) {
                Some(w) => format!("jobs: output{id} (wait {w} s)"),
                None => format!("jobs: output{id}"),
            },
            a => format!("jobs: {a}{id}"),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let action = str_arg(&args, "action").unwrap_or("list");
        let id = int_arg(&args, "id").map(|i| i as u32);
        match (action, id) {
            ("list", _) => {
                let all = ctx.jobs.list();
                if all.is_empty() {
                    return ToolOutput::ok("No background jobs in this session.");
                }
                let mut text = String::from(" id  kind   state                 time    output  what\n");
                for j in &all {
                    text.push_str(&row(j));
                    text.push('\n');
                }
                ToolOutput::ok(text)
            }
            ("output" | "kill", None) => ToolOutput::error(format!("jobs: `id` is required for {action}")),
            ("output", Some(id)) => {
                let wait = Duration::from_secs(int_arg(&args, "wait").unwrap_or(0).min(MAX_WAIT_S));
                match ctx.jobs.read(id, wait, &ctx.cancel).await {
                    None => ToolOutput::error(format!("jobs: there is no job {id} (see action \"list\")")),
                    Some(r) => {
                        let mut text = format!("job {id}: {}, {}", r.info.status.word(), duration(r.info.elapsed_ms));
                        if r.info.status.running() && !wait.is_zero() {
                            text.push_str(&format!(" (still running after waiting {} s)", wait.as_secs()));
                        }
                        text.push('\n');
                        if r.new.is_empty() {
                            text.push_str("(no new output)");
                        } else {
                            text.push_str(&r.new);
                        }
                        ToolOutput::ok(text)
                    }
                }
            }
            ("kill", Some(id)) => match ctx.jobs.get(id) {
                None => ToolOutput::error(format!("jobs: there is no job {id}")),
                Some(j) if !j.status.running() => {
                    ToolOutput::ok(format!("job {id} had already ended: {}", j.status.word()))
                }
                Some(_) => {
                    ctx.jobs.kill(id);
                    ToolOutput::ok(format!("job {id} stopped"))
                }
            },
            (other, _) => ToolOutput::error(format!("jobs: unknown action `{other}` (list, output or kill)")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bash::Bash;

    #[test]
    fn a_background_command_runs_on_and_is_read_and_killed() {
        crate::util::run_bounded(Duration::from_secs(120), "a_background_command_runs_on", async {
            if nexo_core::shell::shell().kind == "powershell" {
                eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
                return;
            }
            let d = tempfile::tempdir().unwrap();
            let c = ToolContext::new(d.path().to_path_buf());
            let t = std::time::Instant::now();
            let out = Bash.run(json!({"command": "echo one; sleep 1; echo two", "background": true}), &c).await;
            assert!(t.elapsed() < Duration::from_millis(800), "returns at once");
            assert!(out.content.contains("background job 1"), "{}", out.content);
            // the turn that started it ends: the job does not
            c.cancel.cancel();
            let c = ToolContext { cancel: Default::default(), ..c };
            let out = Jobs.run(json!({"action": "output", "id": 1, "wait": 20}), &c).await;
            assert!(out.content.starts_with("job 1: done (exit 0)"), "{}", out.content);
            assert!(out.content.contains("one\ntwo\n"), "{}", out.content);
            assert!(c.jobs.take_ended().is_empty(), "read whole: no notice");

            Bash.run(json!({"command": "sleep 60", "background": true}), &c).await;
            Bash.run(json!({"command": "exit 3", "background": true}), &c).await;
            let list = Jobs.run(json!({}), &c).await.content;
            assert!(list.contains("sleep 60") && list.contains("running"), "{list}");
            assert_eq!(Jobs.run(json!({"action": "kill", "id": 2}), &c).await.content, "job 2 stopped");
            let out = Jobs.run(json!({"action": "output", "id": 3, "wait": 20}), &c).await;
            assert!(out.content.starts_with("job 3: done (exit 3)"), "{}", out.content);
            assert_eq!(c.jobs.running(), 0);
            assert_eq!(c.jobs.get(2).unwrap().status, nexo_core::JobStatus::Killed);
            assert!(Jobs.run(json!({"action": "output"}), &c).await.is_error);
            assert!(Jobs.run(json!({"action": "output", "id": 9}), &c).await.is_error);
            assert_eq!(Jobs.summary(&json!({"action": "output", "id": 2, "wait": 30})), "jobs: output 2 (wait 30 s)");
        });
    }
}
