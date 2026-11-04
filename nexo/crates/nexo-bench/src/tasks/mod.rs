//! The tasks. Each one is a small project with a trap that long-horizon agents typically fall
//! into: a rule given at the start, a needle in noisy output, a detail needed much later, a change
//! of plan, a misleading error, a value from an earlier session, a long history, a reverse
//! engineering job whose facts are spread over hundreds of steps.
//!
//! Every task ships with a reference solution, so its checker is tested too.

use std::path::Path;

use crate::check::Outcome;

mod inventory;
pub mod padded;
pub mod realm;
mod realm_game;
mod small;

/// Builds what a task's text files cannot carry, in its working directory.
pub type Setup = fn(&Path) -> Result<(), String>;

/// One benchmark task.
pub struct Task {
    pub id: &'static str,
    /// What it tests about the context.
    pub trap: &'static str,
    /// The group `--tasks` can name instead of listing ids: core, long, more or hard.
    pub group: &'static str,
    /// The files of the working directory: `(relative path, content)`.
    pub files: Vec<(String, String)>,
    /// `(session, user message)`; a new session number starts a new session that resumes the
    /// previous one's saved history.
    pub turns: Vec<(u32, String)>,
    /// Grades the working directory once the agent is done.
    pub check: fn(&Path) -> Outcome,
    /// Writes the reference solution into a materialized working directory.
    pub solution: fn(&Path),
    /// Builds what text files cannot carry (a compiled binary).
    pub setup: Option<Setup>,
    /// Shell commands run sealed: no network, nothing outside the working directory. A sealed task
    /// refuses to run where commands cannot be sealed.
    pub sealed: bool,
    /// Programs the sealed box lacks (script interpreters, say), by name; empty for all of them.
    pub mask: &'static [&'static str],
    /// Ok when this machine can run the task, else why not.
    pub ready: fn() -> Result<(), String>,
}

impl Task {
    /// Writes the task's files into `wd` and runs its setup.
    pub fn materialize(&self, wd: &Path) -> Result<(), String> {
        for (rel, text) in &self.files {
            let p = wd.join(rel);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
            }
            std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        self.setup.map_or(Ok(()), |f| f(wd))
    }
}

/// Always ready.
pub(crate) fn always() -> Result<(), String> {
    Ok(())
}

/// The groups `--tasks` accepts besides task ids.
pub const GROUPS: [&str; 7] = ["all", "core", "long", "heavy", "more", "hard", "padded"];

/// Every task, in the order they run.
pub fn all() -> Vec<Task> {
    let mut out = small::core();
    out.extend(inventory::tasks());
    out.extend(small::more());
    out.extend(padded::tasks());
    out.push(realm::task());
    out
}

/// The tasks named by `names` (ids or groups, comma lists allowed); every task when empty.
pub fn select<S: AsRef<str>>(names: &[S]) -> Result<Vec<Task>, String> {
    let wanted: Vec<String> = names
        .iter()
        .flat_map(|n| n.as_ref().split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>())
        .filter(|s| !s.is_empty())
        .collect();
    let every = all();
    if wanted.is_empty() || wanted.iter().any(|w| w == "all") {
        return Ok(every);
    }
    for w in &wanted {
        if !GROUPS.contains(&w.as_str()) && !every.iter().any(|t| t.id == w) {
            let ids: Vec<&str> = every.iter().map(|t| t.id).collect();
            return Err(format!("unknown task {w:?}: pick from {} or a group ({})", ids.join(", "), GROUPS.join(", ")));
        }
    }
    Ok(every.into_iter().filter(|t| wanted.iter().any(|w| w == t.id || w == t.group)).collect())
}

/// `(path, text)` pairs from string literals.
pub(crate) fn files(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect()
}

/// A `Cargo.toml` for a mini-project with no dependencies. The empty `[workspace]` keeps cargo
/// from attaching it to a workspace the working directory happens to sit in.
pub(crate) fn manifest(name: &str, extra: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{extra}\n[workspace]\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_ids_are_unique_and_groups_select_them() {
        let every = all();
        let mut ids: Vec<&str> = every.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), every.len());
        assert_eq!(every.len(), 17);
        assert_eq!(select(&["realm"]).unwrap().len(), 1);
        assert_eq!(select(&["core,realm"]).unwrap().len(), 7);
        assert_eq!(select::<&str>(&[]).unwrap().len(), 17);
        assert_eq!(select(&["heavy"]).unwrap().len(), 3);
        assert!(select(&["nope"]).is_err());
    }

    #[test]
    fn the_misleading_error_checker_accepts_its_solution_and_rejects_the_start() {
        let task = all().into_iter().find(|t| t.id == "misleading-error").unwrap();
        let wd = tempfile::tempdir().unwrap();
        task.materialize(wd.path()).unwrap();
        assert!(!(task.check)(wd.path()).ok);
        (task.solution)(wd.path());
        let got = (task.check)(wd.path());
        assert!(got.ok, "{}", got.detail);
        assert_eq!(got.score, 1.0);
    }

    /// Every checker against its own solution, and against the untouched start. Slow: it builds
    /// a dozen small cargo projects (run with `cargo test -p nexo-bench -- --ignored`).
    #[test]
    #[ignore]
    fn every_checker_accepts_its_solution_and_rejects_the_start() {
        let mut bad = Vec::new();
        for task in all() {
            if (task.ready)().is_err() {
                continue;
            }
            let wd = tempfile::tempdir().unwrap();
            task.materialize(wd.path()).unwrap();
            if (task.check)(wd.path()).ok {
                bad.push(format!("{}: the untouched task passes", task.id));
            }
            (task.solution)(wd.path());
            let got = (task.check)(wd.path());
            if !got.ok {
                bad.push(format!("{}: {}", task.id, got.detail));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
