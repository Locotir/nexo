//! Hooks: shell commands the configuration (or a trusted project's `.claude/settings.json`) runs at
//! points of the loop. See `docs/HOOKS.md` for the events, the JSON and the exit codes.
//!
//! Each command gets one JSON object on stdin (`session_id`, `cwd`, `event`, `hook_event_name`, and the
//! fields of the event) and `NEXO_EVENT`, `NEXO_TOOL`, `NEXO_CWD` in its environment. Exit 0 goes on, and
//! the stdout JSON may carry a decision; exit 2 blocks where the event can be blocked, with stderr as the
//! reason; any other exit code is a failure the person is told about and nothing more.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use nexo_core::config::HookConfig;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

/// How long a hook may run when it sets no `timeout`.
const LIMIT: Duration = Duration::from_secs(60);

/// A hook's verdict on a tool call or a permission question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run without asking the person (the deny rules still apply).
    Allow,
    /// Ask the person even if a rule would let it run.
    Ask,
}

/// What the hooks of one event said.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// A hook blocked (exit 2, or a `deny` decision), with this reason.
    pub blocked: Option<String>,
    /// An `allow` or `ask` decision (`ask` wins over `allow`).
    pub decision: Option<Decision>,
    /// Text for the model: `additional_context` of the hooks, in order.
    pub context: Vec<String>,
    /// The tool input a hook replaced (the last one wins).
    pub updated_input: Option<Value>,
    /// Hooks that failed, one line each.
    pub failures: Vec<String>,
}

/// The events whose hooks may block with exit 2 or a `deny` decision.
fn can_block(event: &str) -> bool {
    matches!(event, "pre_tool" | "post_tool" | "user_prompt_submit" | "stop" | "permission_request")
}

/// The events where plain text printed on exit 0 is context for the model.
fn stdout_is_context(event: &str) -> bool {
    matches!(event, "user_prompt_submit" | "session_start")
}

/// True when some hook listens to `event` (so the caller can skip building its payload).
pub fn listens(hooks: &[HookConfig], event: &str) -> bool {
    hooks.iter().any(|h| h.event_name() == Some(event))
}

/// `pattern` matches `subject` (a tool name; for other events the source, agent or kind): empty or `*`
/// is everything, a trailing `*` after plain text is a prefix, anything else is a regex that has to
/// match the whole name (`edit|write`, `mcp__.*`). A pattern that is no regex must equal the name.
fn matches(pattern: Option<&str>, subject: Option<&str>) -> bool {
    let Some(p) = pattern.map(str::trim).filter(|p| !p.is_empty() && *p != "*") else { return true };
    let Some(t) = subject else { return false };
    if let Some(prefix) = p.strip_suffix('*') {
        if !prefix.chars().any(|c| ".()[]{}^$+?\\|*".contains(c)) {
            return t.starts_with(prefix);
        }
    }
    match regex::Regex::new(&format!("^(?:{p})$")) {
        Ok(re) => re.is_match(t),
        Err(_) => p == t,
    }
}

/// The JSON a hook reads: `fields` plus the session, the folder and the event's two names.
fn payload_of(fields: &Value, event: &str, tool: Option<&str>, cwd: &Path, session_id: &str) -> Value {
    let mut obj = fields.as_object().cloned().unwrap_or_default();
    obj.insert("session_id".into(), json!(session_id));
    obj.insert("cwd".into(), json!(cwd.to_string_lossy()));
    obj.insert("event".into(), json!(event));
    obj.insert("hook_event_name".into(), json!(long_name(event)));
    if let Some(t) = tool {
        obj.entry("tool_name").or_insert_with(|| json!(t));
    }
    Value::Object(obj)
}

/// The event as other tools spell it: `pre_tool` is `PreToolUse`.
fn long_name(event: &str) -> String {
    let camel: String = event
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect();
    match event {
        "pre_tool" | "post_tool" => format!("{camel}Use"),
        _ => camel,
    }
}

/// What a hook printed as JSON on exit 0.
#[derive(Default)]
struct Reply {
    decision: Option<String>,
    reason: Option<String>,
    context: Option<String>,
    updated: Option<Value>,
}

