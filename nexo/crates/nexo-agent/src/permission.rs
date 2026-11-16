//! Who may run what: the mode, the rules, and what to suggest when asking.
//!
//! A rule is `tool` (any call of it) or `tool(glob)`, where the glob is
//! matched against the call's *target*: the command for `bash`
//! (`bash(git *)`, `bash(cargo test*)`), the path for file tools
//! (`edit(src/**)`, `read(/etc/*)`), and the tool name for the rest
//! (`mcp__server__*`). `deny` beats `allow` beats the mode.

use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use nexo_core::config::PermissionsConfig;
use nexo_core::permission::PermissionMode;
use nexo_core::tool::ToolKind;
use serde_json::Value;

mod bash;
mod paths;
mod rules;
mod suggest;

#[cfg(test)]
mod flag_tests;
#[cfg(test)]
mod note_scope_tests;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub mode: PermissionMode,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// Tools that touch nothing outside the conversation: never asked about (a `deny` rule still holds).
pub use nexo_core::tool::catalog::ALWAYS_ALLOWED;

use paths::editable;
use rules::names_a_path;
use suggest::is_flag;

pub use paths::is_sensitive;
pub use rules::{host_of, rule_matches, rule_matches_as, target_of};
pub use suggest::{flag_words, persist_rule, suggested_rules, with_flags};

/// Tools that read by running the project's own code (a language server or compiler runs its build
/// scripts and macros): asked about like a command, never run where only reads may.
pub const RUNS_PROJECT_CODE: &[&str] = &["diagnostics"];

impl Policy {
    pub fn from_config(cfg: &PermissionsConfig) -> Policy {
        Policy { mode: cfg.mode, allow: cfg.allow.clone(), deny: cfg.deny.clone() }
    }

