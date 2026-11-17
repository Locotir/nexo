//! FEATURES.md is proof by test name; a renamed test must not leave a dead reference behind.

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

/// The backticked names in the last cell of every table row.
fn proof_names(features: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in features.lines() {
        let Some(row) = line.strip_prefix("| ").and_then(|l| l.trim_end().strip_suffix(" |")) else {
            continue;
        };
        let Some((_, proof)) = row.rsplit_once(" | ") else {
            continue;
        };
        for (i, part) in proof.split('`').enumerate() {
            let is_name = !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if i % 2 == 1 && is_name {
                names.push(part.to_string());
            }
        }
    }
    names
}

#[test]
fn every_test_named_in_features_md_still_exists() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let features = std::fs::read_to_string(crates.join("../FEATURES.md")).expect("FEATURES.md");
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    let source: String = files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).collect();
    let names = proof_names(&features);
    assert!(names.len() > 100, "the table parser found only {} names", names.len());
    let missing: Vec<_> = names.iter().filter(|n| !source.contains(&format!("fn {n}("))).collect();
    assert!(missing.is_empty(), "FEATURES.md names tests that do not exist: {missing:?}");
}
