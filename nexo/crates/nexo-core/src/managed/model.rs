//! The shape of the managed policy and the error reading it can give.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::HookConfig;
use crate::sandbox::SandboxMode;

#[derive(Debug, thiserror::Error)]
#[error(
    "the managed policy file {} {problem}; nexo does not start without a valid policy. \
     Fix the file or ask your administrator.",
    .file.display()
)]
pub struct ManagedError {
    pub file: PathBuf,
    pub problem: String,
}

/// Glob lists: `*` for any run of characters (also `/`), `?` for one; case does not matter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AllowDeny {
    /// When set, only what matches one of these may be used.
    pub allow: Option<Vec<String>>,
    /// What matches one of these may never be used; this wins over `allow`.
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedPermissions {
    /// Rules that always apply (`bash(rm -rf *)`); a user `allow` rule never beats them.
    pub deny: Vec<String>,
    /// Rules added to the user's own.
    pub allow: Vec<String>,
    /// The least strict sandbox a session may have (`off`, `workspace-write`, `read-only`).
    pub sandbox: Option<SandboxMode>,
    /// With `sandbox`: that mode exactly, not a minimum.
    pub sandbox_forced: bool,
    /// `false`: sandboxed commands never get the network. `true` or unset: the user decides.
    pub sandbox_network: Option<bool>,
    /// The only extra directories a sandboxed command may write; the user's own and `--add-dir` are dropped.
    pub sandbox_write_dirs: Option<Vec<PathBuf>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedMcp {
    /// Server names (globs). When set, a server must match one of these or an `allow_commands` entry.
    pub allow: Option<Vec<String>>,
    pub deny: Vec<String>,
    /// Globs on `command arg1 arg2` of a local server, or on the url of a remote one.
    pub allow_commands: Option<Vec<String>>,
    pub deny_commands: Vec<String>,
}

/// `[audit]` of the policy: `enabled = true` turns the audit log on for everyone and no other file can turn it off.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedAudit {
    pub enabled: bool,
    /// Pins the log file; the user's own `path` is replaced.
    pub path: Option<String>,
}

/// What the user and project levels may bring in. Unset means allowed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedExtensions {
    pub user_plugins: Option<bool>,
    pub project_plugins: Option<bool>,
    pub user_skills: Option<bool>,
    pub project_skills: Option<bool>,
    pub user_commands: Option<bool>,
    pub project_commands: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagedPolicy {
    pub permissions: ManagedPermissions,
    pub providers: AllowDeny,
    /// Globs on `model` and on `provider/model`.
    pub models: AllowDeny,
    pub mcp: ManagedMcp,
    pub allow_user_hooks: Option<bool>,
    pub allow_project_hooks: Option<bool>,
    /// Hooks that always run and that no file of the user's can remove.
    pub forced_hooks: Vec<HookConfig>,
    pub extensions: ManagedExtensions,
    /// Registers neither `web_search` nor `web_fetch`.
    pub disable_web: bool,
    /// The local audit log (docs/AUDIT.md): forced on, and where it is written.
    pub audit: ManagedAudit,
    /// No `yolo` permission mode (`--yolo`, `/mode yolo`, `NEXO_PERMISSION_MODE=yolo`).
    pub disable_bypass_mode: bool,
    /// The file this was read from; never part of the file.
    #[serde(skip)]
    pub file: PathBuf,
}
