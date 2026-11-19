use super::client::passed_env;
use super::*;
use nexo_core::config::McpServerConfig;

#[tokio::test]
async fn a_missing_command_is_an_error() {
    let cfg = McpServerConfig { command: "/nonexistent/mcp-server".into(), ..Default::default() };
    assert!(McpClient::connect("x", &cfg).await.is_err());
    assert!(McpClient::connect("x", &McpServerConfig::default()).await.is_err());
}
#[test]
fn a_server_gets_only_what_a_process_needs_to_run() {
    for k in ["PATH", "HOME", "LANG", "LC_ALL", "XDG_RUNTIME_DIR", "TMPDIR"] {
        assert!(passed_env(k), "{k}");
    }
    for k in ["DEEPSEEK_API_KEY", "OPENAI_API_KEY", "GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY", "SSH_AUTH_SOCK"] {
        assert!(!passed_env(k), "{k}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_line_without_end_is_cut_off_at_the_cap() {
    // after the first request the server writes 17 MiB with no newline and keeps the pipe open
    let script = "read l; yes x | tr -d '\\n' | head -c 17825792; sleep 30";
    let cfg = McpServerConfig { command: "sh".into(), args: vec!["-c".into(), script.into()], ..Default::default() };
    let e = match McpClient::connect("big", &cfg).await {
        Ok(_) => panic!("connected"),
        Err(e) => e.to_string(),
    };
    assert!(e.contains("over 16 MiB"), "{e}");
}

// it reads /proc, which only Linux has: elsewhere `gone` would pass without the fix
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_server_that_fails_to_start_takes_what_it_started_with_it() {
    // the launcher leaves a grandchild behind and ends at once, as `npx` would with a worker that ignores EOF
    let d = tempfile::tempdir().unwrap();
    let script = "sleep 31 >/dev/null 2>&1 & echo $! > pid";
    let cfg = McpServerConfig {
        command: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: Some(d.path().to_path_buf()),
        ..Default::default()
    };
    assert!(McpClient::connect("orphan", &cfg).await.is_err());
    let pid = std::fs::read_to_string(d.path().join("pid")).unwrap().trim().to_string();
    // a killed process may linger as a zombie until its parent reaps it: that is gone too
    let gone = || match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s.rsplit(") ").next().is_some_and(|rest| rest.starts_with('Z')),
        Err(_) => true,
    };
    for _ in 0..50 {
        if gone() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(gone(), "the sleep (pid {pid}) is still running");
}
