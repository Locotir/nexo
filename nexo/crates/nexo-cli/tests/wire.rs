//! `nexo --wire`: a real request to a local model server is recorded whole, its answer beside it, the report
//! made, and no credential of the provider written anywhere.

use std::io::{Read, Write};
use std::process::Command;

/// A model server that answers `answers` POSTs with one OpenAI chat stream each (anything else: 404), and keeps
/// the bodies it got.
fn server(answers: usize) -> (u16, std::thread::JoinHandle<Vec<String>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let mut got = Vec::new();
        while got.len() < answers {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 65536];
            // the headers, then as many bytes as they announce
            let body = loop {
                let n = s.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break String::from_utf8_lossy(&buf[end + 4..end + 4 + len]).to_string();
                    }
                }
                if n == 0 {
                    break String::new();
                }
            };
            if !buf.starts_with(b"POST") {
                let _ = write!(s, "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                continue;
            }
            got.push(body);
            let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"wired reply\"}}]}\n\n\
                       data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":77,\"completion_tokens\":3}}\n\n\
                       data: [DONE]\n\n";
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                sse.len()
            );
        }
        got
    });
    (port, h)
}

#[test]
fn a_wired_request_is_kept_whole_with_its_answer_and_no_key() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    // the settings live apart from the project: a project's files go to the model, and so on the wire
    let (cfg, work) = (dir.join("cfg"), dir.join("work"));
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let (port, server) = server(1);
    std::fs::write(
        cfg.join("config.toml"),
        format!(
            "provider = \"local\"\nmodel = \"m\"\n[providers.local]\nprotocol = \"openai-chat\"\nmodels = [\"m\"]\n\
             base_url = \"http://127.0.0.1:{port}/v1\"\n\
             [providers.local.headers]\nx-api-key = \"sk-wire-secret\"\n\
             [agent]\nretries = 0\n[context]\npolicy = \"plain\"\n"
        ),
    )
    .unwrap();
    let wire = dir.join("wire");
    let out = Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(["-p", "say the magic word xyzzy", "--wire"])
        .arg(&wire)
        .current_dir(&work)
        .env("NEXO_CONFIG_DIR", &cfg)
        .env("NEXO_DATA_DIR", dir.join("data"))
        .env("NEXO_CACHE_DIR", dir.join("cache"))
        .stdin(std::process::Stdio::null())
        .output()
        .expect("nexo runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("wired reply"));
    assert!(stderr.contains("nexo wire: recording"), "{stderr}");
    let sent = server.join().unwrap();

    let run = std::path::PathBuf::from(std::fs::read_to_string(wire.join("latest")).unwrap().trim());
    // the body on disk is the body the server got
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run.join("0001-request.json")).unwrap()).unwrap();
    let got: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    assert_eq!(file["body"], got);
    assert!(file["body"].to_string().contains("xyzzy"));
    assert_eq!(file["method"], "POST");
    // the answer as it came, the reply text taken from it
    let sse = std::fs::read_to_string(run.join("0001-response.sse")).unwrap();
    assert!(sse.contains("finish_reason") && sse.contains("[DONE]"), "{sse}");
    assert_eq!(std::fs::read_to_string(run.join("0001-reply.txt")).unwrap(), "wired reply");
    // the timeline: the context view, the request and its answer with the provider's count
    let events: Vec<serde_json::Value> = std::fs::read_to_string(run.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(events.iter().any(|e| e["type"] == "note" && e["kind"] == "view"), "{events:?}");
    let answer = events.iter().find(|e| e["type"] == "response").expect("a response line");
    assert_eq!(answer["status"], 200);
    assert_eq!(answer["usage"]["prompt_tokens"], 77);
    assert_eq!(answer["stop"], "stop");
    assert!(answer["error"].is_null(), "{answer}");
    let report = std::fs::read_to_string(run.join("report.html")).unwrap();
    assert!(report.contains("nexo wire") && report.contains(r#""protocol":"openai-chat""#), "{report}");
    // no credential of the provider in any file
    for entry in std::fs::read_dir(&run).unwrap() {
        let text = String::from_utf8_lossy(&std::fs::read(entry.unwrap().path()).unwrap()).to_string();
        assert!(!text.contains("sk-wire-secret"), "the provider's key written");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(run.join("0001-request.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the recording is the owner's only: {mode:o}");
    }
    // `nexo wire` names the newest report
    let shown = Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(["wire", "--dir"])
        .arg(&wire)
        .env("NEXO_CONFIG_DIR", dir)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&shown.stdout).trim().ends_with("report.html"));
}

#[test]
fn without_the_flag_nothing_is_recorded() {
    let d = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(["wire", "--dir"])
        .arg(d.path().join("none"))
        .env("NEXO_CONFIG_DIR", d.path())
        .env_remove("NEXO_WIRE")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("start nexo with --wire"));
}
