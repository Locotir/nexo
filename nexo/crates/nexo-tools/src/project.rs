//! What the project in a folder is, read from its manifests: the command that checks it (compiles and tests),
//! and whether a shell command is such a check. General: no project, language or task is known by name beyond
//! the manifest file each toolchain defines.

use std::path::{Path, PathBuf};

/// The command that checks a project, and the folder it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The command line (`cargo test -q`).
    pub command: String,
    /// The folder of the manifest that decided it.
    pub dir: PathBuf,
    /// The manifest file name (`Cargo.toml`).
    pub manifest: &'static str,
}

/// The check command of the project that holds `dir`: the nearest manifest at or above it decides.
/// Nothing when no manifest on the way up names a test.
pub fn check_command(dir: &Path) -> Option<Check> {
    check_command_within(dir, None)
}

/// As [`check_command`], looking no higher than `root` when it is given and above `dir`.
pub fn check_command_within(dir: &Path, root: Option<&Path>) -> Option<Check> {
    for d in dir.ancestors() {
        if let Some((command, manifest)) = check_in(d) {
            return Some(Check { command, dir: d.to_path_buf(), manifest });
        }
        if root.is_some_and(|r| d == r) {
            break;
        }
    }
    None
}

fn check_in(d: &Path) -> Option<(String, &'static str)> {
    if d.join("Cargo.toml").is_file() {
        let offline = if offline_ok(d) { " --offline" } else { "" };
        return Some((format!("cargo test -q{offline} --no-fail-fast"), "Cargo.toml"));
    }
    if d.join("go.mod").is_file() {
        return Some(("go test ./...".into(), "go.mod"));
    }
    if let Ok(text) = std::fs::read_to_string(d.join("package.json")) {
        let test = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("scripts")?.get("test")?.as_str().map(str::to_string));
        // `npm init` leaves a script that only fails
        if test.is_some_and(|t| !t.trim().is_empty() && !t.contains("no test specified")) {
            return Some(("npm test -s".into(), "package.json"));
        }
    }
    for m in ["pyproject.toml", "pytest.ini", "setup.cfg"] {
        if d.join(m).is_file() {
            return Some(("pytest -q -x".into(), m));
        }
    }
    None
}

/// Whether a Rust project needs nothing from a registry or a git host: the build can run offline.
fn offline_ok(dir: &Path) -> bool {
    if std::fs::read_to_string(dir.join("Cargo.lock")).is_ok_and(|l| l.contains("registry+") || l.contains("git+")) {
        return false;
    }
    let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else { return false };
    let mut deps = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            deps = line.contains("dependencies");
            continue;
        }
        if deps && line.contains('=') && !line.starts_with('#') {
            let local = line.contains("path") && !line.contains("version") && !line.contains("git");
            if !local {
                return false;
            }
        }
    }
    true
}

/// Words that start a command line without being the program: skipped to find it.
const WRAPPERS: &[&str] = &["time", "nice", "command", "env", "exec", "sudo", "timeout"];

/// Whether `cmd` runs a build or a test of a project: any command of the line (split at `&&`, `||`, `;`,
/// `|` and new lines) that starts a known toolchain's compile or test.
pub fn is_check_command(cmd: &str) -> bool {
    cmd.split(['\n', ';', '|', '&']).any(segment_checks)
}

