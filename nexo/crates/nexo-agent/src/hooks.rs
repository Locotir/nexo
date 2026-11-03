//! Hooks: shell commands the configuration runs at points of the loop.
//! `pre_tool` runs before a tool (exit code 2 blocks the call, and what the
//! command printed is the reason the model is given), `post_tool` after it,
//! `turn_end` when a request is finished. Each command gets the event as JSON
//! on stdin and `NEXO_EVENT`, `NEXO_TOOL`, `NEXO_CWD` in its environment.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use nexo_core::config::HookConfig;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

/// How long a hook may run.
const LIMIT: Duration = Duration::from_secs(60);

/// What the hooks of one event said.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// A `pre_tool` hook blocked the call, with this reason.
    pub blocked: Option<String>,
    /// Hooks that failed, one line each.
    pub failures: Vec<String>,
}

/// `pattern` matches `tool`: empty or `*`, the name itself, or a prefix ending in `*`.
fn matches(pattern: Option<&str>, tool: Option<&str>) -> bool {
    let Some(p) = pattern.map(str::trim).filter(|p| !p.is_empty() && *p != "*") else { return true };
    let Some(t) = tool else { return false };
    match p.strip_suffix('*') {
        Some(prefix) => t.starts_with(prefix),
        None => p.split('|').any(|alt| alt.trim() == t),
    }
}

/// Runs the hooks of `event` (for `tool`, when it is a tool event), one after another.
pub async fn run(hooks: &[HookConfig], event: &str, tool: Option<&str>, payload: &Value, cwd: &Path) -> Outcome {
    let mut out = Outcome::default();
    for h in hooks.iter().filter(|h| h.event == event && matches(h.matcher.as_deref(), tool)) {
        let shell = nexo_core::shell::shell();
        let mut cmd = tokio::process::Command::new(&shell.program);
        cmd.args(shell.args_for(&h.command))
            .current_dir(cwd)
            .env("NEXO_EVENT", event)
            .env("NEXO_TOOL", tool.unwrap_or(""))
            .env("NEXO_CWD", cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                out.failures.push(format!("hook `{}` did not start: {e}", h.command));
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.to_string().as_bytes()).await;
        }
        let done = match tokio::time::timeout(LIMIT, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                out.failures.push(format!("hook `{}`: {e}", h.command));
                continue;
            }
            Err(_) => {
                out.failures.push(format!(
                    "hook `{}` took longer than {} s and was stopped",
                    h.command,
                    LIMIT.as_secs()
                ));
                continue;
            }
        };
        let said = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
        let (stdout, stderr) = (said(&done.stdout), said(&done.stderr));
        match done.status.code() {
            Some(0) => {}
            Some(2) if event == "pre_tool" => {
                let why = if stderr.is_empty() { stdout } else { stderr };
                out.blocked = Some(if why.is_empty() { format!("`{}` said no", h.command) } else { why });
                return out;
            }
            code => {
                let code = code.map(|c| c.to_string()).unwrap_or_else(|| "a signal".into());
                let detail = if stderr.is_empty() { stdout } else { stderr };
                out.failures.push(format!(
                    "hook `{}` failed ({code}){}",
                    h.command,
                    if detail.is_empty() { String::new() } else { format!(": {detail}") }
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use serde_json::json;

    #[cfg(unix)]
    fn hook(event: &str, matcher: Option<&str>, command: &str) -> HookConfig {
        HookConfig { event: event.into(), matcher: matcher.map(String::from), command: command.into() }
    }

    #[test]
    fn matchers() {
        assert!(matches(None, Some("bash")) && matches(Some("*"), None));
        assert!(matches(Some("bash"), Some("bash")) && !matches(Some("bash"), Some("read")));
        assert!(matches(Some("web_*"), Some("web_fetch")) && matches(Some("edit|write"), Some("write")));
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pre_tool_hook_blocks_with_its_reason_and_others_report_failures() {
        let d = tempfile::tempdir().unwrap();
        let hooks = vec![
            hook("pre_tool", Some("bash"), "grep -q 'rm -rf' && { echo 'no rm -rf here' >&2; exit 2; } || exit 0"),
            hook("post_tool", None, "cat > seen.json"),
            hook("turn_end", None, "exit 3"),
        ];
        let blocked = run(&hooks, "pre_tool", Some("bash"), &json!({"args": {"command": "rm -rf /"}}), d.path()).await;
        assert_eq!(blocked.blocked.as_deref(), Some("no rm -rf here"));
        let fine = run(&hooks, "pre_tool", Some("bash"), &json!({"args": {"command": "ls"}}), d.path()).await;
        assert_eq!(fine, Outcome::default());
        let other = run(&hooks, "pre_tool", Some("read"), &json!({}), d.path()).await;
        assert!(other.blocked.is_none(), "the matcher leaves other tools alone");
        run(&hooks, "post_tool", Some("read"), &json!({"tool": "read", "is_error": false}), d.path()).await;
        assert!(std::fs::read_to_string(d.path().join("seen.json")).unwrap().contains("\"tool\":\"read\""));
        let end = run(&hooks, "turn_end", None, &json!({}), d.path()).await;
        assert_eq!(end.failures, ["hook `exit 3` failed (3)"]);
    }
}
