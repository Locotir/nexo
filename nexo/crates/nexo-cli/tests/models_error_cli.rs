//! Exercise `nexo models` as a user against a synthetic loopback provider.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn models_http_error_does_not_echo_remote_secret_in_text_or_json() {
    const SECRET: &str = "opaque-Z9q4v2-example";
    for json in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(25);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("provider request not received: {e}"),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /v1/models "));
            let body = format!("rejected {SECRET}\u{1b}[31m");
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        std::fs::write(dir.path().join("config.toml"), format!(
            "provider = 'localtest'\nmodel = 'configured-model'\n[providers.localtest]\nprotocol = 'openai-chat'\nbase_url = 'http://{addr}/v1'\napi_key_env = 'NEXO_TEST_KEY'\nmodels = ['configured-model']\n"
        )).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nexo"));
        cmd.args(["models", "localtest"]).current_dir(dir.path()).env_clear();
        for name in ["SystemRoot", "SYSTEMROOT", "windir"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd.env("HOME", dir.path())
            .env("NEXO_CONFIG_DIR", dir.path())
            .env("NEXO_DATA_DIR", dir.path().join("data"))
            .env("NEXO_CACHE_DIR", dir.path().join("cache"))
            .env("NEXO_NO_UPDATE_CHECK", "1")
            .env("NEXO_TEST_KEY", "synthetic-key")
            .stdin(Stdio::null());
        if json {
            cmd.arg("--json");
        }
        let output = cmd.output().expect("nexo runs");
        server.join().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "stdout={stdout:?} stderr={stderr:?}");
        for stream in [&stdout, &stderr] {
            assert!(!stream.contains(SECRET), "secret escaped: {stream:?}");
            assert!(!stream.contains('\u{1b}'), "control escaped: {stream:?}");
        }
        let note = if json { &stderr } else { &stdout };
        assert!(note.contains("key missing or rejected") && note.contains("HTTP 401"), "{note:?}");
        if json {
            let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
            assert!(value.as_array().unwrap().iter().any(|m| m["model"] == "configured-model"));
        } else {
            assert!(stdout.contains("configured-model"), "{stdout:?}");
        }
    }
}
