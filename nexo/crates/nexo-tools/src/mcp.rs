//! A client for MCP servers over stdio (JSON-RPC 2.0, one JSON object per
//! line). The server's tools become [`Tool`]s named `mcp__<server>__<name>`.

mod client;
pub mod http;
pub mod hub;
mod names;
pub mod oauth;
mod prompt_args;
#[cfg(test)]
mod tests;
mod types;

pub use client::{McpClient, McpTool, CALL_TIMEOUT, MAX_RESULT_CHARS, PROTOCOL_VERSIONS};
pub use names::full_name;
pub use prompt_args::parse_prompt_args;
pub use types::{PromptArg, PromptDef, PromptText, ResourceDef, TemplateDef, ToolDef};
