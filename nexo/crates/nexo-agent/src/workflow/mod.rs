//! Workflows: a small TOML file of phases and steps; nexo runs the sub-agents it describes, in order and within
//! its ceilings, and gives the main agent one answer.
//!
//! - [`file`]: the format, and every check made before anything starts.
//! - [`run`]: the runner. It starts sub-agents with [`crate::subagent::SubAgent::launch`], the code the `task`
//!   tool starts its own with.
//! - [`tool`]: the `workflow` tool.

pub mod file;
mod run;
pub mod tool;

pub use tool::Workflow;