/// Reads the stdout JSON: `decision` (`allow`, `deny`, `ask`; `approve` and `block` too), `reason`,
/// `additional_context`, `updated_input`, also under the `hookSpecificOutput` key other tools use.
fn parse_reply(stdout: &str) -> Option<Reply> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    let obj = v.as_object()?;
    let special = obj.get("hookSpecificOutput").and_then(Value::as_object);
    let pick = |keys: &[&str]| -> Option<&Value> {
        keys.iter().find_map(|k| obj.get(*k).or_else(|| special.and_then(|s| s.get(*k))))
    };
    let text = |keys: &[&str]| pick(keys).and_then(Value::as_str).map(str::to_string);
    Some(Reply {
        decision: text(&["decision", "permissionDecision"]).map(|d| d.to_ascii_lowercase()),
        reason: text(&["reason", "permissionDecisionReason"]),
        context: text(&["additional_context", "additionalContext"]),
        updated: pick(&["updated_input", "updatedInput"]).filter(|u| u.is_object()).cloned(),
    })
}

/// Runs the hooks of `event` (`subject` is what their `matcher` is tested against: the tool for a tool
/// event), one after another, each reading `fields` completed with the session, the folder and the event.
pub async fn run(
    hooks: &[HookConfig],
    event: &str,
    subject: Option<&str>,
    fields: &Value,
    cwd: &Path,
    session_id: &str,
) -> Outcome {
    let mut out = Outcome::default();
    let mut payload = payload_of(fields, event, subject, cwd, session_id);
    for h in hooks.iter().filter(|h| h.event_name() == Some(event) && matches(h.matcher.as_deref(), subject)) {
        let limit = h.timeout.map(Duration::from_secs).unwrap_or(LIMIT);
        let shell = nexo_core::shell::shell();
        let mut cmd = tokio::process::Command::new(&shell.program);
        cmd.args(shell.args_for(&h.command))
            .current_dir(cwd)
            .env("NEXO_EVENT", event)
            .env("NEXO_TOOL", subject.unwrap_or(""))
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
        // the payload is written on its own: a hook that never reads it must not hold the run past
        // the limit (the write ends when the child is killed and its pipe closes)
        if let Some(mut stdin) = child.stdin.take() {
            let text = payload.to_string();
            tokio::spawn(async move {
                let _ = stdin.write_all(text.as_bytes()).await;
            });
        }
        // on a timeout the child is dropped, and `kill_on_drop` stops it
        let done = match tokio::time::timeout(limit, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                out.failures.push(format!("hook `{}`: {e}", h.command));
                continue;
            }
            Err(_) => {
                out.failures.push(format!(
                    "hook `{}` took longer than {} s and was stopped",
                    h.command,
                    limit.as_secs()
                ));
                continue;
            }
        };
        let said = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
        let (stdout, stderr) = (said(&done.stdout), said(&done.stderr));
        match done.status.code() {
            Some(0) => {
                if let Some(r) = parse_reply(&stdout) {
                    out.context.extend(r.context.filter(|c| !c.trim().is_empty()));
                    if let Some(u) = r.updated {
                        for key in ["args", "tool_input"] {
                            if payload.get(key).is_some() {
                                payload[key] = u.clone();
                            }
                        }
                        out.updated_input = Some(u);
                    }
                    match r.decision.as_deref() {
                        Some("deny" | "block") if can_block(event) => {
                            let why = r.reason.filter(|w| !w.trim().is_empty());
                            out.blocked = Some(why.unwrap_or_else(|| format!("`{}` said no", h.command)));
                            return out;
                        }
                        Some("ask") => out.decision = Some(Decision::Ask),
                        Some("allow" | "approve") if out.decision != Some(Decision::Ask) => {
                            out.decision = Some(Decision::Allow)
                        }
                        _ => {}
                    }
                } else if stdout_is_context(event) && !stdout.is_empty() {
                    out.context.push(stdout);
                }
            }
            Some(2) if can_block(event) => {
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

/// The fields of a tool event: nexo's names (`tool`, `args`) and the names other tools use.
pub fn tool_fields(name: &str, args: &Value) -> Value {
    json!({"tool": name, "args": args, "tool_name": name, "tool_input": args})
}

/// `/hooks`: the loaded hooks, one line each, with where they come from.
pub fn describe(hooks: &[HookConfig]) -> String {
    if hooks.is_empty() {
        return "no hooks loaded (see docs/HOOKS.md; set them in [[hooks]] of config.toml or in a trusted \
                project's .claude/settings.json)"
            .into();
    }
    let mut lines = vec![format!("{} hook(s):", hooks.len())];
    for h in hooks {
        let event =
            h.event_name().map(str::to_string).unwrap_or_else(|| format!("{} (unknown event, never runs)", h.event));
        let matcher =
            h.matcher.as_deref().filter(|m| !m.trim().is_empty()).map(|m| format!(" [{m}]")).unwrap_or_default();
        let timeout = h.timeout.map(|t| format!(" ({t} s)")).unwrap_or_default();
        let source = if h.source.is_empty() { "nexo config" } else { h.source.as_str() };
        lines.push(format!("  {event}{matcher}{timeout}: {}  <- {source}", h.command));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn hook(event: &str, matcher: Option<&str>, command: &str) -> HookConfig {
        HookConfig {
            event: event.into(),
            matcher: matcher.map(String::from),
            command: command.into(),
            ..Default::default()
        }
    }

    #[test]
    fn matchers() {
        assert!(matches(None, Some("bash")) && matches(Some("*"), None));
        assert!(matches(Some("bash"), Some("bash")) && !matches(Some("bash"), Some("read")));
        assert!(matches(Some("web_*"), Some("web_fetch")) && matches(Some("edit|write"), Some("write")));
        // regexes match the whole name
        assert!(matches(Some("mcp__.*"), Some("mcp__git__log")) && !matches(Some("mcp__.*"), Some("x_mcp__a")));
        assert!(matches(Some("(read|grep)"), Some("grep")) && !matches(Some("re"), Some("read")));
        assert!(matches(Some("a.c"), Some("abc")) && !matches(Some("a.c"), Some("abcd")));
        // a pattern that is no regex must equal the name
        assert!(matches(Some("a("), Some("a(")) && !matches(Some("a("), Some("a")));
        assert!(!matches(Some("bash"), None));
    }

    #[test]
    fn event_spellings_agree() {
        for (said, canonical) in [
            ("pre_tool", "pre_tool"),
            ("PreToolUse", "pre_tool"),
            ("post-tool-use", "post_tool"),
            ("PostToolUse", "post_tool"),
            ("UserPromptSubmit", "user_prompt_submit"),
            ("SubagentStop", "subagent_stop"),
            ("turn_end", "turn_end"),
            ("Stop", "stop"),
        ] {
            assert_eq!(nexo_core::config::canonical_event(said), Some(canonical), "{said}");
        }
        assert_eq!(nexo_core::config::canonical_event("Whatever"), None);
        assert_eq!(long_name("pre_tool"), "PreToolUse");
        assert_eq!(long_name("session_start"), "SessionStart");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_event_gets_the_session_the_folder_and_its_name_on_stdin() {
        let d = tempfile::tempdir().unwrap();
        let hooks: Vec<HookConfig> =
            nexo_core::config::HOOK_EVENTS.iter().map(|e| hook(e, None, &format!("cat > seen-{e}.json"))).collect();
        for e in nexo_core::config::HOOK_EVENTS {
            let fields = json!({"extra": e});
            let out = run(&hooks, e, Some("bash"), &fields, d.path(), "sess-1").await;
            assert_eq!(out, Outcome::default(), "{e}");
            let seen: Value =
                serde_json::from_str(&std::fs::read_to_string(d.path().join(format!("seen-{e}.json"))).unwrap())
                    .unwrap();
            assert_eq!(seen["session_id"], "sess-1", "{e}");
            assert_eq!(seen["event"], *e);
            assert_eq!(seen["hook_event_name"], long_name(e));
            assert_eq!(seen["cwd"], d.path().to_string_lossy().as_ref());
            assert_eq!(seen["extra"], *e);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pre_tool_hook_blocks_with_its_reason_and_others_report_failures() {
        let d = tempfile::tempdir().unwrap();
        let hooks = vec![
            hook("pre_tool", Some("bash"), "grep -q 'rm -rf' && { echo 'no rm -rf here' >&2; exit 2; } || exit 0"),
            hook("post_tool", None, "cat > seen.json"),
            hook("turn_end", None, "exit 3"),
        ];
        let call = |c: &str| tool_fields("bash", &json!({"command": c}));
        let blocked = run(&hooks, "pre_tool", Some("bash"), &call("rm -rf /"), d.path(), "s").await;
        assert_eq!(blocked.blocked.as_deref(), Some("no rm -rf here"));
        let fine = run(&hooks, "pre_tool", Some("bash"), &call("ls"), d.path(), "s").await;
        assert_eq!(fine, Outcome::default());
        let other = run(&hooks, "pre_tool", Some("read"), &json!({}), d.path(), "s").await;
        assert!(other.blocked.is_none(), "the matcher leaves other tools alone");
        run(&hooks, "post_tool", Some("read"), &json!({"tool": "read", "is_error": false}), d.path(), "s").await;
        assert!(std::fs::read_to_string(d.path().join("seen.json")).unwrap().contains("\"tool\":\"read\""));
        let end = run(&hooks, "turn_end", None, &json!({}), d.path(), "s").await;
        assert_eq!(end.failures, ["hook `exit 3` failed (3)"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn json_decisions_context_and_updated_input() {
        let d = tempfile::tempdir().unwrap();
        let run_one = |cmd: &str, event: &str| {
            let hooks = vec![hook(event, None, cmd)];
            let (path, event) = (d.path().to_path_buf(), event.to_string());
            async move {
                run(&hooks, &event, Some("bash"), &tool_fields("bash", &json!({"command": "ls"})), &path, "s").await
            }
        };
        let deny = run_one(r#"echo '{"decision":"deny","reason":"not today"}'"#, "pre_tool").await;
        assert_eq!(deny.blocked.as_deref(), Some("not today"));
        let allow = run_one(r#"echo '{"decision":"allow","additional_context":"fyi"}'"#, "pre_tool").await;
        assert_eq!((allow.decision, allow.context, allow.blocked), (Some(Decision::Allow), vec!["fyi".into()], None));
        let ask = run_one(r#"echo '{"decision":"ask"}'"#, "pre_tool").await;
        assert_eq!(ask.decision, Some(Decision::Ask));
        let upd = run_one(r#"echo '{"updated_input":{"command":"ls -la"}}'"#, "pre_tool").await;
        assert_eq!(upd.updated_input, Some(json!({"command": "ls -la"})));
        // the spelling other tools use
        let theirs = run_one(
            r#"echo '{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"theirs"}}'"#,
            "pre_tool",
        )
        .await;
        assert_eq!(theirs.blocked.as_deref(), Some("theirs"));
        // a deny means nothing where nothing can be blocked
        let note = run_one(r#"echo '{"decision":"deny"}'"#, "notification").await;
        assert!(note.blocked.is_none());
        // plain text is context for a prompt, and nothing for a tool
        let prompt = run_one("echo today is monday", "user_prompt_submit").await;
        assert_eq!(prompt.context, ["today is monday"]);
        assert!(run_one("echo hi", "pre_tool").await.context.is_empty());
        // exit 2 where nothing can be blocked is a failure, not a block
        let late = run_one("echo oops >&2; exit 2", "session_end").await;
        assert!(late.blocked.is_none() && late.failures[0].contains("failed (2): oops"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_later_hook_sees_the_input_an_earlier_one_replaced() {
        let d = tempfile::tempdir().unwrap();
        let hooks = vec![
            hook("pre_tool", None, r#"echo '{"updated_input":{"command":"echo safe"}}'"#),
            hook("pre_tool", None, "cat > second.json"),
        ];
        let out =
            run(&hooks, "pre_tool", Some("bash"), &tool_fields("bash", &json!({"command": "rm x"})), d.path(), "s")
                .await;
        assert_eq!(out.updated_input, Some(json!({"command": "echo safe"})));
        let seen = std::fs::read_to_string(d.path().join("second.json")).unwrap();
        assert!(seen.contains("echo safe") && !seen.contains("rm x"), "{seen}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hook_that_never_reads_a_large_payload_is_stopped_at_the_limit() {
        let d = tempfile::tempdir().unwrap();
        let mut h = hook("turn_end", None, "sleep 30");
        h.timeout = Some(1);
        let payload = json!({"big": "x".repeat(1 << 20)});
        let start = std::time::Instant::now();
        let out = run(&[h], "turn_end", None, &payload, d.path(), "s").await;
        assert!(start.elapsed() < Duration::from_secs(10), "it waited {:?}", start.elapsed());
        assert_eq!(out.failures, ["hook `sleep 30` took longer than 1 s and was stopped"]);
    }

    #[test]
    fn hooks_are_listed_with_their_source() {
        assert!(describe(&[]).contains("no hooks"));
        let mut h = HookConfig {
            event: "PreToolUse".into(),
            matcher: Some("bash".into()),
            command: "x".into(),
            ..Default::default()
        };
        h.source = "project .claude/settings.json".into();
        let odd = HookConfig { event: "nope".into(), command: "y".into(), timeout: Some(5), ..Default::default() };
        let text = describe(&[h, odd]);
        assert!(text.contains("pre_tool [bash]: x  <- project .claude/settings.json"), "{text}");
        assert!(text.contains("nope (unknown event, never runs) (5 s): y  <- nexo config"), "{text}");
    }
}
