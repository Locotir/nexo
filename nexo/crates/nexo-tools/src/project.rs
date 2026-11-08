//! What kind of project a folder holds, read from its manifests: the command that checks it.

use std::path::Path;

/// The command that tests the project at `dir`, from the nearest manifest at or above it
/// (`Cargo.toml`, `package.json` with a `test` script, Python config, `go.mod`).
pub fn check_command(dir: &Path) -> Option<String> {
    dir.ancestors().find_map(|d| {
        if d.join("Cargo.toml").is_file() {
            return Some("cargo test -q".to_string());
        }
        if let Ok(text) = std::fs::read_to_string(d.join("package.json")) {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            if v.pointer("/scripts/test").and_then(|t| t.as_str()).is_some() {
                return Some("npm test -s".to_string());
            }
        }
        if ["pyproject.toml", "pytest.ini", "setup.cfg"].iter().any(|f| d.join(f).is_file()) {
            return Some("pytest -q -x".to_string());
        }
        d.join("go.mod").is_file().then(|| "go test ./...".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cargo_npm_pytest_and_go_check_commands() {
        let t = |file: &str, body: &str| {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join(file), body).unwrap();
            std::fs::create_dir(d.path().join("sub")).unwrap();
            check_command(&d.path().join("sub"))
        };
        assert_eq!(t("Cargo.toml", "[package]").as_deref(), Some("cargo test -q"));
        assert_eq!(t("package.json", r#"{"scripts":{"test":"jest"}}"#).as_deref(), Some("npm test -s"));
        assert_eq!(t("package.json", "{}"), None);
        assert_eq!(t("pytest.ini", "").as_deref(), Some("pytest -q -x"));
        assert_eq!(t("go.mod", "module x").as_deref(), Some("go test ./..."));
    }
}
