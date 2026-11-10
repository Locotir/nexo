//! First-run rough edges: what the binary prints when the setup is wrong. No network, no key.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const MOCK: &str =
    "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\"]\n[context]\npolicy = \"plain\"\n";

fn nexo(dir: &Path, config: &str, args: &[&str]) -> Output {
    std::fs::write(dir.join("config.toml"), config).unwrap();
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("NEXO_CONFIG_DIR", dir)
        .env("NEXO_DATA_DIR", dir.join("data"))
        .env("NEXO_CACHE_DIR", dir.join("cache"))
        .env("NEXO_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::null())
        .output()
        .expect("nexo runs")
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn the_interface_without_a_terminal_says_to_use_print_mode() {
    let d = tempfile::tempdir().unwrap();
    let o = nexo(d.path(), MOCK, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let e = err(&o);
    assert!(e.contains("needs a terminal") && e.contains("nexo -p"), "{e}");
    assert!(!e.contains("os error"), "{e}");
}

#[test]
fn an_unknown_provider_in_the_config_names_the_file_the_key_and_the_value() {
    let d = tempfile::tempdir().unwrap();
    let o = nexo(d.path(), "provider = \"anthropc\"\n", &["-p", "hi"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("config.toml") && e.contains("provider = \"anthropc\""), "{e}");
    assert!(e.contains("did you mean \"anthropic\""), "{e}");
    assert_eq!(e.trim().lines().count(), 1, "{e}");
    let o = nexo(d.path(), "provider = \"nosuch\"\n", &["-p", "hi"]);
    assert!(err(&o).contains("known providers: "), "{}", err(&o));
    let o = nexo(d.path(), "provider = \"nosuch\"\n", &["doctor", "--offline"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("FIX ") && out(&o).contains("provider = \"nosuch\""), "{}", out(&o));
}

#[test]
fn doctor_flags_an_unknown_theme_policy_and_model() {
    let d = tempfile::tempdir().unwrap();
    let cfg = "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\"]\n\
               [ui]\ntheme = \"nosuchtheme\"\n[context]\npolicy = \"nosuchpolicy\"\n";
    let o = nexo(d.path(), cfg, &["doctor", "--offline"]);
    let s = out(&o);
    assert!(s.contains("[ui] theme = \"nosuchtheme\"") && s.contains("accepted: auto, nexo"), "{s}");
    assert!(s.contains("[context] policy = \"nosuchpolicy\"") && s.contains("accepted: auto, plain"), "{s}");
    assert!(s.contains("config.toml"), "{s}");
    // a good setting is not flagged
    let o = nexo(d.path(), MOCK, &["doctor", "--offline"]);
    assert!(!out(&o).contains("theme =") && !out(&o).contains("policy ="), "{}", out(&o));
}

#[test]
fn doctor_flags_a_model_a_hosted_provider_does_not_have() {
    let d = tempfile::tempdir().unwrap();
    let cfg = "provider = \"mockhost\"\nmodel = \"nosuchmodel\"\n[providers.mockhost]\nprotocol = \"mock\"\nmodels = [\"a\", \"b\"]\n\
               base_url = \"http://127.0.0.1:9\"\napi_key_cmd = \"echo dummy\"\n";
    let o = nexo(d.path(), cfg, &["doctor", "--offline"]);
    let s = out(&o);
    assert!(s.contains("model = \"nosuchmodel\"") && s.contains("a, b"), "{s}");
}

#[test]
fn print_mode_json_start_up_errors_are_a_result_object_on_stdout() {
    let d = tempfile::tempdir().unwrap();
    let o = nexo(d.path(), "provider = \"nosuch\"\n", &["-p", "hi", "--output-format", "json"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("nosuch"), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(out(&o).trim()).expect("a JSON object on stdout");
    assert_eq!(v["type"], "result");
    assert_eq!(v["subtype"], "error");
    assert_eq!(v["is_error"], true);
    assert_eq!(v["exit_code"], 1);
    assert!(v["error"].as_str().unwrap().contains("nosuch"), "{v}");
    // bad TOML is a start-up error too; text mode keeps stdout empty
    let o = nexo(d.path(), "provider = \"mock", &["-p", "hi", "--output-format", "stream-json"]);
    let v: serde_json::Value = serde_json::from_str(out(&o).trim()).expect("a JSON object on stdout");
    assert_eq!(v["type"], "result");
    let o = nexo(d.path(), "provider = \"nosuch\"\n", &["-p", "hi"]);
    assert!(out(&o).is_empty(), "{}", out(&o));
}

#[test]
fn export_with_no_sessions_creates_nothing() {
    let d = tempfile::tempdir().unwrap();
    let o = nexo(d.path(), MOCK, &["export"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o).trim(), "no sessions yet");
    let html =
        std::fs::read_dir(d.path()).unwrap().flatten().any(|e| e.path().extension().is_some_and(|x| x == "html"));
    assert!(!html, "no file was written");
    assert!(
        !d.path().join("data/sessions").exists()
            || std::fs::read_dir(d.path().join("data/sessions")).unwrap().count() == 0
    );
}

#[test]
fn pricing_needs_no_key() {
    let d = tempfile::tempdir().unwrap();
    let o = nexo(d.path(), "", &["pricing", "anthropic/claude-sonnet-4-5"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("anthropic/claude-sonnet-4-5") && out(&o).contains("context window"), "{}", out(&o));
    let o = nexo(d.path(), "", &["pricing"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("prices"), "{}", out(&o));
}

#[test]
fn an_unreachable_provider_names_the_address_and_ollama() {
    let d = tempfile::tempdir().unwrap();
    // nothing listens on this port of the local machine
    let cfg =
        "[providers.ollama]\nprotocol = \"openai-chat\"\nbase_url = \"http://127.0.0.1:9/v1\"\nmodels = [\"m\"]\n";
    let o = nexo(d.path(), cfg, &["-p", "hi", "--provider", "ollama", "--model", "m"]);
    let e = err(&o);
    assert!(e.contains("http://127.0.0.1:9/v1") && e.contains("ollama serve"), "{e}");
    assert!(!e.contains("Could not reach the provider:"), "{e}");
}
