//! Part of the agent loop; see the parent module.

use super::*;

/// What the permission dialog shows for a call.
pub(super) fn detail_of(kind: ToolKind, args: &Value, cwd: &Path) -> ToolDisplay {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    match kind {
        ToolKind::Exec => ToolDisplay::Shell {
            command: s("command").or_else(|| s("cmd")).unwrap_or_default(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        },
        ToolKind::Write => {
            let path = s("path").or_else(|| s("file_path")).unwrap_or_default();
            let file = if Path::new(&path).is_absolute() { PathBuf::from(&path) } else { cwd.join(&path) };
            if let (Some(old), Some(new)) = (s("old_string"), s("new_string")) {
                // only the replaced text is shown: where it sits in the file numbers the diff
                let start_line = line_in_file(&file, &old);
                ToolDisplay::Diff { path, old, new, start_line }
            } else if args.get("files").is_some() {
                // several files: the dialog names each one
                ToolDisplay::List { items: nexo_core::tool::call_paths(args) }
            } else if args.get("edits").is_some() {
                // several changes: the whole file as they would leave it
                let old = std::fs::read_to_string(&file).unwrap_or_default();
                match nexo_tools::edit::changes(args).and_then(|c| nexo_tools::edit::apply(&old, &c, &path)) {
                    Ok((new, _)) => ToolDisplay::Diff { path, old, new, start_line: 1 },
                    Err(_) => ToolDisplay::Plain,
                }
            } else if let Some(content) = s("content") {
                let old = std::fs::read_to_string(&file).unwrap_or_default();
                ToolDisplay::Diff { path, old, new: content, start_line: 1 }
            } else {
                ToolDisplay::Plain
            }
        }
        // a read of several files: every file the dialog asks about
        ToolKind::Read if nexo_core::tool::has_paths(args) => {
            ToolDisplay::List { items: nexo_core::tool::call_paths(args) }
        }
        // the whole address, query included: what it would carry out is the person's call
        ToolKind::Network => match s("url").or_else(|| s("uri")) {
            Some(u) => ToolDisplay::List { items: vec![u] },
            None => ToolDisplay::Plain,
        },
        _ => ToolDisplay::Plain,
    }
}

/// The most of a file read to find where a text sits in it (the edit tool takes as much).
pub(super) const LINE_LOOKUP_MAX: u64 = 32 * 1024 * 1024;

/// The line (counted from 1) of the file at `path` where `snippet` first appears; 1 when the
/// file cannot be read, is not a plain file within `LINE_LOOKUP_MAX` (a preview must not wait
/// on a pipe or read a huge file) or does not hold the text.
pub(super) fn line_in_file(path: &Path, snippet: &str) -> usize {
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= LINE_LOOKUP_MAX) {
        return 1;
    }
    let Ok(bytes) = std::fs::read(path) else { return 1 };
    line_of(&String::from_utf8_lossy(&bytes), snippet)
}

/// The line (counted from 1) where `snippet` first appears in `text`; 1 if it does not.
pub(super) fn line_of(text: &str, snippet: &str) -> usize {
    let (text, snippet) = (text.replace("\r\n", "\n"), snippet.replace("\r\n", "\n"));
    text.find(&snippet).map_or(1, |at| text[..at].matches('\n').count() + 1)
}

/// The person was asked and allowed the call.
pub(super) const ASKED: Why = Why { decision: "asked", by: "user" };

pub(super) enum Verdict {
    Run,
    Denied(String),
    /// A PreToolUse hook stopped the call.
    Blocked(String),
}

