//! The vulnerability hunt: a small document-store crate with a request-handler layer, four real
//! security flaws and two lookalikes that are correct. The whole crate fits in the context, so the
//! task is long in what the agent must read and short in what it must do: a handful of tool steps,
//! all of them depending on having the full picture in view.

use std::collections::BTreeSet;
use std::path::Path;

use regex::Regex;

use super::{always, manifest, Task};
use crate::check::{read, Copy, Outcome, Part};

/// The vulnerability hunt.
pub fn tasks() -> Vec<Task> {
    vec![vuln_hunt()]
}

/// The library, one module per file (`src/lib.rs` first).
const SRC: [(&str, &str); 14] = [
    ("src/lib.rs", include_str!("vault/src/lib.rs")),
    ("src/audit.rs", include_str!("vault/src/audit.rs")),
    ("src/auth.rs", include_str!("vault/src/auth.rs")),
    ("src/error.rs", include_str!("vault/src/error.rs")),
    ("src/export.rs", include_str!("vault/src/export.rs")),
    ("src/files.rs", include_str!("vault/src/files.rs")),
    ("src/handler.rs", include_str!("vault/src/handler.rs")),
    ("src/query.rs", include_str!("vault/src/query.rs")),
    ("src/quota.rs", include_str!("vault/src/quota.rs")),
    ("src/service.rs", include_str!("vault/src/service.rs")),
    ("src/store.rs", include_str!("vault/src/store.rs")),
    ("src/text.rs", include_str!("vault/src/text.rs")),
    ("src/users.rs", include_str!("vault/src/users.rs")),
    ("src/wire.rs", include_str!("vault/src/wire.rs")),
];

/// The visible tests, all green at the start.
const TESTS: [(&str, &str); 9] = [
    ("tests/common/mod.rs", include_str!("vault/tests/common/mod.rs")),
    ("tests/auth.rs", include_str!("vault/tests/auth.rs")),
    ("tests/export.rs", include_str!("vault/tests/export.rs")),
    ("tests/files.rs", include_str!("vault/tests/files.rs")),
    ("tests/handler.rs", include_str!("vault/tests/handler.rs")),
    ("tests/query.rs", include_str!("vault/tests/query.rs")),
    ("tests/service.rs", include_str!("vault/tests/service.rs")),
    ("tests/text.rs", include_str!("vault/tests/text.rs")),
    ("tests/wire.rs", include_str!("vault/tests/wire.rs")),
];

/// The modules with a real flaw: a path joined without a check (`files.rs`), a user value pasted
/// into a filter (`query.rs`), a delete without an ownership check (`service.rs`) and a length from
/// the header trusted for a slice (`wire.rs`). Sorted.
const FLAWED: [&str; 4] = ["files.rs", "query.rs", "service.rs", "wire.rs"];

/// The lookalikes that are correct: a hand-written constant-time compare (`auth.rs`) and a file path
/// built from a title that `sanitize_name` has already reduced to `[a-z0-9-]` (`export.rs`).
#[cfg(test)]
const DECOYS: [&str; 2] = ["auth.rs", "export.rs"];

/// The crate as the request finds it.
fn crate_files() -> Vec<(String, String)> {
    let mut out = vec![("Cargo.toml".to_string(), manifest("vault", ""))];
    out.extend(SRC.iter().chain(TESTS.iter()).map(|(p, t)| (p.to_string(), t.to_string())));
    out
}

fn turns() -> [String; 1] {
    ["Please audit the Rust crate in this folder, a small multi-user document store called vault, for security \
         vulnerabilities. Fix every real one in src/ with the smallest change that closes it, without breaking the \
         existing tests or the public API (the hash and token primitives are stand-ins: leave their strength \
         alone). Run `cargo test` to confirm. Then write SECURITY.md listing, for each vulnerability you fixed, the \
         file name of the module it was in (like `name.rs`) and one line on the fix. Do not list files you did not \
         fix."
        .into()]
}

