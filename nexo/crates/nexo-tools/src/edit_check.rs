//! Compiler errors in a file the model just changed, reported inside the `edit` and `write` result so
//! the model does not spend a step running the build to find them. Cheap and bounded: one checker picked
//! by the project's manifest, a hard time limit, only the lines about the changed file, never an error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::checkers::{program_ok, run};

/// Hard limit of the check; a slower one is dropped silently.
pub const LIMIT: Duration = Duration::from_secs(20);
/// Most lines reported.
const MAX_LINES: usize = 10;
/// Most characters of those lines.
const MAX_CHARS: usize = 600;

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Turns the check on or off for this process (`[tools] edit_diagnostics`).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// The text to append to an edit or write result: empty when the file is clean, the check is off,
/// no checker applies or it ran out of time.
pub async fn report(file: &Path) -> String {
    if !ENABLED.load(Ordering::Relaxed) {
        return String::new();
    }
    crate::lsp::warm(file);
    report_within(file, LIMIT).await
}

pub(crate) async fn report_within(file: &Path, limit: Duration) -> String {
    if crate::data_check::is_data(file) {
        return crate::data_check::report(file);
    }
    // the compiler check and a language server already running answer together; the server adds what
    // the compiler does not say (types in Python, C/C++, warnings) and waits no longer than QUICK
    let (server, compiler) = tokio::join!(crate::lsp::quick(file, crate::lsp::QUICK), found(file, limit));
    let mut lines = compiler.unwrap_or_default();
    for (n, text) in server.unwrap_or_default() {
        if !lines.iter().any(|l| l.contains(&format!(":{n}:")) || l.contains(&format!("({n},"))) {
            lines.push(text);
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = String::from("\ndiagnostics:");
    let mut shown = 0;
    for l in lines.iter().take(MAX_LINES) {
        if shown > 0 && out.len() + l.len() > MAX_CHARS {
            break;
        }
        out.push('\n');
        out.push_str(l);
        shown += 1;
    }
    if lines.len() > shown {
        out.push_str(&format!("\n(+{} more)", lines.len() - shown));
    }
    out
}

fn ancestor_with(file: &Path, name: &str) -> Option<PathBuf> {
    file.ancestors().skip(1).find(|d| d.join(name).is_file()).map(Path::to_path_buf)
}

/// The error lines about `file`; `None` when no checker applies or it failed to finish.
async fn found(file: &Path, limit: Duration) -> Option<Vec<String>> {
    let ext = file.extension()?.to_str()?;
    let (mut cmd, dir, errors_only) = match ext {
        "rs" => {
            let dir = ancestor_with(file, "Cargo.toml")?;
            if !program_ok("cargo") {
                return None;
            }
            let mut c = tokio::process::Command::new("cargo");
            c.args(["check", "--message-format=short", "--quiet", "--color=never", "--all-targets"]);
            (c, dir, true)
        }
        "ts" | "tsx" | "mts" | "cts" => {
            let dir = ancestor_with(file, "tsconfig.json")?;
            let tsc = dir.join("node_modules").join(".bin").join(if cfg!(windows) { "tsc.cmd" } else { "tsc" });
            if !tsc.is_file() {
                return None;
            }
            let mut c = tokio::process::Command::new(tsc);
            c.args(["--noEmit", "--pretty", "false", "-p"]).arg(&dir);
            (c, dir, false)
        }
        "py" => {
            let py = ["python3", "python"].into_iter().find(|p| program_ok(p))?;
            let mut c = tokio::process::Command::new(py);
            c.args(["-m", "py_compile"]).arg(file);
            c.env("PYTHONPYCACHEPREFIX", std::env::temp_dir().join("nexo-pycache"));
            (c, file.parent()?.to_path_buf(), false)
        }
        "go" => {
            let dir = ancestor_with(file, "go.mod")?;
            if !program_ok("go") {
                return None;
            }
            let rel = file.parent()?.strip_prefix(&dir).ok()?.to_string_lossy().replace('\\', "/");
            let mut c = tokio::process::Command::new("go");
            c.arg("vet").arg(if rel.is_empty() { "./".to_string() } else { format!("./{rel}") });
            (c, dir, false)
        }
        _ => return None,
    };
    cmd.current_dir(&dir);
    let (out, err, _) = run(cmd, limit).await.ok()?;
    let file_s = file.to_string_lossy().replace('\\', "/");
    let text = format!("{err}{out}");
    if ext == "py" {
        return Some(py_error(&text, &file_s));
    }
    let mut lines: Vec<String> = Vec::new();
    for l in text.lines().map(str::trim) {
        if (errors_only && !l.contains(": error")) || !about(l, &file_s) {
            continue;
        }
        let l = slashed(l);
        if !lines.contains(&l) {
            lines.push(l);
        }
    }
    Some(lines)
}

/// `line` with the path before the position written with forward slashes (Windows prints `src\lib.rs:2:5`).
fn slashed(line: &str) -> String {
    let colon = line.find(':').unwrap_or(0);
    let end = line.find('(').filter(|p| *p < colon).unwrap_or(colon);
    format!("{}{}", line[..end].replace('\\', "/"), &line[end..])
}

/// Whether `line` is a diagnostic about the file `file` (absolute, forward slashes):
/// `path:LINE:COL: ...` (cargo, go) or `path(LINE,COL): ...` (tsc), the path relative to the project.
fn about(line: &str, file: &str) -> bool {
    let Some(colon) = line.find(':') else { return false };
    let end = line.find('(').filter(|p| *p < colon).unwrap_or(colon);
    let path = line[..end].replace('\\', "/");
    let path = path.trim_start_matches("./");
    !path.is_empty() && (file == path || file.ends_with(&format!("/{path}")))
}

/// py_compile prints `File "path", line N`, the source, a caret and `SyntaxError: message`.
fn py_error(text: &str, file: &str) -> Vec<String> {
    let mut at = None;
    for l in text.lines().map(str::trim) {
        if let Some((path, n)) = l.strip_prefix("File \"").and_then(|r| r.split_once("\", line ")) {
            if path.replace('\\', "/") == file {
                at = Some(n.trim().to_string());
            }
        } else if let (Some(n), true) = (&at, l.contains("Error")) {
            return vec![format!("{file}:{n}: {l}")];
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_with(body: &str) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        let f = d.path().join("src/lib.rs");
        std::fs::write(&f, body).unwrap();
        (d, f)
    }

    #[tokio::test]
    async fn csv_check_reports_rows_columns_and_ragged_lines() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("data.csv");
        std::fs::write(&f, "id,name,qty\n1,a,2\n2,\"b, c\",3\n3,d\n4,e,5\n").unwrap();
        let r = report_within(&f, LIMIT).await;
        assert_eq!(r, "\ncheck: data.csv | 5 rows × 3 columns | ragged rows: 1 (line 4 has 2)", "{r}");
        std::fs::write(&f, "a;b\n1;2\n3;4\n").unwrap();
        assert_eq!(report_within(&f, LIMIT).await, "\ncheck: data.csv | 3 rows × 2 columns | ragged rows: 0");
        let t = d.path().join("d.tsv");
        std::fs::write(&t, "a\tb\n1\t2\n").unwrap();
        assert_eq!(report_within(&t, LIMIT).await, "\ncheck: d.tsv | 2 rows × 2 columns | ragged rows: 0");
    }

    #[tokio::test]
    async fn json_check_reports_the_first_error_with_its_line() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("c.json");
        std::fs::write(&f, "{\n  \"a\": 1,\n  \"b\": ,\n}\n").unwrap();
        let r = report_within(&f, LIMIT).await;
        assert!(r.starts_with("\ncheck: c.json | invalid JSON at line 3"), "{r}");
        std::fs::write(&f, "{\"a\": [1, 2]}").unwrap();
        assert_eq!(report_within(&f, LIMIT).await, "\ncheck: c.json | valid JSON");
        let t = d.path().join("c.toml");
        std::fs::write(&t, "a = 1\nb = \n").unwrap();
        assert!(report_within(&t, LIMIT).await.starts_with("\ncheck: c.toml | invalid TOML at line 2"));
        let y = d.path().join("c.yaml");
        std::fs::write(&y, "a:\n\t- b\n").unwrap();
        assert!(report_within(&y, LIMIT).await.starts_with("\ncheck: c.yaml | invalid YAML at line 2"));
    }

    #[test]
    fn a_windows_path_is_reported_with_forward_slashes() {
        assert_eq!(slashed("src\\a\\lib.rs:2:5: error[E0308]: x\\y"), "src/a/lib.rs:2:5: error[E0308]: x\\y");
        assert_eq!(slashed("src\\a.ts(3,4): error TS1: m"), "src/a.ts(3,4): error TS1: m");
        assert_eq!(slashed("no position"), "no position");
    }

    #[tokio::test]
    async fn a_type_error_is_reported_and_a_clean_file_is_not() {
        if !program_ok("cargo") {
            return;
        }
        let (_d, f) = crate_with("pub fn a() -> u32 {\n    \"no\"\n}\n");
        let r = report_within(&f, Duration::from_secs(120)).await;
        assert!(r.starts_with("\ndiagnostics:\n") && r.contains("src/lib.rs:2:") && r.contains("error"), "{r}");
        std::fs::write(&f, "pub fn a() -> u32 {\n    1\n}\n").unwrap();
        assert_eq!(report_within(&f, Duration::from_secs(120)).await, "");
    }

    #[tokio::test]
    async fn the_edit_tool_carries_it_and_the_switch_turns_it_off() {
        if !program_ok("cargo") {
            return;
        }
        use nexo_core::{Tool, ToolContext};
        let (d, _f) = crate_with("pub fn a() -> u32 {\n    1\n}\n");
        let c = ToolContext::new(d.path().to_path_buf());
        let args = serde_json::json!({"path": "src/lib.rs", "old_string": "    1", "new_string": "    \"no\""});
        let out = crate::edit::Edit.run(args.clone(), &c).await;
        assert!(!out.is_error && out.content.contains("\ndiagnostics:\n"), "{}", out.content);
        set_enabled(false);
        let back = serde_json::json!({"path": "src/lib.rs", "old_string": "    \"no\"", "new_string": "    2"});
        let _ = crate::edit::Edit.run(back, &c).await;
        let again = serde_json::json!({"path": "src/lib.rs", "old_string": "    2", "new_string": "    \"no\""});
        let out = crate::edit::Edit.run(again, &c).await;
        set_enabled(true);
        assert!(!out.is_error && !out.content.contains("diagnostics:"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_check_that_runs_out_of_time_says_nothing() {
        if !program_ok("cargo") {
            return;
        }
        let (_d, f) = crate_with("pub fn a() -> u32 {\n    \"no\"\n}\n");
        assert_eq!(report_within(&f, Duration::from_millis(1)).await, "");
    }

    #[tokio::test]
    async fn no_manifest_no_check() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.rs");
        std::fs::write(&f, "fn a() -> u32 { \"x\" }").unwrap();
        assert_eq!(report_within(&f, LIMIT).await, "");
    }

    #[test]
    fn python_syntax_error_names_line_and_message() {
        let t = "  File \"/p/a.py\", line 2\n    x = = 1\n        ^\nSyntaxError: invalid syntax\n";
        assert_eq!(py_error(t, "/p/a.py"), vec!["/p/a.py:2: SyntaxError: invalid syntax".to_string()]);
        assert!(py_error(t, "/p/b.py").is_empty());
    }

    #[test]
    fn only_lines_about_the_file_match() {
        let f = "/p/crate/src/lib.rs";
        assert!(about("src/lib.rs:2:5: error[E0308]: mismatched types", f));
        assert!(!about("src/other.rs:2:5: error: x", f));
        assert!(!about("xsrc/lib.rs:2:5: error: x", f));
        assert!(about("src/lib.ts(3,5): error TS1: x", "/p/src/lib.ts"));
        assert!(about("./main.go:3:5: undefined: x", "/p/main.go"));
        assert!(!about("error: could not compile `t`", f));
    }
}
