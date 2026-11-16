//! The OS sandbox of the bash tool: on Linux with Landlock, on macOS with `sandbox-exec`. Skipped
//! (with a message) where that backend does not work (`sandbox_bwrap.rs` tests the Linux fallback).
#![cfg(any(target_os = "linux", target_os = "macos"))]

use nexo_core::sandbox::{SandboxMode, SandboxSettings};
use nexo_core::{Tool, ToolContext, ToolOutput};
use nexo_tools::bash::Bash;
use nexo_tools::sandbox::{self, Backend};
use serde_json::json;

use crate::scratch;

/// The scratch folder a command without a session gets lives until the process exits.
fn wipe_scratch_at_exit() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| scratch::at_exit(|| nexo_tools::bash::wipe_tmp("")));
}

/// Whether this system's own backend works here: Landlock on Linux, `sandbox-exec` on macOS.
fn backend_available() -> bool {
    let own = match sandbox::backend() {
        Backend::Landlock => cfg!(target_os = "linux"),
        Backend::SandboxExec(_) => cfg!(target_os = "macos"),
        _ => false,
    };
    if !own {
        eprintln!("skipped: the sandbox backend of this system is not available here ({:?})", sandbox::backend());
    }
    own
}

async fn run(dir: &std::path::Path, s: SandboxSettings, args: serde_json::Value) -> ToolOutput {
    wipe_scratch_at_exit();
    let mut ctx = ToolContext::new(dir.to_path_buf());
    ctx.sandbox = s;
    Bash.run(args, &ctx).await
}

fn ws(network: bool) -> SandboxSettings {
    SandboxSettings::new(SandboxMode::WorkspaceWrite, network, vec![])
}

/// A directory in the home, outside every writable root.
fn in_home() -> tempfile::TempDir {
    let home = dirs::home_dir().expect("home");
    tempfile::Builder::new().prefix(".nexo-sandbox-test-").tempdir_in(home).unwrap()
}

#[tokio::test]
async fn writing_inside_the_project_works() {
    if !backend_available() {
        return;
    }
    let p = tempfile::tempdir().unwrap();
    let o = run(p.path(), ws(false), json!({"command": "echo hi > out.txt && cat out.txt"})).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(p.path().join("out.txt").is_file());
}

#[tokio::test]
async fn writing_in_home_outside_the_project_fails_and_the_note_appears() {
    if !backend_available() {
        return;
    }
    let p = tempfile::tempdir().unwrap();
    let outside = in_home();
    let target = outside.path().join("x.txt");
    let cmd = format!("echo no > {}", target.display());
    let o = run(p.path(), ws(false), json!({"command": cmd})).await;
    // a non-zero exit is shown; the exact code (1 on bash, 2 on dash) is the shell's business
    assert!(o.content.contains("[exit code") && !o.content.contains("[exit code 0]"), "{}", o.content);
    assert!(!target.exists());
    assert!(o.content.contains("[sandbox:") && o.content.contains("unsandboxed"), "{}", o.content);
    // the same command, asked to run outside the sandbox, works
    let o = run(p.path(), ws(false), json!({"command": cmd, "unsandboxed": true})).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(target.exists());
}

#[tokio::test]
async fn read_only_mode_cannot_write_the_project_and_extra_dirs_open_up() {
    if !backend_available() {
        return;
    }
    // the temp directory is writable in every mode, so the project lives in the home here
    let p = in_home();
    let ro = SandboxSettings::new(SandboxMode::ReadOnly, false, vec![]);
    let o = run(p.path(), ro, json!({"command": "echo x > a.txt"})).await;
    assert!(
        o.content.contains("[exit code") && !o.content.contains("[exit code 0]") && !p.path().join("a.txt").exists(),
        "{}",
        o.content
    );
    let extra = in_home();
    let s = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![extra.path().to_path_buf()]);
    let cmd = format!("echo x > {}/b.txt", extra.path().display());
    let o = run(p.path(), s, json!({"command": cmd})).await;
    assert!(!o.is_error, "{}", o.content);
}

#[tokio::test]
async fn reading_the_whole_disk_works() {
    if !backend_available() {
        return;
    }
    let p = tempfile::tempdir().unwrap();
    let o = run(p.path(), ws(false), json!({"command": "cat /etc/passwd | head -n 1"})).await;
    assert!(!o.is_error && !o.content.trim().is_empty(), "{}", o.content);
}

#[tokio::test]
async fn network_off_blocks_a_connect_to_a_local_listener() {
    if !backend_available() {
        return;
    }
    if std::process::Command::new("bash").arg("-c").arg("true").status().is_err() {
        eprintln!("skipped: bash is not installed");
        return;
    }
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let p = tempfile::tempdir().unwrap();
    let cmd = format!("bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port} && echo connected'");
    let o = run(p.path(), ws(false), json!({"command": cmd})).await;
    assert!(o.content.contains("[exit code") && !o.content.contains("connected"), "{}", o.content);
    // control: with the network on the same command reaches the listener
    let o = run(p.path(), ws(true), json!({"command": cmd})).await;
    assert!(o.content.contains("connected"), "{}", o.content);
    drop(l);
}

#[tokio::test]
async fn the_sandbox_lets_bash_run_without_asking_and_says_so() {
    if !backend_available() {
        return;
    }
    let s = ws(false);
    assert!(Bash.sandbox_holds(&json!({"command": "ls"}), &s));
    assert!(!Bash.sandbox_holds(&json!({"command": "ls", "unsandboxed": true}), &s));
    assert!(!Bash.sandbox_holds(&json!({"command": "ls"}), &SandboxSettings::default()));
    let line = sandbox::describe(&s);
    assert!(line.contains("workspace-write") && line.contains(sandbox::backend().name()), "{line}");
}
