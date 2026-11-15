//! The managed policy: a file an administrator controls and the user cannot override.
//!
//! It is read last and wins over the user file, the project file, `.mcp.json`, the environment and
//! the command line. A user may tighten what it sets and never loosen it. A file that exists but
//! cannot be read or understood stops nexo from starting: a policy that fails open is no policy.
//! See `docs/MANAGED-POLICY.md` for every key and an example.

#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
use crate::config::{Config, HookConfig, McpServerConfig};
#[cfg(test)]
use crate::permission::PermissionMode;
#[cfg(test)]
use crate::sandbox::SandboxMode;

mod apply;
mod glob;
mod model;
mod path;

pub use apply::*;
pub use glob::*;
pub use model::*;
pub use path::*;

#[cfg(test)]
mod tests;
