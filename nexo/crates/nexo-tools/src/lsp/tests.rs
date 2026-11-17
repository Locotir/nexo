use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nexo_core::{Tool, ToolContext};
use serde_json::json;

use super::quick::*;
use super::running::*;
use super::server::*;
use super::table::*;
use super::*;
#[test]
fn servers_by_extension_and_the_report() {
    assert_eq!(servers_for(Path::new("a/b.rs")).unwrap().0, "rust");
    assert_eq!(servers_for(Path::new("x.PY")).unwrap().0, "python");
    assert!(servers_for(Path::new("notes.md")).is_none());
    let d = vec![
        json!({"range": {"start": {"line": 4, "character": 2}}, "severity": 2, "message": "unused", "source": "rustc"}),
        json!({"range": {"start": {"line": 9, "character": 0}}, "severity": 1, "message": "mismatched types\nexpected u32"}),
    ];
    assert_eq!(
        format("src/a.rs", &d),
        "src/a.rs: 1 error, 1 warning\nsrc/a.rs:10:1 error: mismatched types\nsrc/a.rs:5:3 warning: unused (rustc)"
    );
    assert_eq!(format("a.py", &[]), "a.py: no problems found");
}

#[test]
fn the_languages_of_a_project_skip_hidden_and_vendored_folders() {
    let d = tempfile::tempdir().unwrap();
    for f in ["a.py", "b.py", "src/c.rs", "notes.md", "node_modules/x.js", ".git/y.go", "target/z.c"] {
        let p = d.path().join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    let mut langs: Vec<&str> = sample_files(d.path()).iter().filter_map(|f| servers_for(f)).map(|s| s.0).collect();
    langs.sort();
    assert_eq!(langs, ["python", "rust"]);
}

#[test]
fn initialize_params_disable_build_scripts() {
    let ra = initialize_params("rust-analyzer", Path::new("/p"));
    assert_eq!(ra["initializationOptions"]["cargo"]["buildScripts"]["enable"], false);
    assert_eq!(ra["initializationOptions"]["procMacro"]["enable"], false);
    assert_eq!(ra["initializationOptions"]["checkOnSave"], false);
    assert!(initialize_params("pyright-langserver", Path::new("/p")).get("initializationOptions").is_none());
}

/// A server that exits at once (a rustup shim without the component) fails `initialize` now, not at the limit.
#[tokio::test]
async fn a_server_that_exits_fails_fast() {
    let t = std::time::Instant::now();
    let r = Server::start(&["false"], Path::new("/")).await;
    assert!(r.is_err());
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

/// Against the real pyright, when it is installed: errors found, then gone once fixed.
#[tokio::test]
async fn pyright_finds_the_errors_and_sees_the_fix() {
    if !which("pyright-langserver") {
        eprintln!("pyright-langserver is not installed: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("pyproject.toml"), "[project]\nname = \"x\"\n").unwrap();
    let f = d.path().join("bad.py");
    std::fs::write(&f, "x: int = \"not a number\"\nprint(undefined_name)\n").unwrap();
    let ctx = ToolContext::new(d.path().to_path_buf());
    let out = Diagnostics.run(json!({"path": "bad.py"}), &ctx).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.starts_with("bad.py: 2 errors"), "{}", out.content);
    assert!(out.content.contains("bad.py:1:") && out.content.contains("bad.py:2:"), "{}", out.content);
    assert!(out.content.contains("undefined_name"), "{}", out.content);
    std::fs::write(&f, "x: int = 3\nprint(x)\n").unwrap();
    let out = Diagnostics.run(json!({"path": "bad.py"}), &ctx).await;
    assert_eq!(out.content, "bad.py: no problems found");
    let none = Diagnostics.run(json!({"path": "pyproject.toml"}), &ctx).await;
    assert!(none.is_error && none.content.contains("no language server"));
}

/// Against the real rust-analyzer, when it is installed: a type error in a small crate.
#[tokio::test]
async fn rust_analyzer_finds_a_type_error() {
    if !which("rust-analyzer")
        || std::process::Command::new("rust-analyzer")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
    {
        eprintln!("rust-analyzer is not usable here: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
        .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/main.rs"),
        "fn main() {\n    let n: u32 = \"three\";\n    println!(\"{n}\");\n}\n",
    )
    .unwrap();
    let ctx = ToolContext::new(d.path().to_path_buf());
    let out = Diagnostics.run(json!({"path": "src/main.rs"}), &ctx).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("src/main.rs:2:") && out.content.contains("error"), "{}", out.content);
}

/// A language server written in Python: `BAD` in the text gives an error, a warning and a hint;
/// `SLOW` makes it answer after five seconds; anything else is clean.
const FAKE: &str = r#"
import sys, json, time
def read():
    n = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            n = int(line.split(b":")[1])
    return json.loads(sys.stdin.buffer.read(n))
def send(m):
    b = json.dumps(m).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    sys.stdout.buffer.flush()
def publish(doc, text):
    if "SLOW" in text:
        time.sleep(5)
    d = []
    if "BAD" in text:
        r = {"start": {"line": 0, "character": 0}}
        d = [{"range": r, "severity": 3, "message": "hint only"},
             {"range": {"start": {"line": 2, "character": 4}}, "severity": 2, "message": "unused thing", "source": "fake"},
             {"range": r, "severity": 1, "message": "fake error\nsecond line", "source": "fake"}]
    send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
          "params": {"uri": doc["uri"], "version": doc.get("version"), "diagnostics": d}})
