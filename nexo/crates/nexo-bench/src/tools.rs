//! The tools the model gets in the bench: nexo's own local tools, with every output capped and,
//! in a sealed task, commands sealed and paths kept inside the working directory.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::{
    PermissionDecision, PermissionGate, PermissionRequest, Tool, ToolContext, ToolKind, ToolOutput, ToolSet, ToolSpec,
};
use serde_json::Value;

use crate::seal::Seal;

/// The most characters of a tool's output the model reads.
pub const CAP: usize = 30_000;

/// `text` cut to its first and last `CAP / 2` characters, saying how much was left out.
pub fn cap(text: &str) -> String {
    let n = text.chars().count();
    if n <= CAP {
        return text.to_string();
    }
    let head: String = text.chars().take(CAP / 2).collect();
    let tail: String = text.chars().skip(n - CAP / 2).collect();
    format!("{head}\n... [{} characters truncated] ...\n{tail}", n - CAP)
}

/// The permission gate of a bench run: everything is allowed, nobody is there to ask.
pub struct AllowAll;

#[async_trait]
impl PermissionGate for AllowAll {
    async fn ask(&self, _req: PermissionRequest) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

/// The local tools for a run in `wd`; sealed when `seal` is given.
pub fn toolset(wd: &Path, seal: Option<Seal>) -> ToolSet {
    toolset_with(wd, seal, None, false)
}

/// The local tools named in `only` (every one when None) for a run in `wd`: sealed when `seal` is
/// given, their paths kept inside `wd` when sealed or `confine`.
pub fn toolset_with(wd: &Path, seal: Option<Seal>, only: Option<&[&str]>, confine: bool) -> ToolSet {
    let seal = seal.map(Arc::new);
    let local = nexo_tools::local();
    let mut set = ToolSet::new();
    for name in local.names() {
        if only.is_some_and(|o| !o.contains(&name)) {
            continue;
        }
        let inner = local.get(name).cloned().expect("a listed tool");
        let confine = confine || seal.is_some();
        set.add(Arc::new(Bench { inner, wd: wd.to_path_buf(), seal: seal.clone(), confine }));
    }
    set
}

/// One of nexo's tools as the bench runs it.
struct Bench {
    inner: Arc<dyn Tool>,
    wd: PathBuf,
    seal: Option<Arc<Seal>>,
    /// Paths outside `wd` are refused.
    confine: bool,
}

impl Bench {
    /// Why `args` reach outside the working directory, if they do. The path filter of each search
    /// tool is checked (glob's `pattern`, grep's `glob`); grep's `pattern` is a regex, not a path.
    fn escapes(&self, args: &Value, cwd: &Path) -> Option<String> {
        if let Some(p) = args.get("path").and_then(Value::as_str) {
            if !inside(&self.wd, &cwd.join(p)) {
                return Some(p.to_string());
            }
        }
        let key = match self.inner.name() {
            "glob" => "pattern",
            "grep" => "glob",
            _ => return None,
        };
        args.get(key)
            .and_then(Value::as_str)
            .filter(|p| p.starts_with('/') || p.starts_with('\\') || p.contains("..") || p.contains(':'))
            .map(String::from)
    }
}

#[async_trait]
impl Tool for Bench {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }
    fn kind(&self) -> ToolKind {
        self.inner.kind()
    }
    fn summary(&self, args: &Value) -> String {
        self.inner.summary(args)
    }
    async fn run(&self, mut args: Value, ctx: &ToolContext) -> ToolOutput {
        if self.seal.is_some() || self.confine {
            if let Some(p) = self.escapes(&args, &ctx.cwd) {
                return ToolOutput::error(format!(
                    "{p}: this folder is a sealed box, nothing outside it can be reached"
                ));
            }
        }
        if let Some(seal) = &self.seal {
            if let Some(cmd) = args.get("command").and_then(Value::as_str).filter(|_| self.inner.name() == "bash") {
                args["command"] = Value::String(seal.wrap(cmd));
            }
        }
        let mut out = self.inner.run(args, ctx).await;
        out.content = cap(&out.content);
        out
    }
}

/// True when `p` (absolute) stays inside `root` once `.` and `..` are resolved and, where it
/// exists, its symlinks followed.
fn inside(root: &Path, p: &Path) -> bool {
    let mut norm = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                norm.pop();
            }
            Component::CurDir => {}
            c => norm.push(c),
        }
    }
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut real = norm.clone();
    // the nearest existing ancestor decides, so a link inside the folder cannot point out of it
    let mut rest = Vec::new();
    while !real.exists() {
        match (real.file_name().map(|f| f.to_os_string()), real.parent()) {
            (Some(f), Some(parent)) => {
                rest.push(f);
                real = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut real = real.canonicalize().unwrap_or(real);
    real.extend(rest.into_iter().rev());
    real.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_output_keeps_its_ends_and_says_what_was_cut() {
        let text = format!("{}{}", "a".repeat(20_000), "b".repeat(20_000));
        let c = cap(&text);
        assert!(c.starts_with(&"a".repeat(15_000)) && c.ends_with(&"b".repeat(15_000)));
        assert!(c.contains("[10000 characters truncated]"));
        assert_eq!(cap("short"), "short");
    }

    #[test]
    fn paths_that_climb_out_of_the_folder_are_outside() {
        let wd = tempfile::tempdir().unwrap();
        let root = wd.path().canonicalize().unwrap();
        assert!(inside(&root, &root.join("src/lib.rs")));
        assert!(inside(&root, &root.join("a/../b")));
        assert!(!inside(&root, &root.join("../elsewhere")));
        assert!(!inside(&root, &std::env::temp_dir().join("x")) || std::env::temp_dir().starts_with(&root));
    }

    #[tokio::test]
    async fn every_local_tool_is_offered_and_outputs_are_capped() {
        let wd = tempfile::tempdir().unwrap();
        std::fs::write(wd.path().join("big.txt"), "x".repeat(50_000)).unwrap();
        let set = toolset(wd.path(), None);
        assert_eq!(set.len(), nexo_tools::local().len());
        let read = set.get("read").unwrap();
        let out = read.run(serde_json::json!({"path": "big.txt"}), &ToolContext::new(wd.path().to_path_buf())).await;
        assert!(out.content.chars().count() <= CAP + 100);
    }

    #[tokio::test]
    async fn a_confined_toolset_keeps_only_its_tools_and_lets_grep_search_any_regex() {
        let wd = tempfile::tempdir().unwrap();
        let root = wd.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "key: value\n").unwrap();
        let set = toolset_with(&root, None, Some(&["read", "grep", "glob", "ls"]), true);
        assert_eq!(set.len(), 4);
        assert!(set.get("bash").is_none() && set.get("write").is_none());
        let ctx = ToolContext::new(root.clone());
        let grep = set.get("grep").unwrap();
        let found = grep.run(serde_json::json!({"pattern": "key: .."}), &ctx).await;
        assert!(!found.content.contains("sealed box"), "{}", found.content);
        let out = grep.run(serde_json::json!({"pattern": "x", "glob": "../*"}), &ctx).await;
        assert!(out.content.contains("sealed box"));
        let glob = set.get("glob").unwrap();
        assert!(glob.run(serde_json::json!({"pattern": "/etc/*"}), &ctx).await.content.contains("sealed box"));
        let read = set.get("read").unwrap();
        assert!(read.run(serde_json::json!({"path": "../x"}), &ctx).await.content.contains("sealed box"));
    }
}
