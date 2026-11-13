//! The bubblewrap fallback, forced with NEXO_SANDBOX_BACKEND. Skipped where bwrap is missing or
//! cannot make a sandbox (no user namespaces).
#![cfg(target_os = "linux")]

use nexo_core::sandbox::{SandboxMode, SandboxSettings};
use nexo_core::{Tool, ToolContext};
use nexo_tools::bash::Bash;
use serde_json::json;

#[path = "../../nexo-agent/tests/scratch/mod.rs"]
mod scratch;

#[tokio::test]
async fn bwrap_holds_writes_to_the_project() {
    std::env::set_var("NEXO_SANDBOX_BACKEND", "bwrap");
    let ok = std::process::Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--dev-bind", "/dev", "/dev", "true"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipped: bwrap is missing or cannot make a sandbox here");
        return;
    }
    let p = tempfile::tempdir().unwrap();
    let outside = tempfile::Builder::new().prefix(".nexo-sandbox-test-").tempdir_in(dirs::home_dir().unwrap()).unwrap();
    // the scratch folder a command without a session gets lives until the process exits
    scratch::at_exit(|| nexo_tools::bash::wipe_tmp(""));
    let mut ctx = ToolContext::new(p.path().to_path_buf());
    ctx.sandbox = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![]);
    let o = Bash.run(json!({"command": "echo hi > in.txt"}), &ctx).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(p.path().join("in.txt").is_file());
    let cmd = format!("echo no > {}/x.txt", outside.path().display());
    let o = Bash.run(json!({"command": cmd}), &ctx).await;
    assert!(o.is_error && o.content.contains("[sandbox:"), "{}", o.content);
}
