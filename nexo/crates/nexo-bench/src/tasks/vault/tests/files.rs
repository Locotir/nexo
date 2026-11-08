mod common;

use vault::error::VaultError;
use vault::files::*;

#[test]
fn an_attachment_comes_back_as_written() {
    let scratch = common::Scratch::new("files");
    let root = FileRoot::new(scratch.dir.join("att"));
    root.write("a.txt", b"hello").unwrap();
    assert_eq!(root.read("a.txt").unwrap(), b"hello");
    root.write("a.txt", b"again").unwrap();
    assert_eq!(root.read("a.txt").unwrap(), b"again");
    assert!(root.exists("a.txt") && !root.exists("b.txt"));
}

#[test]
fn names_may_hold_folders_and_dots() {
    let scratch = common::Scratch::new("files");
    let root = FileRoot::new(scratch.dir.join("att"));
    root.write("notes/2026/a.txt", b"1").unwrap();
    root.write("report..final.txt", b"2").unwrap();
    root.write("v1.2.3.tar.gz", b"3").unwrap();
    assert_eq!(root.read("report..final.txt").unwrap(), b"2");
    assert_eq!(root.read("notes/2026/a.txt").unwrap(), b"1");
    assert_eq!(root.list().unwrap(), ["notes/2026/a.txt", "report..final.txt", "v1.2.3.tar.gz"]);
}

#[test]
fn a_missing_attachment_is_not_found() {
    let scratch = common::Scratch::new("files");
    let root = FileRoot::new(scratch.dir.join("att"));
    assert!(matches!(root.read("nope.txt"), Err(VaultError::NotFound(_))));
    assert!(root.list().unwrap().is_empty());
}

#[test]
fn removing_an_attachment_removes_it() {
    let scratch = common::Scratch::new("files");
    let root = FileRoot::new(scratch.dir.join("att"));
    root.write("a.txt", b"x").unwrap();
    root.remove("a.txt").unwrap();
    assert!(!root.exists("a.txt"));
    assert!(root.remove("a.txt").is_err());
}

#[test]
fn odd_names_and_big_files_are_refused() {
    let scratch = common::Scratch::new("files");
    let root = FileRoot::new(scratch.dir.join("att"));
    assert!(root.resolve("   ").is_err());
    assert!(root.resolve("a\0b").is_err());
    assert!(root.resolve(&"n".repeat(MAX_PATH + 1)).is_err());
    assert!(root.write("big.bin", &vec![0u8; MAX_FILE + 1]).is_err());
    assert!(root.write("ok.bin", &vec![0u8; MAX_FILE]).is_ok());
    assert_eq!(root.resolve(" a.txt ").unwrap(), root.base().join("a.txt"));
}
