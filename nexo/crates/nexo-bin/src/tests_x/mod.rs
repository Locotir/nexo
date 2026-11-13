//! Tests of the second half: references, the function list, calls, names and notes, and `fn` pages
//! that open at an address or run across functions.

mod fn_pages;
mod funcs_calls;
mod names_notes;
mod pieces;
mod real_binary;
mod review;
mod xref;

use std::path::{Path, PathBuf};

use crate::fixtures::{self, Fixture, STRING};
use crate::tests::this_binary;
use crate::*;

fn open(f: &Fixture) -> Image {
    Image::from_bytes(f.bytes.clone(), PathBuf::from("/fx/prog")).expect("the fixture opens")
}

/// Both xref fixtures, named.
fn both() -> Vec<(&'static str, Fixture)> {
    vec![("elf", fixtures::elf64_xref()), ("pe", fixtures::pe64_xref())]
}

fn hex(a: u64) -> String {
    format!("{a:#x}")
}

fn whole(o: &Output) -> String {
    o.page(None, usize::MAX / 2).text()
}

/// The `refs` figure of a function row in `funcs` JSON: (callers, callees) by address.
fn row_of(img: &Image, addr: u64) -> (u64, u64) {
    let o = func_list(img, FuncSort::Addr, None, 0).unwrap();
    let p = o.page(None, usize::MAX / 2);
    let item = p.value["items"].as_array().unwrap().iter().find(|i| i["addr"] == addr).expect("function listed");
    (item["callers"].as_u64().unwrap(), item["callees"].as_u64().unwrap())
}

/// The first number after `--from ` in a resume command.
fn from_of(cmd: &str) -> u64 {
    parse_addr(cmd.split("--from ").nth(1).unwrap().split(' ').next().unwrap()).unwrap()
}

/// Read an answer piece by piece with the resume command in its last line and check the pieces
/// join to the whole answer. `make(from, budget)` is the op the command line would run.
fn pieces_join(what: &str, make: impl Fn(Option<u64>, usize) -> Output, budgets: &[usize]) {
    let unbounded = make(None, usize::MAX);
    let all = unbounded.text_all();
    assert_eq!(unbounded.tail, 0, "{what}: the unbounded answer builds every row");
    for &budget in budgets {
        let mut joined: Vec<String> = Vec::new();
        let mut from = None;
        let mut pieces = 0;
        loop {
            let o = make(from, budget);
            assert!(
                o.rows.len() <= budget.max(MIN_LINES),
                "{what}: {} rows built for a page of {budget}",
                o.rows.len()
            );
            let p = o.page(from, budget);
            assert!(p.lines.len() <= budget.max(MIN_LINES), "{what}: budget {budget}");
            pieces += 1;
            assert!(pieces < 20_000, "{what}: does not end");
            match p.next {
                Some(cmd) => {
                    joined.extend(p.lines[..p.lines.len() - 1].iter().cloned());
                    let next = from_of(&cmd);
                    assert!(from.is_none_or(|f| next > f), "{what}: the resume does not move on ({cmd})");
                    from = Some(next);
                }
                None => {
                    joined.extend(p.lines);
                    break;
                }
            }
        }
        assert_eq!(joined.join("\n"), all, "{what}: budget {budget}");
    }
}

fn text_of(o: &Output) -> String {
    o.page(None, 1000).text()
}

/// A fresh folder for the data dir of a test, removed when it goes out of scope.
struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn data_dir() -> TempDir {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "nexo-bin-x-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}
