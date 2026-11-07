//! `nexo plugin install` run as the binary: with nobody to ask it needs `--yes`.

use std::process::{Command, Stdio};

fn nexo(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(args)
        .env("NEXO_CONFIG_DIR", home)
        .env("NEXO_DATA_DIR", home.join("data"))
        .env("NEXO_CACHE_DIR", home.join("cache"))
        .stdin(Stdio::null())
        .output()
        .expect("nexo runs")
}

#[test]
fn install_needs_yes_when_nothing_can_be_asked() {
    let (home, src) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir_all(src.path().join(".claude-plugin")).unwrap();
    std::fs::write(src.path().join(".claude-plugin/plugin.json"), r#"{"name":"acme","version":"1.0.0"}"#).unwrap();
    std::fs::create_dir_all(src.path().join("commands")).unwrap();
    std::fs::write(src.path().join("commands/ship.md"), "Ship").unwrap();
    let s = src.path().to_str().unwrap();
    let out = nexo(home.path(), &["plugin", "install", s]);
    assert!(!out.status.success());
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("--yes") && text.contains("acme:ship"), "{text}");
    assert!(!home.path().join("plugins/acme").exists());
    let out = nexo(home.path(), &["plugin", "install", s, "--yes"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(home.path().join("plugins/acme/commands/ship.md").is_file());
    let list = nexo(home.path(), &["plugin", "list"]);
    assert!(String::from_utf8_lossy(&list.stdout).contains("acme"));
}