fn segment_checks(seg: &str) -> bool {
    let mut words = seg.split_whitespace().filter(|w| !w.contains('=') || w.starts_with('-'));
    let mut first = words.next();
    while first.is_some_and(|w| WRAPPERS.contains(&w) || w.parse::<f64>().is_ok() || w.starts_with('-')) {
        first = words.next();
    }
    let prog = first.map(|w| w.rsplit('/').next().unwrap_or(w));
    let rest: Vec<&str> = words.filter(|w| !w.starts_with('-') && !w.starts_with('+')).collect();
    let sub = rest.first().copied();
    match prog {
        Some("cargo") => matches!(sub, Some("test" | "check" | "build" | "run" | "clippy" | "nextest" | "bench" | "t")),
        Some("pytest" | "py.test" | "tox" | "nosetests") => true,
        Some("python" | "python3") => {
            // `python -m pytest`, `python -m unittest`: the `-m` was filtered with the other flags
            seg.contains(" -m pytest") || seg.contains(" -m unittest")
        }
        Some("npm" | "pnpm" | "yarn" | "bun") => {
            matches!(sub, Some("test" | "t" | "build"))
                || (sub == Some("run")
                    && matches!(rest.get(1), Some(s) if s.contains("test") || s.contains("build") || s.contains("lint")))
        }
        Some("npx") => matches!(sub, Some("jest" | "vitest" | "tsc" | "mocha")),
        Some("go") => matches!(sub, Some("test" | "vet" | "build")),
        Some("make") => matches!(sub, Some("test" | "check" | "all") | None),
        Some("mvn" | "gradle" | "gradlew" | "./gradlew") => matches!(sub, Some("test" | "build" | "check")),
        Some("dotnet") => matches!(sub, Some("test" | "build")),
        Some("tsc") => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (n, t) in files {
            std::fs::write(d.path().join(n), t).unwrap();
        }
        d
    }

    #[test]
    fn cargo_check_command_carries_no_fail_fast() {
        let c = dir_with(&[("Cargo.toml", "[package]\nname = \"t\"\n[dependencies]\nserde = \"1\"\n")]);
        assert_eq!(check_command(c.path()).unwrap().command, "cargo test -q --no-fail-fast");
        let c = dir_with(&[("Cargo.toml", "[package]\nname = \"t\"\n")]);
        assert_eq!(check_command(c.path()).unwrap().command, "cargo test -q --offline --no-fail-fast");
        assert!(is_check_command("cargo test -q --offline --no-fail-fast"));
    }

    #[test]
    fn detects_cargo_npm_pytest_and_go_check_commands() {
        let c = dir_with(&[("Cargo.toml", "[package]\nname = \"t\"\nversion = \"0.1.0\"\n")]);
        let got = check_command(c.path()).unwrap();
        assert_eq!(
            (got.command.as_str(), got.manifest, got.dir.as_path()),
            ("cargo test -q --offline --no-fail-fast", "Cargo.toml", c.path())
        );
        // a registry dependency: the build may need the network
        let c = dir_with(&[("Cargo.toml", "[package]\nname = \"t\"\n[dependencies]\nserde = \"1\"\n")]);
        assert_eq!(check_command(c.path()).unwrap().command, "cargo test -q --no-fail-fast");
        // a path dependency alone stays offline
        let c = dir_with(&[("Cargo.toml", "[package]\nname = \"t\"\n[dependencies]\nshop = { path = \"../shop\" }\n")]);
        assert_eq!(check_command(c.path()).unwrap().command, "cargo test -q --offline --no-fail-fast");

        let n = dir_with(&[("package.json", r#"{"name":"x","scripts":{"test":"jest"}}"#)]);
        assert_eq!(check_command(n.path()).unwrap().command, "npm test -s");
        let n = dir_with(&[("package.json", r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#)]);
        assert_eq!(check_command(n.path()), None);
        let n = dir_with(&[("package.json", r#"{"scripts":{"build":"tsc"}}"#)]);
        assert_eq!(check_command(n.path()), None);

        for m in ["pyproject.toml", "pytest.ini", "setup.cfg"] {
            let p = dir_with(&[(m, "")]);
            assert_eq!(check_command(p.path()).unwrap().command, "pytest -q -x", "{m}");
        }
        let g = dir_with(&[("go.mod", "module x\n")]);
        assert_eq!(check_command(g.path()).unwrap().command, "go test ./...");

        // the nearest manifest above a nested folder, and none in a folder without one
        let root = dir_with(&[("go.mod", "module x\n")]);
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        assert_eq!(check_command(&root.path().join("a/b")).unwrap().dir, root.path());
        assert_eq!(check_command_within(&root.path().join("a/b"), Some(&root.path().join("a"))), None);
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(check_command_within(empty.path(), Some(empty.path())), None);
    }

    #[test]
    fn shell_lines_that_build_or_test_are_checks() {
        for yes in [
            "cargo test 2>&1 | tail -80",
            "cd /tmp/x && cargo test 2>&1 | grep -E \"test result|FAILED\"",
            "cargo build -q 2>&1 | tail -20",
            "mkdir -p tests && cat > tests/t.rs <<'EOF'\nuse shop::tax;\nEOF\ncargo test --offline",
            "RUST_BACKTRACE=1 cargo test -p shop",
            "pytest -q -x tests/",
            "python3 -m pytest -q",
            "npm test -s",
            "npm run test",
            "go test ./... && go vet ./...",
            "time cargo check --all-targets",
        ] {
            assert!(is_check_command(yes), "{yes}");
        }
        for no in [
            "ls src",
            "cat Cargo.toml",
            "sed -n 1,20p src/lib.rs",
            "sqlite3 :memory: \".mode csv\"",
            "echo cargo test",
            "grep -rn test .",
        ] {
            assert!(!is_check_command(no), "{no}");
        }
    }

    /// The shell commands that followed an edit in recorded runs (one per line in the file named by
    /// `NEXO_REPLAY_CMDS`, `\n` for new lines): at least 90 % of those that run cargo, pytest or npm test are
    /// recognised. Skipped when the variable is not set.
    #[test]
    fn replayed_commands_after_an_edit_are_recognised_as_checks() {
        let Some(file) = std::env::var_os("NEXO_REPLAY_CMDS") else { return };
        let text = std::fs::read_to_string(file).unwrap();
        let mention = regex_lite_mentions;
        let (mut total, mut seen) = (0, 0);
        for line in text.lines() {
            let cmd = line.replace("\\n", "\n");
            if mention(&cmd) {
                total += 1;
                if is_check_command(&cmd) {
                    seen += 1;
                } else {
                    eprintln!("not recognised: {}", cmd.lines().next().unwrap_or(""));
                }
            }
        }
        eprintln!("replay: {seen} of {total} recognised");
        assert!(total > 0 && seen * 10 >= total * 9, "{seen} of {total}");
    }

    fn regex_lite_mentions(cmd: &str) -> bool {
        cmd.contains("cargo test")
            || cmd.contains("cargo build")
            || cmd.contains("cargo check")
            || cmd.contains("cargo run")
            || cmd.contains("pytest")
            || cmd.contains("npm test")
    }
}
