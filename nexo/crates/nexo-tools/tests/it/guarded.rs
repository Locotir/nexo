//! The notice for a sandboxed command that changed a path which runs outside the sandbox later.
//! Runs with whichever backend the machine has (`NEXO_SANDBOX_BACKEND=landlock|bwrap` forces one).
#![cfg(target_os = "linux")]

use nexo_core::sandbox::{SandboxMode, SandboxSettings};
use nexo_core::{Tool, ToolContext, ToolOutput};
use nexo_tools::bash::Bash;
use nexo_tools::guarded::NOTE_PREFIX;
use serde_json::json;

#[path = "../../nexo-agent/tests/scratch/mod.rs"]
mod scratch;

/// The scratch folder a command without a session gets lives until the process exits.
fn wipe_scratch_at_exit() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| scratch::at_exit(|| nexo_tools::bash::wipe_tmp("")));
}

fn ws() -> SandboxSettings {
    SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![])
}

async fn run(dir: &std::path::Path, s: SandboxSettings, cmd: &str) -> Option<ToolOutput> {
    if !nexo_tools::sandbox::auto_allows(&ws()) {
        eprintln!("skipped: no sandbox backend works here");
        return None;
    }
    wipe_scratch_at_exit();
    let mut ctx = ToolContext::new(dir.to_path_buf());
    ctx.sandbox = s;
    Some(Bash.run(json!({"command": cmd}), &ctx).await)
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let ok = std::process::Command::new("git").args(["init", "-q"]).current_dir(d.path()).status().unwrap();
    assert!(ok.success());
    d
}

const GIT: &str = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false -c core.hooksPath=.git/hooks";

#[tokio::test]
async fn a_hook_written_in_the_sandbox_is_named() {
    let d = repo();
    let Some(o) = run(d.path(), ws(), "echo 'echo hi' > .git/hooks/pre-commit").await else { return };
    assert!(!o.is_error, "{}", o.content);
    assert!(o
        .content
        .trim_end()
        .ends_with("[sandbox: this command changed .git/hooks/pre-commit, which runs outside the sandbox later]"));
}

#[tokio::test]
async fn ordinary_git_use_says_nothing() {
    let d = repo();
    let cmd = format!("echo a > a.txt && {GIT} add a.txt && {GIT} commit -q -m one && {GIT} status && {GIT} log && {GIT} branch b && {GIT} diff HEAD~0");
    let Some(o) = run(d.path(), ws(), &cmd).await else { return };
    assert!(!o.is_error, "{}", o.content);
    assert!(!o.content.contains(NOTE_PREFIX), "{}", o.content);
}

#[tokio::test]
async fn git_config_and_remote_add_change_the_config() {
    let d = repo();
    let Some(o) = run(d.path(), ws(), "git config core.fsmonitor true").await else { return };
    assert!(o.content.contains("changed .git/config"), "{}", o.content);
    let o = run(d.path(), ws(), "git remote add o https://example.invalid/x.git").await.unwrap();
    assert!(o.content.contains("changed .git/config"), "{}", o.content);
}

#[tokio::test]
async fn a_command_that_touches_nothing_guarded_says_nothing() {
    let d = repo();
    let Some(o) = run(d.path(), ws(), "echo x > notes.txt; mkdir -p src; echo y > src/m.rs").await else { return };
    assert!(!o.content.contains(NOTE_PREFIX), "{}", o.content);
}

#[tokio::test]
async fn project_files_that_steer_the_agent_are_named() {
    let d = repo();
    let Some(o) = run(d.path(), ws(), "mkdir .nexo && echo '{}' > .mcp.json").await else { return };
    assert!(o.content.contains(".nexo") && o.content.contains(".mcp.json"), "{}", o.content);
}

#[tokio::test]
async fn with_the_sandbox_off_there_is_no_line() {
    let d = repo();
    wipe_scratch_at_exit();
    let mut ctx = ToolContext::new(d.path().to_path_buf());
    ctx.sandbox = SandboxSettings::default();
    let o = Bash.run(json!({"command": "echo x > .git/hooks/pre-commit"}), &ctx).await;
    assert!(!o.content.contains(NOTE_PREFIX), "{}", o.content);
}
