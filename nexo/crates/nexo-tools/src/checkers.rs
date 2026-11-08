//! Fallback checkers for `diagnostics` when no usable language server exists:
//! `cargo check` for Rust crates and `node --check` for JavaScript. Each one
//! runs in its own process group, with a hard time limit, and is killed whole.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

pub(crate) fn program_ok(name: &str) -> bool {
    nexo_core::shell::find_program(name).is_some()
}

/// Hard limit of a fallback checker.
pub const LIMIT: Duration = Duration::from_secs(60);

/// What a fallback checker found: diagnostics in the language-server shape.
pub type Found = Result<Vec<Value>, String>;

/// Runs the checker that fits `language`, if any applies to this project.
/// `None`: no checker applies (say so at once).
pub async fn check(file: &Path, root: &Path, language: &str, limit: Duration) -> Option<Found> {
    match language {
        "rust" if root.join("Cargo.toml").is_file() && program_ok("cargo") => {
            Some(cargo_check(file, root, limit).await)
        }
        "javascript" if program_ok("node") && is_plain_js(file) => Some(node_check(file, root, limit).await),
        _ => None,
    }
}

fn is_plain_js(file: &Path) -> bool {
    matches!(file.extension().and_then(|e| e.to_str()), Some("js" | "mjs" | "cjs"))
}

/// Runs a command to its end or the limit; kills the whole group on the limit.
pub(crate) async fn run(mut cmd: tokio::process::Command, limit: Duration) -> Result<(String, String, bool), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(env) = crate::child_env() {
        cmd.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("cannot start the checker: {e}"))?;
    let pid = child.id();
    let mut out = child.stdout.take().ok_or("no stdout")?;
    let mut err = child.stderr.take().ok_or("no stderr")?;
    let work = async {
        let (mut o, mut e) = (String::new(), String::new());
        let (a, b) = tokio::join!(
            async {
                let mut buf = Vec::new();
                let _ = out.read_to_end(&mut buf).await;
                o = String::from_utf8_lossy(&buf).into_owned();
            },
            async {
                let mut lines = BufReader::new(&mut err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if e.len() < 8000 {
                        e.push_str(&l);
                        e.push('\n');
                    }
                }
            }
        );
        let _ = (a, b);
        let ok = child.wait().await.map(|s| s.success()).unwrap_or(false);
        (o, e, ok)
    };
    match tokio::time::timeout(limit, work).await {
        Ok(r) => Ok(r),
        Err(_) => {
            crate::bash::kill_group(pid);
            Err(format!("the checker did not finish in {} s", limit.as_secs()))
        }
    }
}

async fn cargo_check(file: &Path, root: &Path, limit: Duration) -> Found {
    let mut cmd = tokio::process::Command::new("cargo");
    cmd.args(["check", "--message-format=json", "--quiet", "--all-targets"]).current_dir(root);
    let (out, err, ok) = run(cmd, limit).await?;
    let rel = file.strip_prefix(root).unwrap_or(file).to_string_lossy().replace('\\', "/");
    let mut found = Vec::new();
    let mut parsed_any = false;
    for line in out.lines() {
        let Ok(m) = serde_json::from_str::<Value>(line) else { continue };
        if m["reason"] != "compiler-message" {
            continue;
        }
        parsed_any = true;
        let msg = &m["message"];
        let level = msg["level"].as_str().unwrap_or("");
        let sev = match level {
            "error" | "error: internal compiler error" => 1,
            "warning" => 2,
            _ => continue,
        };
        let Some(span) = msg["spans"].as_array().and_then(|s| {
            s.iter().find(|s| {
                s["is_primary"] == true && s["file_name"].as_str().map(|f| f.replace('\\', "/")) == Some(rel.clone())
            })
        }) else {
            continue;
        };
        let line_no = span["line_start"].as_u64().unwrap_or(1).saturating_sub(1);
        let col = span["column_start"].as_u64().unwrap_or(1).saturating_sub(1);
        found.push(json!({
            "range": {"start": {"line": line_no, "character": col}},
            "severity": sev,
            "message": msg["message"],
            "source": "cargo check",
        }));
    }
    found.dedup();
    if !ok && !parsed_any {
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("cargo check failed");
        return Err(format!("cargo check failed: {first}"));
    }
    Ok(found)
}

async fn node_check(file: &Path, root: &Path, limit: Duration) -> Found {
    let mut cmd = tokio::process::Command::new("node");
    cmd.arg("--check").arg(file).current_dir(root);
    let (_, err, ok) = run(cmd, limit).await?;
    if ok {
        return Ok(Vec::new());
    }
    // node prints `path:LINE`, the source line, a caret line, then `SyntaxError: message`
    let line =
        err.lines().next().and_then(|l| l.rsplit(':').next()).and_then(|n| n.trim().parse::<u64>().ok()).unwrap_or(1);
    let message = err.lines().find(|l| l.contains("Error")).unwrap_or("syntax error").trim().to_string();
    Ok(vec![json!({
        "range": {"start": {"line": line.saturating_sub(1), "character": 0}},
        "severity": 1,
        "message": message,
        "source": "node --check",
    })])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn node_syntax_error_and_clean_file() {
        if !program_ok("node") {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let bad = d.path().join("bad.js");
        std::fs::write(&bad, "let a = 1;\nlet = = 2;\n").unwrap();
        let r = check(&bad, d.path(), "javascript", LIMIT).await.unwrap().unwrap();
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0]["range"]["start"]["line"], 1);
        let good = d.path().join("good.js");
        std::fs::write(&good, "let a = 1;\nconsole.log(a);\n").unwrap();
        assert!(check(&good, d.path(), "javascript", LIMIT).await.unwrap().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_checker_without_a_project() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.rs");
        std::fs::write(&f, "fn main() {}").unwrap();
        assert!(check(&f, d.path(), "rust", LIMIT).await.is_none());
        assert!(check(&f, d.path(), "python", LIMIT).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_set_environment_replaces_the_process_own() {
        std::env::set_var("NEXO_TOOLS_TEST_TOKEN", "token-marker");
        let env: crate::Env = std::sync::Arc::new(vec![
            ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
            ("ONLY".into(), "1".into()),
        ]);
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", "env"]);
        let (out, _, ok) = crate::CHILD_ENV.scope(env, run(c, LIMIT)).await.unwrap();
        assert!(ok && out.contains("ONLY=1") && !out.contains("token-marker"), "{out}");
    }

    #[tokio::test]
    async fn the_limit_kills_the_group() {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", "sleep 30 & sleep 30"]);
        let t = std::time::Instant::now();
        let r = run(c, Duration::from_millis(500)).await;
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