fn vuln_hunt() -> Task {
    Task {
        id: "vuln-hunt",
        trap: "a wide audit of a whole small crate: four real flaws in four modules among lookalikes that are \
               correct, so the answer needs the entire codebase in view and a careless fix-everything breaks the \
               tests",
        group: "long",
        files: crate_files(),
        turns: turns().into_iter().map(|t| (1, t)).collect(),
        check: |wd| Outcome::all(vault_parts(wd)),
        solution: vault_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

/// One test per flaw; each asserts that the exploit is now refused.
const EXPLOIT_PROBE: &str = r##"use std::path::PathBuf;

use vault::files::FileRoot;
use vault::query::search;
use vault::service::Service;
use vault::store::Store;
use vault::users::Role;
use vault::wire::Frame;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("vault-hidden-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn hidden_path_traversal() {
    let dir = Scratch::new("traversal");
    let base = dir.0.join("pub");
    std::fs::create_dir_all(&base).unwrap();
    let secret = dir.0.join("secret.txt");
    std::fs::write(&secret, "top secret").unwrap();
    let root = FileRoot::new(base.clone());
    root.write("ok.txt", b"fine").unwrap();
    root.write("notes/a.txt", b"nested").unwrap();
    assert_eq!(root.read("ok.txt").unwrap(), b"fine", "a plain name must keep working");
    assert!(root.read("../secret.txt").is_err(), "../secret.txt was served");
    assert!(root.read("notes/../../secret.txt").is_err(), "notes/../../secret.txt was served");
    assert!(root.read(secret.to_str().unwrap()).is_err(), "an absolute path was served");
    assert!(root.write("../planted.txt", b"x").is_err(), "../planted.txt was written");
    let absolute = dir.0.join("planted2.txt");
    assert!(root.write(absolute.to_str().unwrap(), b"x").is_err(), "an absolute path was written");
    assert!(!dir.0.join("planted.txt").exists() && !absolute.exists(), "a file was planted outside the base");
}

#[test]
fn hidden_filter_injection() {
    let mut store = Store::new();
    let mine = store.insert(1, "alpha plan", "x", vec![], 1);
    let theirs = store.insert(2, "secret payroll", "y", vec![], 2);
    assert_eq!(search(&store, 1, "alpha").unwrap(), vec![mine], "an honest search must keep working");
    for term in ["zzz\" OR title ~ \"", "\" OR owner > 0 OR title ~ \"", "x\" OR owner = 2 OR title ~ \"x"] {
        if let Ok(ids) = search(&store, 1, term) {
            assert!(!ids.contains(&theirs), "the term {term:?} found another user's document");
        }
    }
}

#[test]
fn hidden_delete_without_ownership() {
    let mut s = Service::new(10_000);
    let root = s.register("root", Role::Admin, "root-secret1").unwrap();
    let alice = s.register("alice", Role::Member, "alice-secret1").unwrap();
    let bob = s.register("bob", Role::Member, "bob-secret1").unwrap();
    let doc = s.create(alice, "Plan", "private", vec![]).unwrap();
    assert!(s.delete(bob, doc).is_err(), "bob deleted alice's document");
    assert!(s.store.get(doc).is_some(), "the document is gone after a refused delete");
    assert_eq!(s.quota.used(alice), 7, "the quota changed after a refused delete");
    assert!(s.delete(root, doc).is_ok(), "an admin must still be able to delete");
}

#[test]
fn hidden_oversized_length() {
    let frame = |len: u32, rest: &[u8]| {
        let mut v = b"VLT1".to_vec();
        v.extend([1, 0]);
        v.extend(len.to_be_bytes());
        v.extend(rest);
        v
    };
    assert!(Frame::decode(&frame(3, &[1, 2, 3, 6])).is_ok(), "an honest frame must keep decoding");
    for len in [u32::MAX, 1 << 24, 11, 4] {
        let buf = frame(len, &[1, 2, 3, 6]);
        let got = std::panic::catch_unwind(|| Frame::decode(&buf));
        assert!(matches!(got, Ok(Err(_))), "a length of {len} was not refused with an error: {got:?}");
    }
}
"##;

/// The hidden tests, with what each one's failure means.
const EXPLOITS: [(&str, &str); 4] = [
    ("hidden_path_traversal", "files outside the base folder can still be read or written with .. or an absolute path"),
    ("hidden_filter_injection", "a search term can still change the meaning of the filter"),
    ("hidden_delete_without_ownership", "a user can still delete a document that is not theirs"),
    ("hidden_oversized_length", "a frame with a bad length field still panics or is accepted"),
];

/// Every module file name (`name.rs`) of the library.
fn modules() -> Vec<&'static str> {
    SRC.iter().filter_map(|(p, _)| p.strip_prefix("src/")).collect()
}

/// Whether SECURITY.md names exactly the four flawed modules, or what is wrong. A decoy named as
/// fixed counts as wrong; names of test files and of files outside src/ are ignored.
fn security_notes(wd: &Path) -> Part {
    let notes = read(wd, "SECURITY.md");
    if notes.trim().is_empty() {
        return (false, "SECURITY.md is missing or empty".into());
    }
    let names = Regex::new(r"[A-Za-z0-9_./\\-]*\.rs\b").expect("a valid pattern");
    let known = modules();
    let named: BTreeSet<&str> = names
        .find_iter(&notes)
        .map(|m| m.as_str())
        .filter(|t| !t.contains("tests/") && !t.contains("tests\\"))
        .filter_map(|t| t.rsplit(['/', '\\']).next())
        .filter_map(|b| known.iter().find(|k| **k == b).copied())
        .collect();
    let want: BTreeSet<&str> = FLAWED.into_iter().collect();
    let missing: Vec<&&str> = want.difference(&named).collect();
    let extra: Vec<&&str> = named.difference(&want).collect();
    (
        missing.is_empty() && extra.is_empty(),
        format!(
            "SECURITY.md should name exactly the modules with a flaw: missing {missing:?}, wrongly named {extra:?}"
        ),
    )
}

/// The parts of the verdict: the visible tests, one exploit probe per flaw, SECURITY.md.
fn vault_parts(wd: &Path) -> Vec<Part> {
    let c = match Copy::of(wd) {
        Ok(c) if wd.is_dir() => c,
        _ => return vec![(false, "the working directory is gone".into())],
    };
    // The visible tests are graded as shipped, so editing them cannot make a fix pass.
    for (rel, text) in TESTS {
        if let Err(e) = c.write(rel, text) {
            return vec![(false, format!("cannot restore {rel}: {e}"))];
        }
    }
    let mut parts = vec![c.tests_pass()];
    parts.extend(c.hidden("zz_hidden_exploits", EXPLOIT_PROBE, &EXPLOITS));
    parts.push(security_notes(wd));
    parts
}

/// Replaces `old` by `new` in the file `rel` under `wd`.
fn edit(wd: &Path, rel: &str, old: &str, new: &str) {
    let p = wd.join(rel);
    let src = crate::check::text(&p);
    assert!(src.contains(old), "{rel} has no {old:?}");
    std::fs::write(p, src.replacen(old, new, 1)).expect("a writable working directory");
}

/// The attachment name may only hold plain path components.
fn fix_files(wd: &Path) {
    edit(wd, "src/files.rs", "use std::path::{Path, PathBuf};", "use std::path::{Component, Path, PathBuf};");
    edit(
        wd,
        "src/files.rs",
        "        Ok(self.base.join(name))\n",
        "        if !Path::new(name).components().all(|c| matches!(c, Component::Normal(_))) {\n            return Err(VaultError::Invalid(format!(\"{name:?} leaves the attachments folder\")));\n        }\n        Ok(self.base.join(name))\n",
    );
}

/// A search term may not hold the quote that ends the text in the filter.
fn fix_query(wd: &Path) {
    edit(
        wd,
        "src/query.rs",
        "    Ok(format!(\"owner = {owner} AND title ~ \\\"{term}\\\"\"))",
        "    if term.contains('\"') {\n        return Err(VaultError::Invalid(\"the search term may not hold a double quote\".into()));\n    }\n    Ok(format!(\"owner = {owner} AND title ~ \\\"{term}\\\"\"))",
    );
}

/// A delete is checked against the owner before anything is removed.
fn fix_service(wd: &Path) {
    edit(
        wd,
        "src/service.rs",
        "        self.require_writer(user)?;\n        let doc = self.store.remove(id).ok_or_else(|| Self::not_found(id))?;\n",
        "        self.require_writer(user)?;\n        let doc = self.store.get(id).ok_or_else(|| Self::not_found(id))?;\n        if !self.can_access(user, doc) {\n            return Err(VaultError::Denied(format!(\"document {id} belongs to someone else\")));\n        }\n        let doc = self.store.remove(id).ok_or_else(|| Self::not_found(id))?;\n",
    );
}

/// The length in the header is checked against the payload limit and the bytes there are.
fn fix_wire(wd: &Path) {
    edit(
        wd,
        "src/wire.rs",
        "        let end = HEADER_LEN + len;\n",
        "        let end = HEADER_LEN + len;\n        if len > MAX_PAYLOAD || buf.len() <= end {\n            return Err(VaultError::Malformed(format!(\"the header announces {len} payload bytes, the frame holds fewer\")));\n        }\n",
    );
}

/// SECURITY.md for the four fixes.
fn write_notes(wd: &Path) {
    let notes = "# Security fixes\n\n\
        - `files.rs`: path traversal; a name with `..` or an absolute path is now refused before it is joined to the base folder.\n\
        - `query.rs`: filter injection; a search term with a double quote is refused, so it cannot end the text early.\n\
        - `service.rs`: missing authorization; `delete` checks that the caller owns the document or is an admin.\n\
        - `wire.rs`: unchecked length; the payload length is checked against the limit and the bytes present, and a bad one is an error.\n";
    std::fs::write(wd.join("SECURITY.md"), notes).expect("a writable working directory");
}

fn vault_solution(wd: &Path) {
    fix_files(wd);
    fix_query(wd);
    fix_service(wd);
    fix_wire(wd);
    write_notes(wd);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The indexes of the parts that fail.
    fn failing(wd: &Path) -> Vec<usize> {
        vault_parts(wd).iter().enumerate().filter(|(_, (ok, _))| !ok).map(|(i, _)| i).collect()
    }

    #[test]
    fn the_crate_is_wide_and_hides_nothing_about_the_count() {
        let lines: usize = crate_files().iter().map(|(_, t)| t.lines().count()).sum();
        assert!((1500..=2500).contains(&lines), "{lines}");
        assert_eq!(modules().len() - 1, 13);
        assert!(FLAWED.iter().chain(&DECOYS).all(|m| modules().contains(m)));
        assert!(FLAWED.windows(2).all(|w| w[0] < w[1]));
        let ask = &turns()[0];
        assert!(!ask.contains("four") && !ask.contains('4') && !ask.contains("decoy"));
        assert_eq!(vuln_hunt().turns.len(), 1);
    }

    #[test]
    fn security_notes_want_exactly_the_flawed_modules() {
        let wd = tempfile::tempdir().unwrap();
        let notes = |text: &str| {
            std::fs::write(wd.path().join("SECURITY.md"), text).unwrap();
            security_notes(wd.path()).0
        };
        assert!(!security_notes(wd.path()).0);
        assert!(notes("wire.rs, service.rs, query.rs, files.rs"));
        assert!(notes(
            "- `src/files.rs`: x\n- `src\\query.rs`.\n- service.rs: y\n- **wire.rs**; tests/auth.rs got a test"
        ));
        assert!(!notes("files.rs query.rs service.rs"));
        assert!(!notes("files.rs query.rs service.rs wire.rs auth.rs"));
        assert!(!notes("files.rs query.rs service.rs wire.rs src/export.rs"));
        assert!(!notes("files.rs query.rs service.rs wire.rs lib.rs"));
        assert!(!notes("nothing to see"));
    }

    /// Slow: builds the crate several times.
    #[test]
    fn each_fix_closes_exactly_its_exploit_and_the_solution_passes() {
        let task = vuln_hunt();
        let wd = tempfile::tempdir().unwrap();
        task.materialize(wd.path()).unwrap();
        // the visible tests pass; the four exploits and SECURITY.md do not
        assert_eq!(failing(wd.path()), [1, 2, 3, 4, 5], "{}", (task.check)(wd.path()).detail);
        assert!(!(task.check)(wd.path()).ok);
        fix_files(wd.path());
        assert_eq!(failing(wd.path()), [2, 3, 4, 5]);
        fix_query(wd.path());
        assert_eq!(failing(wd.path()), [3, 4, 5]);
        fix_service(wd.path());
        assert_eq!(failing(wd.path()), [4, 5]);
        fix_wire(wd.path());
        assert_eq!(failing(wd.path()), [5]);
        write_notes(wd.path());
        assert!(failing(wd.path()).is_empty());
        let fresh = tempfile::tempdir().unwrap();
        task.materialize(fresh.path()).unwrap();
        (task.solution)(fresh.path());
        let got = (task.check)(fresh.path());
        assert!(got.ok, "{}", got.detail);
        assert_eq!(got.score, 1.0);
    }

    /// Slow: a careless fix of the lookalike file check breaks a visible test, and editing the
    /// visible tests does not hide it.
    #[test]
    fn a_careless_fix_breaks_the_visible_tests_even_when_they_are_edited() {
        let wd = tempfile::tempdir().unwrap();
        vuln_hunt().materialize(wd.path()).unwrap();
        edit(
            wd.path(),
            "src/files.rs",
            "        Ok(self.base.join(name))\n",
            "        if name.contains(\"..\") || name.starts_with('/') {\n            return Err(VaultError::Invalid(\"no\".into()));\n        }\n        Ok(self.base.join(name))\n",
        );
        let at = |p: &str| wd.path().join(p);
        std::fs::write(at("tests/files.rs"), "").unwrap();
        let parts = vault_parts(wd.path());
        assert!(!parts[0].0, "the visible tests passed after a careless fix");
        assert!(parts[1].0, "{}", parts[1].1);
    }
}
