//! The supervisor's rules: `supervisor.toml` in the nexo folder (the person's), and `.nexo/supervisor.toml` in a
//! project, which can only tighten them. [`Policy::allows`] decides whether a pending tool call may be approved
//! on the person's behalf: a deny rule always wins over an approve rule, and a short fixed list of dangerous
//! calls is refused whatever the files say.

use std::path::Path;

use globset::Glob;
use serde_json::Value;

mod danger;
mod paths;
mod shell;
#[cfg(test)]
mod tests;
mod types;

use danger::dangerous;
pub use danger::{catastrophic, catastrophic_from};
use paths::{
    bash_command, expand_home, glob, home_dir, is_write_tool, matches, normalize, outside, path_args, resolve,
};
use shell::{compound, words};
pub use types::*;

impl Policy {
    /// A policy out of TOML text. Fails on a syntax error, an unknown key or a bad glob.
    pub fn parse(text: &str) -> Result<Policy, String> {
        let p: Policy = toml::from_str(text).map_err(|e| e.to_string())?;
        p.validate()?;
        Ok(p)
    }

    fn validate(&self) -> Result<(), String> {
        let globs = [&self.approve, &self.deny].into_iter().flat_map(|r| r.tools.iter().chain(&r.bash).chain(&r.paths));
        for g in globs.chain(self.agents.iter().map(|a| &a.pattern)) {
            Glob::new(g).map_err(|e| format!("bad pattern {g:?}: {e}"))?;
        }
        Ok(())
    }

