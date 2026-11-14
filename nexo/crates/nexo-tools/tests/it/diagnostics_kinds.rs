//! The diagnostics tool on each project kind: an answer fast, never a 90 s wait.

use std::time::{Duration, Instant};

use nexo_core::{Tool, ToolContext};
use nexo_tools::lsp::Diagnostics;
use serde_json::json;

async fn run(dir: &std::path::Path, path: &str) -> (bool, String, Duration) {
    let ctx = ToolContext::new(dir.to_path_buf());
    let t = Instant::now();
    let out = Diagnostics.run(json!({"path": path}), &ctx).await;
    (out.is_error, out.content, t.elapsed())
}

#[tokio::test]
async fn csv_says_at_once_no_checker() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.csv"), "x,y\n1,2\n").unwrap();
    let (err, text, took) = run(d.path(), "a.csv").await;
    assert!(err && text.contains("no language server"), "{text}");
    assert!(took < Duration::from_secs(2));
}

#[tokio::test]
async fn empty_folder_and_missing_file() {
    let d = tempfile::tempdir().unwrap();
    let (err, text, took) = run(d.path(), "nothing.rs").await;
    assert!(err && text.contains("not a file"), "{text}");
    assert!(took < Duration::from_secs(2));
}

#[tokio::test]
async fn rust_file_outside_any_crate_answers_fast() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("loose.rs"), "fn main() { let n: u32 = \"x\"; }\n").unwrap();
    let (_, text, took) = run(d.path(), "loose.rs").await;
    assert!(!text.is_empty());
    assert!(took < Duration::from_secs(30), "{took:?} {text}");
}

#[tokio::test]
async fn rust_crate_error_is_found_fast() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
        .unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/main.rs"),
        "fn main() {\n    let n: u32 = \"three\";\n    println!(\"{n}\");\n}\n",
    )
    .unwrap();
    let (err, text, took) = run(d.path(), "src/main.rs").await;
    assert!(!err, "{text}");
    assert!(text.contains("src/main.rs:2:") && text.contains("error"), "{text}");
    assert!(took < Duration::from_secs(60), "{took:?}");
}

#[tokio::test]
async fn javascript_syntax_error_and_clean() {
    if nexo_core::shell::find_program("node").is_none()
        || nexo_core::shell::find_program("typescript-language-server").is_some()
    {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("package.json"), "{}").unwrap();
    std::fs::write(d.path().join("bad.js"), "let a = 1;\nlet = = 2;\n").unwrap();
    std::fs::write(d.path().join("ok.js"), "console.log(1);\n").unwrap();
    let (err, text, took) = run(d.path(), "bad.js").await;
    assert!(!err && text.starts_with("bad.js: 1 error") && text.contains("bad.js:2:"), "{text}");
    assert!(took < Duration::from_secs(5));
    let (err, text, _) = run(d.path(), "ok.js").await;
    assert!(!err && text == "ok.js: no problems found", "{text}");
}

#[tokio::test]
async fn python_without_a_server_says_so_at_once() {
    if ["pyright-langserver", "basedpyright-langserver", "pylsp"]
        .iter()
        .any(|p| nexo_core::shell::find_program(p).is_some())
    {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.py"), "print(1)\n").unwrap();
    let (err, text, took) = run(d.path(), "a.py").await;
    assert!(err && text.contains("no checker applies"), "{text}");
    assert!(took < Duration::from_secs(2));
}