    pub fn decide(&self, tool: &str, kind: ToolKind, args: &Value, cwd: &Path) -> Decision {
        // a call of several files is judged file by file, as if each were asked for alone: one denied
        // file denies the call, one that needs asking asks for the call
        let many = match kind {
            ToolKind::Read | ToolKind::Search => nexo_core::tool::has_paths(args),
            ToolKind::Write => args.get("files").is_some_and(|f| f.as_array().is_some_and(|a| !a.is_empty())),
            _ => false,
        };
        let mut ask = false;
        if many {
            for p in nexo_core::tool::call_paths(args) {
                let mut one = args.clone();
                if let Some(o) = one.as_object_mut() {
                    o.remove("paths");
                    o.remove("files");
                    o.remove("check");
                    o.insert("path".into(), Value::String(p));
                }
                match self.decide_one(tool, kind, &one, cwd) {
                    Decision::Deny(why) => return Decision::Deny(why),
                    Decision::Ask => ask = true,
                    Decision::Allow => {}
                }
            }
        } else {
            match self.decide_one(tool, kind, args, cwd) {
                Decision::Deny(why) => return Decision::Deny(why),
                Decision::Ask => ask = true,
                Decision::Allow => {}
            }
        }
        // the command an edit or a write asks to run after it is judged as a command of its own
        if kind == ToolKind::Write {
            if let Some(c) = args.get("check").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
                match self.decide_one("bash", ToolKind::Exec, &serde_json::json!({"command": c}), cwd) {
                    Decision::Deny(why) => return Decision::Deny(format!("check command: {why}")),
                    Decision::Ask => ask = true,
                    Decision::Allow => {}
                }
            }
        }
        if ask {
            Decision::Ask
        } else {
            Decision::Allow
        }
    }

    fn decide_one(&self, tool: &str, kind: ToolKind, args: &Value, cwd: &Path) -> Decision {
        // what would wipe the machine beyond doubt runs in no mode and by no rule
        if kind == ToolKind::Exec {
            if let Some(why) = target_of(kind, args).as_deref().and_then(nexo_supervise::policy::catastrophic) {
                return Decision::Deny(format!("never run: {why}"));
            }
        }
        if let Some(r) = self.deny.iter().find(|r| rule_matches_as(r, tool, kind, args, cwd, true)) {
            return Decision::Deny(format!("denied by rule `{r}`"));
        }
        // a note that saves a rule is a write to the instructions file: decided like one
        if tool == "note" {
            let scope = args.get("scope").and_then(Value::as_str).unwrap_or("session");
            if let Some(file) = nexo_core::compat::rules_path(scope, cwd) {
                let file = serde_json::json!({"path": file.to_string_lossy()});
                return self.decide("edit", ToolKind::Write, &file, cwd);
            }
        }
        if ALWAYS_ALLOWED.contains(&tool) || (tool == "ultracode_execute" && self.mode == PermissionMode::Plan) {
            return Decision::Allow;
        }
        if self.mode == PermissionMode::Plan && matches!(tool, "task" | "workflow") {
            return Decision::Deny("plan mode".into());
        }
        // reading a web page changes nothing here: read-only mode still asks for it
        let reads = matches!(kind, ToolKind::Read | ToolKind::Search | ToolKind::Network);
        // A plan must not start a workflow before the person approves it. Read-only mode still
        // permits workflows subject to their normal approval and per-call permissions.
        if self.mode.reads_only() && !reads && !(tool == "workflow" && self.mode == PermissionMode::ReadOnly) {
            return Decision::Deny(format!("{} mode", self.mode.as_str()));
        }
        // it reads, but by starting the project's own build tools (build scripts, macros, plugins),
        // which can do anything a command can: not where only reads run, asked about as a command is
        if RUNS_PROJECT_CODE.contains(&tool) {
            if self.mode.reads_only() {
                return Decision::Deny(format!("{} mode: {tool} starts the project's build tools", self.mode.as_str()));
            }
            if self.mode != PermissionMode::Auto && !self.allow.iter().any(|r| rule_matches(r, tool, kind, args, cwd)) {
                return Decision::Ask;
            }
        }
        // a command that outlives nexo is never covered by an allow rule or a mode: each one asks
        // (yolo, which asks about nothing, keeps its meaning)
        if kind == ToolKind::Exec && self.mode != PermissionMode::Auto && is_flag(args, "detach") {
            return Decision::Ask;
        }
        // writing into a program under the debugger, or attaching to a process, asks every time
        if kind == ToolKind::Exec
            && self.mode != PermissionMode::Auto
            && nexo_tools::bash::changes_a_program(tool, args)
        {
            return Decision::Ask;
        }
        // keys and credentials are read only when asked, or by a rule that names their path
        if matches!(kind, ToolKind::Read | ToolKind::Search)
            && self.mode != PermissionMode::Auto
            && target_of(kind, args).is_some_and(|p| is_sensitive(&p, cwd))
        {
            let named = self.allow.iter().any(|r| names_a_path(r) && rule_matches(r, tool, kind, args, cwd));
            return if named { Decision::Allow } else { Decision::Ask };
        }
        if self.allow.iter().any(|r| rule_matches(r, tool, kind, args, cwd)) {
            return Decision::Allow;
        }
        match (self.mode, kind) {
            (PermissionMode::Auto, _) => Decision::Allow,
            (_, ToolKind::Read | ToolKind::Search) => Decision::Allow,
            (PermissionMode::AcceptEdits, ToolKind::Write) if editable(args, cwd) => Decision::Allow,
            _ => Decision::Ask,
        }
    }

    /// `decide`, and with the OS sandbox on a command that asks to run outside it (`unsandboxed`)
    /// is never allowed by a rule or a mode: it asks. A `deny` still denies; yolo still allows.
    /// With the sandbox off the flag changes nothing, so it adds no question.
    pub fn decide_boxed(&self, tool: &str, kind: ToolKind, args: &Value, cwd: &Path, sandbox_on: bool) -> Decision {
        match self.decide(tool, kind, args, cwd) {
            Decision::Allow if sandbox_on && self.leaves_the_sandbox(kind, args) => Decision::Ask,
            d => d,
        }
    }

    fn leaves_the_sandbox(&self, kind: ToolKind, args: &Value) -> bool {
        kind == ToolKind::Exec && self.mode != PermissionMode::Auto && is_flag(args, "unsandboxed")
    }

    pub fn add_allow(&mut self, rule: &str) {
        if !self.allow.iter().any(|r| r == rule) {
            self.allow.push(rule.to_string());
        }
    }
}