while True:
    m = read()
    if m is None:
        break
    meth = m.get("method")
    if meth == "initialize":
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {}}})
    elif meth == "textDocument/didOpen":
        publish(m["params"]["textDocument"], m["params"]["textDocument"]["text"])
    elif meth == "textDocument/didChange":
        publish(m["params"]["textDocument"], m["params"]["contentChanges"][0]["text"])
"#;

/// A project with a Python file and the fake server registered as the running `pylsp` there. The
/// server leaves the table, and its process the machine, when this is dropped (even on a panic).
struct Fake {
    dir: tempfile::TempDir,
}

impl Drop for Fake {
    fn drop(&mut self) {
        if let Ok(mut table) = servers().try_lock() {
            table.remove(&("pylsp".to_string(), self.dir.path().to_path_buf()));
        }
    }
}

async fn project_with_fake(text: &str) -> Option<(Fake, PathBuf)> {
    let Some(python) = ["python3", "python"].into_iter().find(|p| which(p)) else {
        eprintln!("python is not installed: skipped");
        return None;
    };
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("pyproject.toml"), "[project]\nname = \"x\"\n").unwrap();
    let script = d.path().join("fake_lsp.py");
    std::fs::write(&script, FAKE).unwrap();
    let file = d.path().join("m.py");
    std::fs::write(&file, text).unwrap();
    let script = script.to_string_lossy().into_owned();
    let server = Server::start(&[python, &script], d.path()).await.unwrap();
    servers().lock().await.insert(("pylsp".into(), d.path().to_path_buf()), Arc::new(server));
    Some((Fake { dir: d }, file))
}

#[test]
fn an_edit_that_breaks_the_file_carries_the_server_errors() {
    crate::util::run_bounded(Duration::from_secs(60), "an_edit_that_breaks_the_file", async {
        let Some((d, file)) = project_with_fake("x = 1\n").await else { return };
        let ctx = ToolContext::new(d.dir.path().to_path_buf());
        ctx.seen.mark(&file);
        let out =
            crate::edit::Edit.run(json!({"path": "m.py", "old_string": "x = 1", "new_string": "BAD = 1"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content
                .contains("\ndiagnostics:\nm.py:1:1 error: fake error (fake)\nm.py:3:5 warning: unused thing (fake)"),
            "{}",
            out.content
        );
        assert!(!out.content.contains("hint only") && !out.content.contains("second line"), "{}", out.content);
        // fixed: nothing is appended
        let out =
            crate::edit::Edit.run(json!({"path": "m.py", "old_string": "BAD = 1", "new_string": "y = 1"}), &ctx).await;
        assert!(!out.is_error && !out.content.contains("diagnostics:"), "{}", out.content);
    });
}

#[test]
fn a_slow_server_is_waited_for_only_a_moment_and_nothing_is_added() {
    crate::util::run_bounded(Duration::from_secs(60), "a_slow_server_is_waited_for", async {
        let Some((_d, file)) = project_with_fake("SLOW\n").await else { return };
        let t = std::time::Instant::now();
        assert!(quick(&file, Duration::from_millis(300)).await.is_none());
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        let t = std::time::Instant::now();
        assert_eq!(crate::edit_check::report_within(&file, Duration::from_secs(20)).await, "");
        assert!(t.elapsed() < QUICK + Duration::from_secs(2), "{:?}", t.elapsed());
    });
}

#[tokio::test]
async fn no_running_server_means_no_wait_and_no_start() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("pyproject.toml"), "").unwrap();
    let file = d.path().join("m.py");
    std::fs::write(&file, "BAD\n").unwrap();
    let t = std::time::Instant::now();
    assert!(quick(&file, QUICK).await.is_none());
    assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
    assert!(servers().lock().await.keys().all(|(_, root)| root != d.path()));
}

/// A server that announces a 100 GB message must end the connection like an EOF, not make nexo
/// allocate that much (an abort that would take the session with it).
#[cfg(unix)]
#[test]
fn a_huge_content_length_ends_the_connection_instead_of_allocating() {
    crate::util::run_bounded(Duration::from_secs(30), "a_huge_content_length", async {
        let script = r"read l; printf 'Content-Length: 99999999999\r\n\r\n'; sleep 30";
        let e = Server::start(&["sh", "-c", script], Path::new("/")).await.err().expect("start fails");
        assert!(e.contains("the language server went away"), "{e}");
    });
}

/// A header line that never ends is cut off at its limit, so the reader cannot grow without bound.
#[cfg(unix)]
#[test]
fn a_header_line_without_end_ends_the_connection() {
    crate::util::run_bounded(Duration::from_secs(30), "a_header_line_without_end", async {
        let script = "read l; head -c 100000 /dev/zero | tr '\\0' 'x'; sleep 30";
        let e = Server::start(&["sh", "-c", script], Path::new("/")).await.err().expect("start fails");
        assert!(e.contains("the language server went away"), "{e}");
    });
}
