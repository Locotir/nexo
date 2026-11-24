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
                    "action": {"type": "string", "enum": ["list", "output", "kill"], "description": "Default list."},
                    "id": {"type": "integer"},
                    "wait": {"type": "integer", "minimum": 0, "maximum": 600}
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

    fn summary_live(&self, args: &Value, jobs: &nexo_core::jobs::Jobs) -> String {
        let id = int_arg(args, "id");
        let job = id.and_then(|i| jobs.get(i as u32));
        let (Some(id), Some(job)) = (id, job) else { return self.summary(args) };
        // what the job is: a shell command, or a sub-agent by its task
        let what = one_line(&job.label, 48);
        let who = match job.kind {
            JobKind::Shell => format!("shell job {id} · {what}"),
            JobKind::Agent => format!("sub-agent \"{what}\" (job {id})"),
        };
        match str_arg(args, "action").unwrap_or("list") {
            "output" => match int_arg(args, "wait").filter(|w| *w > 0) {
                Some(w) => format!("jobs: waiting for {who} · up to {} s", w.min(MAX_WAIT_S)),
                None => format!("jobs: output of {who}"),
            },
            "kill" => format!("jobs: stopping {who}"),
            _ => self.summary(args),
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
                // while the model waits, the screen sees the job's newest output line
                let read = ctx.jobs.read(id, wait, &ctx.cancel);
                tokio::pin!(read);
                // A message/key releases only this wait, never the background worker. Use the same
                // signal as foreground `task`, so the coordinator can hear the queued message now.
                let release = async {
                    match &ctx.detach {
                        Some(detach) => {
                            detach.wait().await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::pin!(release);
                let mut yielded = false;
                let mut tick = tokio::time::interval(Duration::from_millis(700));
                let mut shown = String::new();
                let read = loop {
                    tokio::select! {
                        biased;
                        r = &mut read => break r,
                        _ = &mut release, if !wait.is_zero() => {
                            yielded = true;
                            break ctx.jobs.read(id, Duration::ZERO, &ctx.cancel).await;
                        }
                        _ = tick.tick(), if !wait.is_zero() => {
                            if let Some(l) = ctx.jobs.last_line(id).filter(|l| *l != shown) {
                                ctx.report(one_line(&l, 300));
                                shown = l;
                            }
                        }
                    }
                };
                match read {
                    None => ToolOutput::error(format!("jobs: there is no job {id} (see action \"list\")")),
                    Some(r) => {
                        let mut text = format!("job {id}: {}, {}", r.info.status.word(), duration(r.info.elapsed_ms));
                        if r.info.status.running() && !wait.is_zero() {
                            if yielded {
                                text.push_str(" (wait released for interaction; job continues in the background)");
                            } else {
                                text.push_str(&format!(" (still running after waiting {} s)", wait.as_secs()));
                            }
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
    fn a_wait_on_a_job_names_the_job_and_says_how_long_it_may_wait() {
        let jobs = nexo_core::jobs::Jobs::new();
        let shell = jobs.start(JobKind::Shell, "cargo test -p nexo-bin");
        let agent = jobs.start(JobKind::Agent, "map the session files and their readers\nsecond line");
        let id = shell.id();
        let line = |args: Value| Jobs.summary_live(&args, &jobs);
        assert_eq!(
            line(json!({"action": "output", "id": id, "wait": 60})),
            format!("jobs: waiting for shell job {id} · cargo test -p nexo-bin · up to 60 s")
        );
        assert_eq!(
            line(json!({"action": "output", "id": id})),
            format!("jobs: output of shell job {id} · cargo test -p nexo-bin")
        );
        assert_eq!(
            line(json!({"action": "kill", "id": id})),
            format!("jobs: stopping shell job {id} · cargo test -p nexo-bin")
        );
        // a sub-agent goes by its task, on one line
        assert_eq!(
            line(json!({"action": "output", "id": agent.id(), "wait": 900})),
            format!(
                "jobs: waiting for sub-agent \"map the session files and their readers …\" (job {}) · up to 600 s",
                agent.id()
            )
        );
        // a job the session does not know, or the list: the arguments alone
        assert_eq!(line(json!({"action": "output", "id": 99, "wait": 5})), "jobs: output 99 (wait 5 s)");
        assert_eq!(line(json!({"action": "list"})), "jobs: list");
    }

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

    #[test]
    fn interaction_releases_a_wait_without_stopping_or_losing_the_agents_output() {
        crate::util::run_bounded(Duration::from_secs(10), "interaction_releases_jobs_wait", async {
            for why in [nexo_core::jobs::DetachWhy::Message, nexo_core::jobs::DetachWhy::Key] {
                let mut c = ToolContext::new(std::env::current_dir().unwrap());
                let detach = nexo_core::jobs::Detach::default();
                c.detach = Some(detach.clone());
                let h = c.jobs.start(JobKind::Agent, "slow worker");
                let id = h.id();
                h.line("before interaction");
                let wait = Jobs.run(json!({"action": "output", "id": id, "wait": 600}), &c);
                tokio::pin!(wait);
                // Poll the actual wait first, then fire while it is suspended.
                tokio::select! {
                    _ = &mut wait => panic!("a running job must wait"),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
                detach.fire(why);
                let out = tokio::time::timeout(Duration::from_secs(1), &mut wait).await.unwrap();
                assert!(out.content.contains("wait released for interaction"), "{}", out.content);
                assert!(out.content.contains("before interaction"), "{}", out.content);
                assert!(!out.content.contains("after waiting 600 s"));
                assert!(!c.cancel.is_cancelled());
                assert!(c.jobs.get(id).unwrap().status.running());
                h.line("after interaction");
                h.finish(true, "reported");
                let out = Jobs.run(json!({"action": "output", "id": id, "wait": 600}), &c).await;
                assert!(out.content.contains("after interaction"), "{}", out.content);
                assert!(!out.content.contains("before interaction"));
                assert!(!out.content.contains("wait released"), "ended jobs win over the sticky signal");
                assert!(c.jobs.take_ended().is_empty(), "the report was consumed exactly once");
            }
        });
    }

    #[test]
    fn a_wait_shows_the_newest_line_of_the_job_as_progress() {
        crate::util::run_bounded(Duration::from_secs(30), "a_wait_shows_the_newest_line", async {
            let d = tempfile::tempdir().unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let mut c = ToolContext::new(d.path().to_path_buf());
            c.progress = Some(tx);
            let h = c.jobs.start(JobKind::Agent, "map the readers");
            let id = h.id();
            let jobs = c.jobs.clone();
            tokio::spawn(async move {
                h.line("reading a.rs");
                tokio::time::sleep(Duration::from_millis(1500)).await;
                h.line("reading b.rs");
                tokio::time::sleep(Duration::from_millis(1000)).await;
                h.finish(true, "reported");
            });
            let out = Jobs.run(json!({"action": "output", "id": id, "wait": 20}), &c).await;
            assert!(out.content.contains("reading b.rs"), "{}", out.content);
            drop(c);
            let mut lines = Vec::new();
            while let Ok(p) = rx.try_recv() {
                lines.push(p.line);
            }
            assert_eq!(lines, ["reading a.rs", "reading b.rs"], "each new line once, in order");
            assert_eq!(jobs.last_line(id).as_deref(), Some("reading b.rs"));
        });
    }
}
