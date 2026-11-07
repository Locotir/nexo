//! The OS sandbox of the bash tool, on Linux with Landlock. Skipped (with a message) where the
//! kernel has none.
#![cfg(target_os = "linux")]

use nexo_core::sandbox::{SandboxMode, SandboxSettings};
use nexo_core::{Tool, ToolContext, ToolOutput};
use nexo_tools::bash::Bash;
use nexo_tools::sandbox::{self, Backend};
use serde_json::json;

fn landlock() -> bool {
    if *sandbox::backend() == Backend::Landlock {
        return true;
    }
    eprintln!("skipped: Landlock is not available here ({})", sandbox::backend().name());
    false
}

async fn run(dir: &std::path::Path, s: SandboxSettings, args: serde_json::Value) -> ToolOutput {
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
    if !landlock() {
        return;
    }
    let p = tempfile::tempdir().unwrap();
    let o = run(p.path(), ws(false), json!({"command": "echo hi > out.txt && cat out.txt"})).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(p.path().join("out.txt").is_file());
}

#[tokio::test]
async fn writing_in_home_outside_the_project_fails_and_the_note_appears() {
    if !landlock() {
        return;
    }
    let p = tempfile::tempdir().unwrap();
    let outside = in_home();
    let target = outside.path().join("x.txt");
    let cmd = format!("echo no > {}", target.display());
    let o = run(p.path(), ws(false), json!({"command": cmd})).await;
    assert!(o.is_error, "{}", o.content);
    assert!(!target.exists());
    assert!(o.content.contains("[sandbox:") && o.content.contains("unsandboxed"), "{}", o.content);
    // the same command, asked to run outside the sandbox, works
    let o = run(p.path(), ws(false), json!({"command": cmd, "unsandboxed": true})).await;
    assert!(!o.is_error, "{}", o.content);
    assert!(target.exists());
}

#[tokio::test]
async fn read_only_mode_cannot_write_the_project_and_extra_dirs_open_up() {
    if !landlock() {
        return;
    }
    // the temp directory is writable in every mode, so the project lives in the home here
    let p = in_home();
    let ro = SandboxSettings::new(SandboxMode::ReadOnly, false, vec![]);
    let o = run(p.path(), ro, json!({"command": "echo x > a.txt"})).await;
    assert!(o.is_error && !p.path().join("a.txt").exists(), "{}", o.content);
    let extra = in_home();
    let s = SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![extra.path().to_path_buf()]);
    let cmd = format!("echo x > {}/b.txt", extra.path().display());
    let o = run(p.path(), s, json!({"command": cmd})).await;
    assert!(!o.is_error, "{}", o.content);
}

#[tokio::test]
async fn reading_the_whole_disk_works() {
    if !landlock() {
        return;
    }
    let p = tempfile::tempdir().unwrap();
    let o = run(p.path(), ws(false), json!({"command": "cat /etc/passwd | head -n 1"})).await;
    assert!(!o.is_error && !o.content.trim().is_empty(), "{}", o.content);
}

#[tokio::test]
async fn network_off_blocks_a_connect_to_a_local_listener() {
    if !landlock() {
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
    assert!(o.is_error && !o.content.contains("connected"), "{}", o.content);
    // control: with the network on the same command reaches the listener
    let o = run(p.path(), ws(true), json!({"command": cmd})).await;
    assert!(o.content.contains("connected"), "{}", o.content);
    drop(l);
}

#[tokio::test]
async fn the_sandbox_lets_bash_run_without_asking_and_says_so() {
    if !landlock() {
        return;
    }
    let s = ws(false);
    assert!(Bash.sandbox_holds(&json!({"command": "ls"}), &s));
    assert!(!Bash.sandbox_holds(&json!({"command": "ls", "unsandboxed": true}), &s));
    assert!(!Bash.sandbox_holds(&json!({"command": "ls"}), &SandboxSettings::default()));
    let line = sandbox::describe(&s);
    assert!(line.contains("workspace-write") && line.contains("landlock"), "{line}");
}
