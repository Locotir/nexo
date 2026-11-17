use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use super::running::servers;
use super::server::Server;
use super::table::{root_for, servers_for, usable};
use crate::checkers;
use crate::util::{display_path, resolve, str_arg};
/// The diagnostics as lines a person (and a model) reads: `path:line:col severity message (source)`.
pub fn format(shown: &str, diags: &[Value]) -> String {
    if diags.is_empty() {
        return format!("{shown}: no problems found");
    }
    let mut rows: Vec<(u64, u64, u64, String)> = diags
        .iter()
        .map(|d| {
            let line = d.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0) + 1;
            let col = d.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0) + 1;
            let sev = d["severity"].as_u64().unwrap_or(1);
            let word = match sev {
                1 => "error",
                2 => "warning",
                3 => "info",
                _ => "hint",
            };
            let msg = d["message"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string();
            let src = d["source"].as_str().map(|s| format!(" ({s})")).unwrap_or_default();
            (sev, line, col, format!("{shown}:{line}:{col} {word}: {msg}{src}"))
        })
        .collect();
    rows.sort();
    let errors = rows.iter().filter(|r| r.0 == 1).count();
    let warnings = rows.iter().filter(|r| r.0 == 2).count();
    let mut out = format!(
        "{shown}: {errors} error{}, {warnings} warning{}\n",
        if errors == 1 { "" } else { "s" },
        if warnings == 1 { "" } else { "s" }
    );
    out.push_str(&rows.into_iter().map(|r| r.3).collect::<Vec<_>>().join("\n"));
    out
}

pub struct Diagnostics;

impl Diagnostics {
    /// The server failed: a compiler check if one applies, else the server's own error.
    async fn fallback(&self, file: &Path, root: &Path, language: &str, shown: &str, why: String) -> ToolOutput {
        match checkers::check(file, root, language, checkers::LIMIT).await {
            Some(Ok(d)) => ToolOutput::ok(format(shown, &d)),
            Some(Err(e)) => ToolOutput::error(format!("diagnostics: {why}; {e}")),
            None => ToolOutput::error(format!("diagnostics: {why}; run the compiler or the tests with bash instead")),
        }
    }
}

#[async_trait]
impl Tool for Diagnostics {
    fn name(&self) -> &str {
        "diagnostics"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "diagnostics".into(),
            description: "What the language server says of a file: compile errors, type errors, warnings, with \
                line and column. Edit and write results carry the compiler errors of the file they changed, \
                or the project's own check runs after the step and brings them; this is for other files, for \
                warnings, or when no check came back. Works for \
                Rust, Python, TypeScript/JavaScript, Go and C/C++ when their language server is installed; the \
                first call in a project may take a while as the server indexes it."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "The file."}},
                "required": ["path"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Language-server errors and warnings of a file. Edit and write results carry the changed file's compiler \
                errors, or the project check runs after the step; use this for other files and warnings.",
            params: &[],
        })
    }

    fn rare(&self) -> bool {
        true
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn summary(&self, args: &Value) -> String {
        format!("diagnostics {}", str_arg(args, "path").unwrap_or(""))
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(p) = str_arg(&args, "path") else { return ToolOutput::error("`path` is required") };
        let file = resolve(&ctx.cwd, p);
        let shown = display_path(&ctx.cwd, &file);
        if !file.is_file() {
            return ToolOutput::error(format!("diagnostics: {shown} is not a file"));
        }
        let Some((language, candidates)) = servers_for(&file) else {
            return ToolOutput::error(format!(
                "diagnostics: no language server for {shown} (Rust, Python, TypeScript/JavaScript, Go, C/C++); run the compiler or the tests with bash instead"
            ));
        };
        let root = root_for(&file, &ctx.cwd);
        let Some(cmd) = candidates.iter().find(|c| usable(c)) else {
            // no server: a quick checker of the project kind, or an answer at once
            ctx.report(format!("no {language} language server: using the compiler"));
            return match checkers::check(&file, &root, language, checkers::LIMIT).await {
                Some(Ok(d)) => ToolOutput::ok(format(&shown, &d)),
                Some(Err(e)) => ToolOutput::error(format!("diagnostics: {e}")),
                None => {
                    let names: Vec<&str> = candidates.iter().map(|c| c[0]).collect();
                    ToolOutput::error(format!(
                        "diagnostics: no checker applies to {shown}: no {language} language server is installed or working ({}); run the compiler or the tests with bash instead",
                        names.join(" or ")
                    ))
                }
            };
        };
        ctx.report(format!("asking {} in {}", cmd[0], display_path(&ctx.cwd, &root)));
        // a server started with another environment is another server
        let env = crate::child_env().map(|e| format!(" {e:?}")).unwrap_or_default();
        let key = (format!("{}{env}", cmd.join(" ")), root.clone());
        let server = {
            let mut table = servers().lock().await;
            match table.get(&key) {
                Some(s) => s.clone(),
                None => match Server::start(cmd, &root).await {
                    Ok(s) => {
                        let s = Arc::new(s);
                        table.insert(key.clone(), s.clone());
                        s
                    }
                    Err(e) => return self.fallback(&file, &root, language, &shown, e).await,
                },
            }
        };
        let checked = tokio::select! {
            r = server.check(&file, language) => r,
            _ = ctx.cancel.cancelled() => return ToolOutput::error("interrupted by the person"),
        };
        match checked {
            Ok(d) => ToolOutput::ok(format(&shown, &d)),
            Err(e) => {
                // a server that failed is started afresh next time
                servers().lock().await.remove(&key);
                self.fallback(&file, &root, language, &shown, e).await
            }
        }
    }
}