    /// The person's file, then the project's, which only tightens. A file that is missing counts for nothing; a
    /// file that cannot be read or parsed is left out and named in `errors`, the rest still applies.
    pub fn load(user: Option<&Path>, project: Option<&Path>) -> Loaded {
        let mut out = Loaded::default();
        let mut read = |path: Option<&Path>| -> Option<(Policy, toml::Table)> {
            let path = path?;
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
                Err(e) => {
                    out.errors.push(format!("{}: {e}", path.display()));
                    return None;
                }
            };
            let parsed = Policy::parse(&text).and_then(|p| {
                let said: toml::Table = toml::from_str(&text).map_err(|e| e.to_string())?;
                Ok((p, said))
            });
            match parsed {
                Ok(v) => Some(v),
                Err(e) => {
                    out.errors.push(format!("{}: {e}", path.display()));
                    None
                }
            }
        };
        let user = read(user);
        let project = read(project);
        if let Some((p, _)) = user {
            out.policy = p;
        }
        if let Some((p, said)) = project {
            out.policy.merge_stricter(&p, &said);
        }
        out
    }

    /// Fold a project's policy in. `said` is the project's file as a table, to tell what it set from what it
    /// left at the default. Only what makes the supervisor do less or ask more is taken: it may switch the
    /// supervisor off, slow it, shrink its budget and nudges, narrow the approvals, add denials, forbid
    /// answering and pause sessions. It cannot change thresholds, texts, the model or the alert command (that
    /// one runs a program), nor undo anything the person's file says.
    pub fn merge_stricter(&mut self, project: &Policy, said: &toml::Table) {
        let has = |path: &[&str]| {
            let mut t = said;
            for (i, k) in path.iter().enumerate() {
                match t.get(*k) {
                    Some(toml::Value::Table(inner)) if i + 1 < path.len() => t = inner,
                    Some(_) => return i + 1 == path.len(),
                    None => return false,
                }
            }
            false
        };
        if has(&["enabled"]) {
            self.enabled &= project.enabled;
        }
        if has(&["interval_secs"]) {
            self.interval_secs = self.interval_secs.max(project.interval_secs);
        }
        if let (Some(m), Some(p)) = (self.model.as_mut(), project.model.as_ref()) {
            if has(&["model", "max_usd_per_hour"]) {
                m.max_usd_per_hour = m.max_usd_per_hour.min(p.max_usd_per_hour);
            }
            if has(&["model", "max_calls_per_hour"]) {
                m.max_calls_per_hour = m.max_calls_per_hour.min(p.max_calls_per_hour);
            }
            if has(&["model", "max_input_tokens"]) {
                m.max_input_tokens = m.max_input_tokens.min(p.max_input_tokens);
            }
        }
        if has(&["nudge", "answer_questions"]) {
            self.nudge.answer_questions &= project.nudge.answer_questions;
        }
        if has(&["nudge", "nudges_per_hour"]) {
            self.nudge.nudges_per_hour = self.nudge.nudges_per_hour.min(project.nudge.nudges_per_hour);
        }
        if has(&["nudge", "nudge_cooldown_min"]) {
            self.nudge.nudge_cooldown_min = self.nudge.nudge_cooldown_min.max(project.nudge.nudge_cooldown_min);
        }
        // Approvals narrow: only what both files list survives.
        for (mine, theirs, key) in [
            (&mut self.approve.tools, &project.approve.tools, "tools"),
            (&mut self.approve.bash, &project.approve.bash, "bash"),
            (&mut self.approve.paths, &project.approve.paths, "paths"),
        ] {
            if has(&["approve", key]) {
                mine.retain(|g| theirs.contains(g));
            }
        }
        // Denials only grow.
        for (mine, theirs) in [
            (&mut self.deny.tools, &project.deny.tools),
            (&mut self.deny.bash, &project.deny.bash),
            (&mut self.deny.paths, &project.deny.paths),
        ] {
            for g in theirs {
                if !mine.contains(g) {
                    mine.push(g.clone());
                }
            }
        }
        // Per-session rules: only the restrictive half of each is kept.
        for r in &project.agents {
            let rule = AgentRule {
                pattern: r.pattern.clone(),
                answer_questions: r.answer_questions.filter(|v| !*v),
                paused: r.paused.filter(|v| *v),
            };
            if rule.answer_questions.is_some() || rule.paused.is_some() {
                self.agents.push(rule);
            }
        }
    }

    /// What applies to the session called `name`, working in `cwd`.
    pub fn for_agent(&self, name: &str, cwd: &str) -> ForAgent {
        let mut out = ForAgent { answer_questions: self.nudge.answer_questions, paused: false };
        for r in &self.agents {
            let Some(g) = glob(&r.pattern, false) else { continue };
            if g.is_match(name) || g.is_match(cwd) {
                if let Some(v) = r.answer_questions {
                    out.answer_questions = v;
                }
                if let Some(v) = r.paused {
                    out.paused = v;
                }
            }
        }
        out
    }

    /// May the call `tool(args)` of a session working in `cwd` be approved for the person?
    pub fn allows(&self, tool: &str, args: &Value, cwd: &Path) -> Verdict {
        self.allows_in(tool, args, cwd, home_dir().as_deref())
    }

    /// [`Policy::allows`] with the home folder given (tests).
    pub fn allows_in(&self, tool: &str, args: &Value, cwd: &Path, home: Option<&Path>) -> Verdict {
        let cwd = normalize(cwd, Path::new("/"));
        let command = bash_command(tool, args);
        let paths = path_args(args);

        // 1. What is never approved.
        if let Some(cmd) = command {
            if let Some(why) = dangerous(cmd, &cwd, home) {
                return Verdict::Deny(why.into());
            }
        }
        if is_write_tool(tool) {
            for p in &paths {
                if outside(p, &cwd, home) {
                    return Verdict::Deny(format!("writes outside the session folder: {p}"));
                }
            }
        }

        // 2. The person's denials win over any approval.
        if let Some(g) = self.deny.tools.iter().find(|g| matches(g, tool, false)) {
            return Verdict::Deny(format!("tool denied by {g:?}"));
        }
        if let Some(cmd) = command {
            if let Some(g) = self.deny.bash.iter().find(|g| matches(g, cmd, false)) {
                return Verdict::Deny(format!("command denied by {g:?}"));
            }
        }
        let mut named: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        if let Some(cmd) = command {
            named.extend(words(cmd));
        }
        for p in &named {
            let abs = resolve(p, &cwd, home);
            for g in &self.deny.paths {
                let g = expand_home(g, home);
                if matches(&g, p, true) || matches(&g, &abs, true) {
                    return Verdict::Deny(format!("path denied by {g:?}: {p}"));
                }
            }
        }

        // 3. What the person listed as safe.
        if let Some(cmd) = command {
            if !compound(cmd) && self.approve.bash.iter().any(|g| matches(g, cmd, false)) {
                return Verdict::Approve;
            }
        }
        if self.approve.tools.iter().any(|g| matches(g, tool, false)) {
            let narrowed = !self.approve.paths.is_empty() && !paths.is_empty();
            let inside = paths.iter().all(|p| {
                let abs = resolve(p, &cwd, home);
                self.approve.paths.iter().any(|g| {
                    let g = expand_home(g, home);
                    matches(&g, p, true) || matches(&g, &abs, true)
                })
            });
            if !narrowed || inside {
                return Verdict::Approve;
            }
        }
        Verdict::Pass
    }
}
