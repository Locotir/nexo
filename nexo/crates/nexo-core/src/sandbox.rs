//! The sandbox settings of a session: what the `bash` tool may touch.
//!
//! This file holds only the settings and the list of places a sandboxed command may write. The OS
//! code that enforces them lives in `nexo-tools` (`sandbox.rs`), behind `cfg` gates per platform.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Commands run with the rights of the person (the permission rules are the only guard).
    #[default]
    Off,
    /// Commands read the whole disk but write only the project, the added directories, the temp
    /// directory and the caches of the language tools.
    WorkspaceWrite,
    /// Commands read the whole disk and write nothing but the temp directory and `/dev`.
    ReadOnly,
}

impl SandboxMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SandboxMode::Off => "off",
            SandboxMode::WorkspaceWrite => "workspace-write",
            SandboxMode::ReadOnly => "read-only",
        }
    }

    pub fn parse(s: &str) -> Option<SandboxMode> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "off" | "none" | "false" | "no" => Some(SandboxMode::Off),
            "workspace-write" | "workspace" | "on" | "true" | "yes" => Some(SandboxMode::WorkspaceWrite),
            "read-only" | "readonly" => Some(SandboxMode::ReadOnly),
            _ => None,
        }
    }

    pub fn is_on(&self) -> bool {
        *self != SandboxMode::Off
    }
}

/// The sandbox of a session as the tools see it, copied into each call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SandboxSettings {
    pub mode: SandboxMode,
    /// Whether a sandboxed command may use the network.
    pub network: bool,
    /// Directories besides the project that may be written (`--add-dir` and `sandbox_write_dirs`).
    pub extra_dirs: Vec<PathBuf>,
}

/// Caches of the language tools, relative to the home directory. A command that builds or installs
/// writes there, so they stay writable in `workspace-write`. Missing ones are skipped.
pub const CACHE_DIRS: &[&str] = &[
    ".cargo/registry",
    ".cargo/git",
    ".npm",
    ".cache/pip",
    ".cache/go-build",
    ".cache/yarn",
    ".cache/pnpm",
    ".local/share/pnpm",
    ".gradle",
    ".m2/repository",
    ".nuget/packages",
    "go/pkg",
];

/// Places outside the home that are always writable (device nodes and shared memory).
pub const SYSTEM_WRITE_DIRS: &[&str] = &["/dev"];

impl SandboxSettings {
    pub fn new(mode: SandboxMode, network: bool, extra_dirs: Vec<PathBuf>) -> SandboxSettings {
        SandboxSettings { mode, network, extra_dirs }
    }

    /// The directories a sandboxed command may write, as paths that exist (a root that does not
    /// exist cannot be named to the OS, and is skipped).
    pub fn write_roots(&self, cwd: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut add = |p: PathBuf| {
            let p = p.canonicalize().unwrap_or(p);
            if p.is_dir() && !out.contains(&p) {
                out.push(p);
            }
        };
        for d in SYSTEM_WRITE_DIRS {
            add(PathBuf::from(d));
        }
        // the system temp directory, and the usual places a tool looks for one
        add(std::env::temp_dir());
        if cfg!(unix) {
            add(PathBuf::from("/tmp"));
            add(PathBuf::from("/var/tmp"));
        }
        if self.mode == SandboxMode::WorkspaceWrite {
            add(cwd.to_path_buf());
            for d in &self.extra_dirs {
                add(if d.is_absolute() { d.clone() } else { cwd.join(d) });
            }
            if let Some(home) = dirs::home_dir() {
                for c in CACHE_DIRS {
                    add(home.join(c));
                }
            }
            // where the build puts its output when the person moved it out of the project
            for var in ["CARGO_TARGET_DIR", "CARGO_HOME"] {
                if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                    let p = PathBuf::from(v);
                    // CARGO_HOME holds the toolchain bin too: only its caches are opened
                    if var == "CARGO_HOME" {
                        add(p.join("registry"));
                        add(p.join("git"));
                    } else {
                        add(p);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_and_print() {
        for m in [SandboxMode::Off, SandboxMode::WorkspaceWrite, SandboxMode::ReadOnly] {
            assert_eq!(SandboxMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(SandboxMode::parse("on"), Some(SandboxMode::WorkspaceWrite));
        assert_eq!(SandboxMode::parse("Workspace_Write"), Some(SandboxMode::WorkspaceWrite));
        assert_eq!(SandboxMode::parse("sideways"), None);
    }

    #[test]
    fn roots_hold_the_project_only_in_workspace_write() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().canonicalize().unwrap();
        let ws = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![]);
        assert!(ws.write_roots(&cwd).contains(&cwd));
        let ro = SandboxSettings::new(SandboxMode::ReadOnly, false, vec![]);
        assert!(!ro.write_roots(&cwd).contains(&cwd));
    }

    #[test]
    fn added_dirs_count_only_when_they_exist() {
        let d = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let s = SandboxSettings::new(
            SandboxMode::WorkspaceWrite,
            false,
            vec![extra.path().to_path_buf(), d.path().join("missing")],
        );
        let roots = s.write_roots(d.path());
        assert!(roots.contains(&extra.path().canonicalize().unwrap()));
        assert!(!roots.iter().any(|r| r.ends_with("missing")));
    }
}
