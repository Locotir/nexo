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
