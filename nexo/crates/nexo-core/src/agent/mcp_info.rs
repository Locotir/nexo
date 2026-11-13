use super::*;

/// Where an MCP server stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    Connected,
    /// A remote server that answered 401 and has no login that works.
    NeedsLogin,
    /// Could not start or reach; the reason.
    Failed(String),
    /// Turned off (in the configuration or for this session).
    Disabled,
}

/// One argument of an MCP prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpArgInfo {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

/// One prompt of an MCP server (it becomes the command `/mcp__<server>__<name>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPromptInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub args: Vec<McpArgInfo>,
}

/// One MCP server as the `/mcp` window and the completion of `@server:` and `/mcp__` see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    /// `stdio`, `http` or `sse`.
    pub transport: String,
    pub status: McpStatus,
    pub tools: usize,
    /// The URIs of the listed resources.
    #[serde(default)]
    pub resources: Vec<String>,
    /// The URI templates of the listed resource templates.
    #[serde(default)]
    pub templates: Vec<String>,
    #[serde(default)]
    pub prompts: Vec<McpPromptInfo>,
}
