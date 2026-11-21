//! Part of the agent loop; see the parent module.

use super::*;

/// What the permission dialog shows for a call.
pub(super) fn detail_of(name: &str, kind: ToolKind, args: &Value, cwd: &Path) -> ToolDisplay {
    if let Some(items) = mcp_items(name, args) {
        return ToolDisplay::List { items };
    }
    if name == "workflow" {
        return ToolDisplay::List { items: crate::workflow::tool::question(args, cwd).1 };
    }
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
                // several files: each one with what it replaces (the paths alone when that cannot be read)
                match replacements(args) {
                    Some(items) => ToolDisplay::List { items },
                    None => ToolDisplay::List { items: nexo_core::tool::call_paths(args) },
                }
            } else if args.get("edits").is_some() {
                // several changes: the whole file as they would leave it
                let old = std::fs::read_to_string(&file).unwrap_or_default();
                match nexo_tools::edit::changes(args).and_then(|c| nexo_tools::edit::apply(&old, &c, &path)) {
                    Ok((new, _)) => ToolDisplay::Diff { path, old, new, start_line: 1 },
                    // a change that does not fit the file: what it asks for is still shown
                    Err(_) => replacements(args).map_or(ToolDisplay::Plain, |items| ToolDisplay::List { items }),
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

/// Longest value of one argument the dialog shows; the rest is counted.
const MCP_ARG_MAX: usize = 4000;

/// An MCP tool call for the dialog: the server and tool, then every argument as `name: value`
/// (nested values as indented JSON). One value over `MCP_ARG_MAX` characters ends with how many are hidden.
fn mcp_items(name: &str, args: &Value) -> Option<Vec<String>> {
    let (server, tool) = name.strip_prefix("mcp__")?.split_once("__")?;
    let show = |v: &Value| {
        let t = match v {
            Value::String(s) => s.clone(),
            Value::Object(_) | Value::Array(_) => serde_json::to_string_pretty(v).unwrap_or_default(),
            other => other.to_string(),
        };
        let n = t.chars().count();
        if n > MCP_ARG_MAX {
            let head: String = t.chars().take(MCP_ARG_MAX).collect();
            format!("{head}... ({} more characters not shown)", n - MCP_ARG_MAX)
        } else {
            t
        }
    };
    let mut items = vec![format!("server: {server}"), format!("tool: {tool}")];
    match args {
        Value::Object(m) => items.extend(m.iter().map(|(k, v)| format!("{k}: {}", show(v)))),
        Value::Null => {}
        other => items.push(format!("arguments: {}", show(other))),
    }
    Some(items)
}

/// Replacements the permission dialog spells out; the rest are counted.
const REPLACEMENTS_SHOWN: usize = 4;

/// The first line of a fragment, with a mark when more follows.
fn first_line(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    match (lines.next(), lines.next().is_some()) {
        (Some(l), true) => format!("{l} …"),
        (Some(l), false) => l.to_string(),
        (None, _) => "(empty)".to_string(),
    }
}

/// What a call of `edit` with `edits` or `files` replaces, as lines for the dialog: each file's path (with
/// its number of changes) and, for each replacement, `- old` and `+ new`; after the first
/// `REPLACEMENTS_SHOWN` a count of what is left. `None` when the call has nothing readable.
pub(super) fn replacements(args: &Value) -> Option<Vec<String>> {
    let path_of = |v: &Value| {
        v.get("path").or_else(|| v.get("file_path")).and_then(Value::as_str).unwrap_or("(no path)").to_string()
    };
    let files: Vec<(String, Vec<nexo_tools::edit::Change>)> = match args.get("files").and_then(Value::as_array) {
        Some(list) => {
            list.iter().map(|f| Some((path_of(f), nexo_tools::edit::changes(f).ok()?))).collect::<Option<Vec<_>>>()?
        }
        None => vec![(path_of(args), nexo_tools::edit::changes(args).ok()?)],
    };
    let total: usize = files.iter().map(|(_, c)| c.len()).sum();
    let mut items = Vec::new();
    let mut shown = 0;
    for (path, changes) in &files {
        if shown >= REPLACEMENTS_SHOWN {
            break;
        }
        let n = changes.len();
        items.push(if n > 1 { format!("{path} ({n} changes)") } else { path.clone() });
        for (old, new, _) in changes.iter().take(REPLACEMENTS_SHOWN - shown) {
            items.push(format!("- {}", first_line(old)));
            items.push(format!("+ {}", first_line(new)));
            shown += 1;
        }
    }
    if total > shown {
        items.push(format!("+{} more", total - shown));
    }
    (!items.is_empty()).then_some(items)
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

/// The exact state under which an internal task was authorized. Equality is
/// deliberately conservative: even a harmless rule or sandbox change invalidates it.
#[derive(Clone, PartialEq)]
pub(super) struct TaskPolicySnapshot {
    policy: Policy,
    sandbox: nexo_core::sandbox::SandboxSettings,
    cwd: PathBuf,
    session: String,
    epoch: u64,
    mode: PermissionMode,
}

impl TaskPolicySnapshot {
    // Caller holds State. Lock order everywhere is State -> PlanCell -> Jobs;
    // never await or invoke hooks while holding either lock.
    pub(super) fn capture(st: &State) -> Self {
        let mode = st.plan.lock().unwrap_or_else(|e| e.into_inner()).mode;
        Self {
            policy: st.perm.clone(),
            sandbox: st.sandbox.clone(),
            cwd: st.cwd.clone(),
            session: st.session.id().to_string(),
            epoch: st.epoch,
            mode,
        }
    }

    fn valid_for(&self, current: &Self, ctx: &ToolContext) -> bool {
        self == current
            && !self.mode.reads_only()
            && self.mode == self.policy.mode
            && self.session == ctx.session_id
            && self.cwd == ctx.cwd
            && !ctx.cancel.is_cancelled()
    }
}

/// Single-use, argument-bound evidence. Only produced by the ordinary permission path.
pub(crate) struct InternalTaskPermit {
    state: Weak<Mutex<State>>,
    snapshot: TaskPolicySnapshot,
    args: Value,
    cancel: CancellationToken,
}

pub(crate) struct TaskSnapshotCheck {
    state: Weak<Mutex<State>>,
    snapshot: TaskPolicySnapshot,
    cancel: CancellationToken,
}

impl TaskSnapshotCheck {
    /// Best-effort check between preparation phases; no lock is held across effects.
    pub(crate) async fn check_snapshot(&self, ctx: &ToolContext) -> Result<(), String> {
        let state = self.state.upgrade().ok_or("session ended")?;
        let st = state.lock().await;
        if self.cancel.is_cancelled() || !self.snapshot.valid_for(&TaskPolicySnapshot::capture(&st), ctx) {
            return Err("task authorization changed during preparation".into());
        }
        Ok(())
    }
}

impl InternalTaskPermit {
    pub(crate) fn snapshot_check(&self) -> TaskSnapshotCheck {
        TaskSnapshotCheck { state: self.state.clone(), snapshot: self.snapshot.clone(), cancel: self.cancel.clone() }
    }

    /// Register under State -> PlanCell -> Jobs, excluding policy and mode writers.
    /// Authorization takes effect at registration; later checks are best effort and
    /// cannot revoke an effect already in progress.
    pub(crate) async fn register(
        self,
        args: &Value,
        ctx: &ToolContext,
        label: String,
    ) -> Result<nexo_core::jobs::JobHandle, String> {
        let state = self.state.upgrade().ok_or("session ended")?;
        let st = state.lock().await;
        let plan = st.plan.lock().unwrap_or_else(|e| e.into_inner());
        let current = TaskPolicySnapshot {
            policy: st.perm.clone(),
            sandbox: st.sandbox.clone(),
            cwd: st.cwd.clone(),
            session: st.session.id().to_string(),
            epoch: st.epoch,
            mode: plan.mode,
        };
        if &self.args != args || self.cancel.is_cancelled() || !self.snapshot.valid_for(&current, ctx) {
            return Err("task authorization changed before registration".into());
        }
        // Use the session's registry, never an untrusted ToolContext registry.
        Ok(st.jobs.start(nexo_core::jobs::JobKind::Agent, label))
    }
}

/// Authorize an internal `task` call using the same hooks and permission path as a tool call.
///
/// This is only a point-in-time verdict, not a launch permit: policy, mode, session and
/// cancellation can change before a job is registered. A caller must synchronize and
/// revalidate those conditions at registration; this function does not launch or audit a job.
pub(super) async fn authorize_internal_task(
    state: &Shared,
    id: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> (Verdict, Why) {
    let (events, gate, cwd, tools) = {
        let st = state.lock().await;
        (st.events.clone(), st.gate.clone(), st.cwd.clone(), st.tools.clone())
    };
    if cancel.is_cancelled() {
        return (Verdict::Denied("interrupted by the person".into()), Why::denied("user"));
    }
    let outcome = fire(state, "pre_tool", Some("task"), crate::hooks::tool_fields("task", args)).await;
    if cancel.is_cancelled() {
        return (Verdict::Denied("interrupted by the person".into()), Why::denied("user"));
    }
    let decision = match internal_task_hook(args, outcome) {
        Ok(decision) => decision,
        Err(verdict) => return verdict,
    };
    let Some(tool) = tools.get("task") else {
        return (Verdict::Denied("unknown tool `task`".into()), Why::denied("rule"));
    };
    let verdict =
        permit_why(state, &events, &gate, &cwd, id, "task", tool.as_ref(), args, cancel, decision, None).await;
    if cancel.is_cancelled() {
        return (Verdict::Denied("interrupted by the person".into()), Why::denied("user"));
    }
    verdict
}

/// Obtain a permit only if the policy observed on both sides of authorization
/// still matches. Rule-only approvals additionally must be allowed by that policy,
/// so a transient permissive policy cannot turn an Ask into an unasked launch.
pub(super) async fn permit_internal_task(
    state: &Shared,
    id: &str,
    args: &Value,
    cancel: &CancellationToken,
    expected: &TaskPolicySnapshot,
) -> Result<InternalTaskPermit, String> {
    let (before, decision) = {
        let st = state.lock().await;
        let snapshot = TaskPolicySnapshot::capture(&st);
        if &snapshot != expected {
            return Err("plan authorization changed before step".into());
        }
        let tool = st.tools.get("task").ok_or("unknown tool `task`")?;
        let d = st.perm.decide_boxed("task", tool.kind(), args, &st.cwd, st.sandbox.mode.is_on());
        (snapshot, d)
    };
    let (verdict, why) = authorize_internal_task(state, id, args, cancel).await;
    if !matches!(verdict, Verdict::Run)
        || matches!(decision, Decision::Deny(_))
        || (why.by == "rule" && decision != Decision::Allow)
    {
        return Err("internal task was not authorized under the current policy".into());
    }
    let after = TaskPolicySnapshot::capture(&*state.lock().await);
    if before != after || cancel.is_cancelled() || before.mode.reads_only() || before.policy.mode != before.mode {
        return Err("task authorization state changed".into());
    }
    Ok(InternalTaskPermit {
        state: Arc::downgrade(state),
        snapshot: before,
        args: args.clone(),
        cancel: cancel.clone(),
    })
}

fn internal_task_hook(
    args: &Value,
    outcome: crate::hooks::Outcome,
) -> Result<Option<crate::hooks::Decision>, (Verdict, Why)> {
    if let Some(reason) = outcome.blocked {
        return Err((Verdict::Blocked(reason), Why::denied("hook")));
    }
    if outcome.updated_input.as_ref().is_some_and(|updated| updated != args) {
        return Err((Verdict::Blocked("PreToolUse changed internal task arguments".into()), Why::denied("hook")));
    }
    Ok(outcome.decision)
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
    let kind = tool.call_kind(args);
    let (mut decision, epoch) = {
        let st = state.lock().await;
        let d = st.perm.decide_boxed(name, kind, args, cwd, st.sandbox.mode.is_on());
        // a command the OS sandbox holds in needs no asking (a rule that denies still wins)
        let boxed = tool.sandbox_holds(args, &st.sandbox);
        let d = if d == Decision::Ask && (boxed || name == "ultracode_execute") { Decision::Allow } else { d };
        (d, st.epoch)
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
            // what the call does beyond its command (outside the sandbox, detached) is said in the request
            let words = permission::flag_words(kind, args, state.lock().await.sandbox.mode.is_on());
            let summary = permission::with_flags(tool.summary(args), name, &words);
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
                agent: agent.map(str::to_string),
                tool: name.to_string(),
                kind,
                summary,
                detail: detail_of(name, kind, args, cwd),
                // a flagged call asks every time: no rule is offered for it
                suggested_rules: if words.is_empty() {
                    permission::suggested_rules(name, kind, args, cwd)
                } else {
                    Vec::new()
                },
            };
            let d = tokio::select! {
                d = gate.ask(req) => d,
                _ = cancel.cancelled() => PermissionDecision::Deny { reason: Some("interrupted by the person".into()) },
            };
            emit(events, AgentEvent::PermissionResolved { id: id.to_string(), decision: d.clone() });
            match d {
                PermissionDecision::Allow => (Verdict::Run, ASKED),
                // a flagged call is allowed this once, whatever the answer says: nothing is remembered for it
                PermissionDecision::AllowAlways { .. } if !words.is_empty() => (Verdict::Run, ASKED),
                PermissionDecision::AllowAlways { rule, persist } => {
                    // the conversation was replaced while the question was open: this call still runs, but the
                    // rule belongs to the conversation that was asked, not to the new one
                    let same = {
                        let mut st = state.lock().await;
                        let same = st.epoch == epoch;
                        if same {
                            st.perm.add_allow(&rule);
                        }
                        same
                    };
                    if persist && same {
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

    fn mcp(args: Value) -> Vec<String> {
        items_of(detail_of("mcp__files__put", ToolKind::Other, &args, Path::new("/")))
    }

    #[test]
    fn an_mcp_call_shows_server_tool_and_each_argument() {
        let items = mcp(json!({"path": "a.txt", "n": 3, "on": true}));
        assert_eq!(items, ["server: files", "tool: put", "path: a.txt", "n: 3", "on: true"]);
        assert_eq!(mcp(Value::Null), ["server: files", "tool: put"]);
    }

    #[test]
    fn an_mcp_value_of_500_characters_is_whole() {
        let long = "x".repeat(500);
        let items = mcp(json!({ "body": long }));
        assert_eq!(items[2], format!("body: {long}"));
    }

    #[test]
    fn an_mcp_nested_object_is_indented_json() {
        let items = mcp(json!({"opts": {"a": [1, 2], "b": "c"}}));
        assert!(items[2].starts_with("opts: {\n  \"a\": ["), "{}", items[2]);
        assert!(items[2].contains("\"b\": \"c\""));
    }

    #[test]
    fn an_mcp_value_over_the_limit_says_how_much_is_hidden() {
        let items = mcp(json!({ "body": "z".repeat(MCP_ARG_MAX + 123) }));
        assert!(items[2].ends_with("... (123 more characters not shown)"), "{}", &items[2][items[2].len() - 50..]);
        assert_eq!(items[2].chars().filter(|c| *c == 'z').count(), MCP_ARG_MAX);
        let exact = mcp(json!({ "body": "z".repeat(MCP_ARG_MAX) }));
        assert!(!exact[2].contains("not shown"));
    }

    #[test]
    fn a_tool_that_is_not_mcp_keeps_its_detail() {
        let d = detail_of("mcp_fake", ToolKind::Other, &json!({"a": 1}), Path::new("/"));
        assert_eq!(d, ToolDisplay::Plain);
    }

    #[test]
    fn an_edit_preview_starts_at_the_line_of_the_text_it_replaces() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "one\ntwo\r\nthree\nfour\n").unwrap();
        let call = |old: &str| json!({"path": "a.rs", "old_string": old, "new_string": "x"});
        assert_eq!(start_of(&detail_of("write", ToolKind::Write, &call("three\nfour"), d.path())), 3);
        assert_eq!(start_of(&detail_of("write", ToolKind::Write, &call("two\r\nthree"), d.path())), 2);
        // a text the file does not hold, or a file that is not there: from the top
        assert_eq!(start_of(&detail_of("write", ToolKind::Write, &call("zzz"), d.path())), 1);
        let gone = json!({"path": "nope.rs", "old_string": "a", "new_string": "b"});
        assert_eq!(start_of(&detail_of("write", ToolKind::Write, &gone, d.path())), 1);
        // what is not a plain file, or is past the size the edit tool takes, is not read
        let dir = json!({"path": ".", "old_string": "a", "new_string": "b"});
        assert_eq!(start_of(&detail_of("write", ToolKind::Write, &dir, d.path())), 1);
        let mut big = std::fs::File::create(d.path().join("big.log")).unwrap();
        std::io::Write::write_all(&mut big, b"a\nb\nTARGET\n").unwrap();
        big.set_len(LINE_LOOKUP_MAX + 1).unwrap(); // sparse
        let target = json!({"path": "big.log", "old_string": "TARGET", "new_string": "b"});
        assert_eq!(start_of(&detail_of("write", ToolKind::Write, &target, d.path())), 1);
        // a whole file written over another is shown whole
        let whole = json!({"path": "a.rs", "content": "new"});
        assert_eq!(start_of(&detail_of("write", ToolKind::Write, &whole, d.path())), 1);
    }
    fn items_of(d: ToolDisplay) -> Vec<String> {
        match d {
            ToolDisplay::List { items } => items,
            other => panic!("not a list: {other:?}"),
        }
    }

    #[test]
    fn several_files_name_each_path_with_what_it_replaces_and_count_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let files = json!({"files": [
            {"path": "a.rs", "old_string": "alpha", "new_string": "beta"},
            {"path": "b.rs", "edits": [
                {"old_string": "one\ntwo", "new_string": "1"},
                {"old_string": "three", "new_string": "3"},
                {"old_string": "four", "new_string": "4"},
                {"old_string": "five", "new_string": "5"},
            ]},
        ]});
        let items = items_of(detail_of("write", ToolKind::Write, &files, d.path()));
        assert_eq!(
            items,
            [
                "a.rs",
                "- alpha",
                "+ beta",
                "b.rs (4 changes)",
                "- one …",
                "+ 1",
                "- three",
                "+ 3",
                "- four",
                "+ 4",
                "+1 more"
            ]
        );
        // a call with nothing readable still lists its paths
        let bad = json!({"files": [{"path": "a.rs"}, {"path": "b.rs"}]});
        assert_eq!(items_of(detail_of("write", ToolKind::Write, &bad, d.path())), ["a.rs", "b.rs"]);
    }

    #[test]
    fn edits_that_do_not_fit_the_file_are_still_spelled_out() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "nothing alike\n").unwrap();
        let call = json!({"path": "a.rs", "edits": [
            {"old_string": "x", "new_string": "y"},
            {"old_string": "z", "new_string": "w"},
        ]});
        let items = items_of(detail_of("write", ToolKind::Write, &call, d.path()));
        assert_eq!(items, ["a.rs (2 changes)", "- x", "+ y", "- z", "+ w"]);
    }
}

#[cfg(test)]
mod internal_task_tests {
    use super::*;
    use crate::hooks::{Decision as HookDecision, Outcome};
    use serde_json::json;

    #[test]
    fn snapshot_rejects_policy_mode_session_and_cancellation_changes() {
        let mut ctx = ToolContext::new(PathBuf::from("/project"));
        ctx.session_id = "s".into();
        let snapshot = TaskPolicySnapshot {
            policy: Policy { mode: PermissionMode::Auto, ..Default::default() },
            sandbox: Default::default(),
            cwd: ctx.cwd.clone(),
            session: ctx.session_id.clone(),
            epoch: 3,
            mode: PermissionMode::Auto,
        };
        assert!(snapshot.valid_for(&snapshot, &ctx));
        let mut changed = snapshot.clone();
        changed.policy.deny.push("task".into());
        assert!(!snapshot.valid_for(&changed, &ctx));
        changed = snapshot.clone();
        changed.mode = PermissionMode::Plan;
        assert!(!snapshot.valid_for(&changed, &ctx));
        changed = snapshot.clone();
        changed.epoch += 1;
        assert!(!snapshot.valid_for(&changed, &ctx));
        changed = snapshot.clone();
        changed.sandbox.mode = nexo_core::sandbox::SandboxMode::ReadOnly;
        assert!(!snapshot.valid_for(&changed, &ctx));
        ctx.session_id = "another".into();
        assert!(!snapshot.valid_for(&snapshot, &ctx));
        ctx.session_id = "s".into();
        ctx.cancel.cancel();
        assert!(!snapshot.valid_for(&snapshot, &ctx));
    }

    #[test]
    fn hook_cannot_replace_approved_task_arguments() {
        let args = json!({"prompt": "original"});
        let changed = Outcome { updated_input: Some(json!({"prompt": "other"})), ..Default::default() };
        assert!(matches!(internal_task_hook(&args, changed), Err((Verdict::Blocked(_), _))));
        let same =
            Outcome { updated_input: Some(args.clone()), decision: Some(HookDecision::Ask), ..Default::default() };
        assert!(matches!(internal_task_hook(&args, same), Ok(Some(HookDecision::Ask))));
        let blocked = Outcome { blocked: Some("no".into()), decision: Some(HookDecision::Allow), ..Default::default() };
        assert!(matches!(internal_task_hook(&args, blocked), Err((Verdict::Blocked(reason), _)) if reason == "no"));
    }
}
