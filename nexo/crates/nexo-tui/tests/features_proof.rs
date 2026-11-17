//! FEATURES.md is proof by test name; a renamed test must not leave a dead reference behind.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Build output and vendored sources never hold the tests the table names.
            if !matches!(path.file_name().and_then(|n| n.to_str()), Some("target" | ".git")) {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The name of a function declared on this line, if any.
fn declared_fn(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    rest = rest.strip_prefix("pub(crate) ").or_else(|| rest.strip_prefix("pub ")).unwrap_or(rest);
    rest = rest.strip_prefix("async ").unwrap_or(rest);
    let name = rest.strip_prefix("fn ")?.split('(').next()?;
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(name)
}

/// What one source file proves: the functions that are real tests, keyed by the file stem.
/// A file built with `harness = false` has no `#[test]`; its tests are the plain functions it
/// reports itself as `test <name> ... ok`.
#[derive(Default)]
struct Proven {
    by_stem: HashMap<String, HashSet<String>>,
}

impl Proven {
    fn read(&mut self, file: &Path) {
        let Ok(text) = std::fs::read_to_string(file) else {
            return;
        };
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let (mut tests, mut plain, mut reported) = (HashSet::new(), HashSet::new(), HashSet::new());
        let mut test_attribute = false;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("#[") {
                test_attribute |= trimmed.starts_with("#[test") || trimmed.starts_with("#[tokio::test");
                continue;
            }
            if let Some(name) = declared_fn(trimmed) {
                let into = if test_attribute { &mut tests } else { &mut plain };
                into.insert(name.to_string());
            }
            test_attribute = false;
            if let Some(name) = trimmed.split("\"test ").nth(1).and_then(|r| r.split(" ... ok").next()) {
                reported.insert(name.to_string());
            }
        }
        tests.extend(plain.intersection(&reported).cloned());
        self.by_stem.entry(stem).or_default().extend(tests);
    }

    fn has(&self, module: Option<&str>, name: &str) -> bool {
        match module {
            Some(stem) => self.by_stem.get(stem).is_some_and(|t| t.contains(name)),
            None => self.by_stem.values().any(|t| t.contains(name)),
        }
    }
}

/// The backticked references in the last cell of every table row.
fn proof_cells(features: &str) -> Vec<String> {
    let mut cells = Vec::new();
    for line in features.lines() {
        let Some(row) = line.strip_prefix("| ").and_then(|l| l.trim_end().strip_suffix(" |")) else {
            continue;
        };
        let Some((_, proof)) = row.rsplit_once(" | ") else {
            continue;
        };
        cells.extend(proof.split('`').skip(1).step_by(2).map(str::to_string));
    }
    cells
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[test]
fn every_test_named_in_features_md_still_exists() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let features = std::fs::read_to_string(workspace.join("FEATURES.md")).expect("FEATURES.md");
    let mut files = Vec::new();
    rust_files(&workspace.join("crates"), &mut files);
    let mut proven = Proven::default();
    for file in &files {
        proven.read(file);
    }
    let (mut checked, mut missing) = (0, Vec::new());
    for cell in proof_cells(&features) {
        // A module-qualified name must live in a file of that name; a source path must exist.
        let reference = match cell.rsplit_once("::") {
            Some((module, name)) if is_identifier(module) && is_identifier(name) => Some((Some(module), name)),
            _ if is_identifier(&cell) => Some((None, cell.as_str())),
            _ => None,
        };
        if let Some((module, name)) = reference {
            checked += 1;
            if !proven.has(module, name) {
                missing.push(cell.clone());
            }
        } else if cell.starts_with("crates/") || cell.ends_with(".rs") {
            checked += 1;
            if !workspace.join(&cell).exists() {
                missing.push(cell.clone());
            }
        }
    }
    assert!(checked > 100, "the table parser found only {checked} references");
    assert!(missing.is_empty(), "FEATURES.md names tests or files that do not exist: {missing:?}");
}
