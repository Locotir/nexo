//! The `.mcp.json` project file other harnesses use:
//!
//! ```json
//! { "mcpServers": {
//!     "files":  { "command": "npx", "args": ["-y", "server"], "env": { "KEY": "${KEY}" } },
//!     "remote": { "type": "http", "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer ${TOKEN}" } },
//!     "legacy": { "type": "sse", "url": "https://example.com/sse" } } }
//! ```
//!
//! A project's file is read only when the project is trusted (`nexo config trust`), like the
//! servers of its `.nexo/config.toml`, since a server entry runs a command.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::config::McpServerConfig;

/// The servers of a `.mcp.json` text. An entry that is not an object, or has neither a command nor
/// an address, is left out.
pub fn parse(text: &str) -> Result<BTreeMap<String, McpServerConfig>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    let Some(servers) = v.get("mcpServers").and_then(Value::as_object) else {
        return Ok(out);
    };
    for (name, s) in servers {
        let Some(s) = s.as_object() else { continue };
        let text = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_string);
        let map = |k: &str| -> BTreeMap<String, String> {
            s.get(k)
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| match v {
                            Value::String(x) => Some((k.clone(), x.clone())),
                            Value::Number(_) | Value::Bool(_) => Some((k.clone(), v.to_string())),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut cfg = McpServerConfig::default();
        if let Some(url) = text("url").filter(|u| !u.is_empty()) {
            cfg.url = Some(url);
            cfg.headers = map("headers");
            // `sse` is the old transport; `http`, `streamable-http` and no type are the new one
            cfg.transport = Some(if text("type").as_deref() == Some("sse") { "sse" } else { "http" }.into());
        } else if let Some(command) = text("command").filter(|c| !c.is_empty()) {
            cfg.command = command;
            cfg.args = s
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            cfg.env = map("env");
            cfg.cwd = text("cwd").map(Into::into);
        } else {
            continue;
        }
        if s.get("disabled").and_then(Value::as_bool) == Some(true) {
            cfg.enabled = Some(false);
        }
        out.insert(name.clone(), cfg);
    }
    Ok(out)
}

/// Reads `<root>/.mcp.json` into `servers`: the servers the configuration already has stay as they
/// are. A file of a project that is not in `trust_file` is not applied; the returned notice says so.
pub fn apply(servers: &mut BTreeMap<String, McpServerConfig>, root: &Path, trusted: bool) -> Option<String> {
    let file = root.join(".mcp.json");
    let text = std::fs::read_to_string(&file).ok()?;
    let found = match parse(&text) {
        Ok(f) => f,
        Err(e) => return Some(format!("{} cannot be read: {e}", file.display())),
    };
    if found.is_empty() {
        return None;
    }
    if !trusted {
        let names: Vec<_> = found.keys().cloned().collect();
        return Some(format!(
            "{} is not trusted, so its MCP servers are ignored: {}. If you trust this project, run \
             `nexo config trust \"{}\"` and start again.",
            file.display(),
            names.join(", "),
            root.display()
        ));
    }
    for (name, cfg) in found {
        servers.entry(name).or_insert(cfg);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"{"mcpServers": {
        "files": {"command": "npx", "args": ["-y", "srv"], "env": {"K": "${K}"}},
        "remote": {"type": "http", "url": "https://x.test/mcp", "headers": {"Authorization": "Bearer ${T}"}},
        "old": {"type": "sse", "url": "https://x.test/sse"},
        "bare": {"url": "https://x.test/m"},
        "off": {"command": "x", "disabled": true},
        "empty": {}, "junk": 3}}"#;

    #[test]
    fn both_shapes_are_read() {
        let s = parse(TEXT).unwrap();
        assert_eq!(s.len(), 5);
        assert_eq!(s["files"].command, "npx");
        assert_eq!(s["files"].args, ["-y", "srv"]);
        assert_eq!(s["files"].env["K"], "${K}");
        assert_eq!(s["remote"].url.as_deref(), Some("https://x.test/mcp"));
        assert_eq!(s["remote"].headers["Authorization"], "Bearer ${T}");
        assert_eq!(s["remote"].transport.as_deref(), Some("http"));
        assert_eq!(s["old"].transport.as_deref(), Some("sse"));
        assert_eq!(s["bare"].transport.as_deref(), Some("http"));
        assert_eq!(s["off"].enabled, Some(false));
        assert!(parse("not json").is_err());
        assert!(parse("{}").unwrap().is_empty());
    }

    #[test]
    fn an_untrusted_project_adds_nothing_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), TEXT).unwrap();
        let mut servers = BTreeMap::new();
        let note = apply(&mut servers, dir.path(), false).unwrap();
        assert!(servers.is_empty());
        assert!(note.contains("nexo config trust"), "{note}");
        let mut own = BTreeMap::new();
        own.insert("files".to_string(), McpServerConfig { command: "mine".into(), ..Default::default() });
        assert!(apply(&mut own, dir.path(), true).is_none());
        assert_eq!(own.len(), 5);
        assert_eq!(own["files"].command, "mine", "the configuration's own server wins");
    }
}