/// Whether a call may run: the policy decides, or the person is asked, with what a PreToolUse hook decided:
/// `ask` asks the person even if a rule would let the call run, `allow` runs a call that would have been
/// asked about. The second value says who decided, for the audit log.
#[allow(clippy::too_many_arguments)]
pub(super) async fn permit_why(
    state: &Shared,
    events: &EventTx,
    gate: &Arc<dyn PermissionGate>,
    cwd: &Path,
    id: &str,
    name: &str,
    tool: &dyn Tool,
    args: &Value,
    cancel: &CancellationToken,
    hook: Option<crate::hooks::Decision>,
    agent: Option<&str>,
) -> (Verdict, Why) {
    let kind = tool.kind();
    let mut decision = {
        let st = state.lock().await;
        let d = st.perm.decide(name, kind, args, cwd);
        // a command the OS sandbox holds in needs no asking (a rule that denies still wins)
        let boxed = tool.sandbox_holds(args, &st.sandbox);
        if d == Decision::Ask && boxed {
            Decision::Allow
        } else {
            d
        }
    };
    let mut by_hook = false;
    match (hook, &decision) {
        // a PreToolUse hook that wants the person asked, or lets the call run unasked; a deny rule stands
        (Some(crate::hooks::Decision::Ask), Decision::Allow) => decision = Decision::Ask,
        (Some(crate::hooks::Decision::Allow), Decision::Ask) => {
            decision = Decision::Allow;
            by_hook = true;
        }
        _ => {}
    }
    match decision {
        Decision::Allow if by_hook => (Verdict::Run, Why { decision: "allowed", by: "hook" }),
        Decision::Allow => (Verdict::Run, Why::RULE),
        Decision::Deny(r) => (Verdict::Denied(r), Why::denied("rule")),
        Decision::Ask => {
            // PermissionRequest hooks answer before the person is asked: allow, deny, or leave it to them
            let summary = tool.summary(args);
            let mut fields = crate::hooks::tool_fields(name, args);
            if let Some(a) = agent {
                fields["agent"] = Value::from(a);
            }
            let o = fire(state, "permission_request", Some(name), fields).await;
            if let Some(why) = o.blocked {
                return (Verdict::Denied(why), Why::denied("hook"));
            }
            if o.decision == Some(crate::hooks::Decision::Allow) {
                return (Verdict::Run, Why { decision: "allowed", by: "hook" });
            }
            notify_hooks(state, "permission_prompt", format!("nexo needs your permission to use {name}: {summary}"));
            let req = PermissionRequest {
                id: id.to_string(),
                tool: name.to_string(),
                kind,
                summary,
                detail: detail_of(kind, args, cwd),
                suggested_rules: permission::suggested_rules(name, kind, args, cwd),
            };
            let d = tokio::select! {
                d = gate.ask(req) => d,
                _ = cancel.cancelled() => PermissionDecision::Deny { reason: Some("interrupted by the person".into()) },
            };
            emit(events, AgentEvent::PermissionResolved { id: id.to_string(), decision: d.clone() });
            match d {
                PermissionDecision::Allow => (Verdict::Run, ASKED),
                PermissionDecision::AllowAlways { rule, persist } => {
                    state.lock().await.perm.add_allow(&rule);
                    if persist {
                        match nexo_core::paths::project_dir(cwd) {
                            Some(dir) => {
                                if let Err(e) = permission::persist_rule(&dir, &rule) {
                                    notice(events, Level::Warn, format!("cannot save the rule: {e}"));
                                } else if let Some(root) = dir.parent().filter(|r| {
                                    !nexo_core::config::is_trusted(r, &nexo_core::paths::trusted_projects_file())
                                }) {
                                    notice(
                                        events,
                                        Level::Info,
                                        format!(
                                            "saved; the allow rules of a project apply in later sessions once \
                                             you trust it: nexo config trust \"{}\"",
                                            root.display()
                                        ),
                                    );
                                }
                            }
                            None => notice(
                                events,
                                Level::Warn,
                                "no project directory to save the rule in (only for this session)",
                            ),
                        }
                    }
                    (Verdict::Run, ASKED)
                }
                PermissionDecision::Deny { reason } => {
                    (Verdict::Denied(reason.unwrap_or_else(|| "denied by the person".into())), Why::denied("user"))
                }
            }
        }
    }
}

#[cfg(test)]
mod detail_tests {
    use super::*;
    use serde_json::json;

    fn start_of(d: &ToolDisplay) -> usize {
        match d {
            ToolDisplay::Diff { start_line, .. } => *start_line,
            other => panic!("not a diff: {other:?}"),
        }
    }

    #[test]
    fn an_edit_preview_starts_at_the_line_of_the_text_it_replaces() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "one\ntwo\r\nthree\nfour\n").unwrap();
        let call = |old: &str| json!({"path": "a.rs", "old_string": old, "new_string": "x"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &call("three\nfour"), d.path())), 3);
        assert_eq!(start_of(&detail_of(ToolKind::Write, &call("two\r\nthree"), d.path())), 2);
        // a text the file does not hold, or a file that is not there: from the top
        assert_eq!(start_of(&detail_of(ToolKind::Write, &call("zzz"), d.path())), 1);
        let gone = json!({"path": "nope.rs", "old_string": "a", "new_string": "b"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &gone, d.path())), 1);
        // what is not a plain file, or is past the size the edit tool takes, is not read
        let dir = json!({"path": ".", "old_string": "a", "new_string": "b"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &dir, d.path())), 1);
        let mut big = std::fs::File::create(d.path().join("big.log")).unwrap();
        std::io::Write::write_all(&mut big, b"a\nb\nTARGET\n").unwrap();
        big.set_len(LINE_LOOKUP_MAX + 1).unwrap(); // sparse
        let target = json!({"path": "big.log", "old_string": "TARGET", "new_string": "b"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &target, d.path())), 1);
        // a whole file written over another is shown whole
        let whole = json!({"path": "a.rs", "content": "new"});
        assert_eq!(start_of(&detail_of(ToolKind::Write, &whole, d.path())), 1);
    }
}
