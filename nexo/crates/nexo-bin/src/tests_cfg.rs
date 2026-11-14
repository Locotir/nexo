//! `cfg`: the dominators on hand-built graphs, and the rows of real functions from small C programs the test
//! compiles itself (skipped, with a message, when there is no C compiler).

use serde_json::Value;

use crate::cfg::dominators_and_loops;
use crate::tests_map::{compile, whole};
use crate::{cfg_op, Image, Lines, PageAsk};

// ---------------------------------------------------------------------------------------------
// The graph algorithms, on graphs written by hand

#[test]
fn dominators_of_a_diamond_with_a_tail_and_an_unreachable_block() {
    //   0 -> 1, 2 ; 1 -> 3 ; 2 -> 3 ; 3 -> 4 ; 5 -> 3 (nothing reaches 5)
    let succs = vec![vec![1, 2], vec![3], vec![3], vec![4], vec![], vec![3]];
    let (idom, loops) = dominators_and_loops(&succs, 0);
    assert_eq!(idom, vec![Some(0), Some(0), Some(0), Some(0), Some(3), None]);
    assert!(loops.is_empty());
}

#[test]
fn dominators_and_loops_of_a_nested_loop_with_an_exit_in_the_middle() {
    // 0 -> 1 ; 1 -> 2, 6 ; 2 -> 3 ; 3 -> 4, 5 ; 4 -> 3 ; 5 -> 1 ; 6 ends
    // outer loop: header 1, body 1 2 3 4 5 ; inner loop: header 3, body 3 4
    let succs = vec![vec![1], vec![2, 6], vec![3], vec![4, 5], vec![3], vec![1], vec![]];
    let (idom, loops) = dominators_and_loops(&succs, 0);
    assert_eq!(idom, vec![Some(0), Some(0), Some(1), Some(2), Some(3), Some(3), Some(1)]);
    assert_eq!(loops.len(), 2);
    assert_eq!(loops[&1].iter().copied().collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
    assert_eq!(loops[&3].iter().copied().collect::<Vec<_>>(), vec![3, 4]);
}

#[test]
fn a_join_reached_from_two_branches_of_a_loop_body_is_dominated_by_the_branch_not_by_an_arm() {
    // the classic case where a naive "first predecessor" answer is wrong: 0 -> 1, 2 ; 1 -> 3 ; 2 -> 3, 4 ; 3 -> 4 ;
    // 4 -> 2 (a loop whose header 2 is entered from 0 and from 4: the loop has two entries, 1 is not on it)
    let succs = vec![vec![1, 2], vec![3], vec![3, 4], vec![4], vec![2]];
    let (idom, loops) = dominators_and_loops(&succs, 0);
    assert_eq!(idom, vec![Some(0), Some(0), Some(0), Some(0), Some(0)]);
    // 2 does not dominate 4 (0 -> 1 -> 3 -> 4 avoids it), so 4 -> 2 is not a back edge
    assert!(loops.is_empty(), "{loops:?}");
}

#[test]
fn a_block_that_jumps_to_itself_is_a_loop_of_one() {
    let succs = vec![vec![1], vec![1, 2], vec![]];
    let (idom, loops) = dominators_and_loops(&succs, 0);
    assert_eq!(idom, vec![Some(0), Some(0), Some(1)]);
    assert_eq!(loops[&1].iter().copied().collect::<Vec<_>>(), vec![1]);
}

// ---------------------------------------------------------------------------------------------
// Real functions

const SRC: &str = r#"
__attribute__((noinline)) int sum(int n) { int s = 0; for (int i = 0; i < n; i++) s += i; return s; }
__attribute__((noinline)) int pick(int a) { int r; if (a > 3) r = a * 2; else r = a + 7; return r + 1; }
__attribute__((noinline)) int sw(int c, int v) {
    switch (c) {
    case 0: return v + 10; case 1: return v * 3; case 2: return v - 4;
    case 3: return v ^ 7; case 4: return v << 2; case 5: return v | 9;
    default: return 0;
    }
}
int main(int argc, char **argv) { (void)argv; return sum(argc) + pick(argc) + sw(argc, 1); }
"#;

const FLAGS: &[&str] = &["-O0", "-fno-pie", "-no-pie"];

fn items(o: &crate::Output, kind: &str) -> Vec<Value> {
    o.rows.iter().filter_map(|r| r.json.clone()).filter(|j| j["row"] == kind).collect()
}

fn sample(dir: &std::path::Path) -> Option<Image> {
    compile(dir, SRC, FLAGS).map(|exe| Image::open(&exe).unwrap())
}

#[test]
fn a_function_with_a_loop_shows_the_loop_its_header_and_the_back_edge() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = sample(dir.path()) else { return };
    let o = cfg_op(&img, "sum").unwrap();
    let t = whole(&o);
    println!("{t}");
    let loops = items(&o, "loop");
    assert_eq!(loops.len(), 1, "{t}");
    let header = loops[0]["header"].as_u64().unwrap();
    assert!(t.contains(&format!("loop header {header:#x}")), "{t}");
    // the header holds the test of the `for`: it is a block with both conditional edges
    let edges = items(&o, "edge");
    let kinds_from_header: Vec<&str> =
        edges.iter().filter(|e| e["from"] == header).map(|e| e["kind"].as_str().unwrap()).collect();
    assert!(
        kinds_from_header.contains(&"cond-true") && kinds_from_header.contains(&"cond-false"),
        "{kinds_from_header:?}\n{t}"
    );
    // a back edge returns to the header from a block inside the loop, and says so
    let back: Vec<&Value> = edges.iter().filter(|e| e["back"] == true).collect();
    assert_eq!(back.len(), 1, "{t}");
    assert_eq!(back[0]["to"], header);
    let body: Vec<u64> = loops[0]["blocks"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap()).collect();
    assert!(body.len() >= 2 && body.contains(&back[0]["from"].as_u64().unwrap()), "{t}");
    assert!(t.contains("; back edge"), "{t}");
    // the blocks inside the loop name their loop; the entry names none and has no dominator
    let blocks = items(&o, "block");
    for b in &blocks {
        let start = b["start"].as_u64().unwrap();
        assert_eq!(b["loop"] == header, body.contains(&start), "{start:#x}\n{t}");
    }
    let entry = blocks.iter().find(|b| b["entry"] == true).unwrap();
    assert!(entry["idom"].is_null() && entry["loop"].is_null(), "{t}");
    // the header is dominated by the entry side, not by a block of the body
    let h = blocks.iter().find(|b| b["start"] == header).unwrap();
    assert!(!body.contains(&h["idom"].as_u64().unwrap()), "{t}");
    // the function ends in a `ret`, and the row says so
    assert!(t.contains("; ends: ret"), "{t}");
}

#[test]
fn an_if_else_shows_both_conditional_edges_and_a_join_dominated_by_the_condition() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = sample(dir.path()) else { return };
    let o = cfg_op(&img, "pick").unwrap();
    let t = whole(&o);
    println!("{t}");
    assert!(items(&o, "loop").is_empty(), "{t}");
    let edges = items(&o, "edge");
    let cond: Vec<&Value> = edges.iter().filter(|e| e["kind"].as_str().unwrap().starts_with("cond-")).collect();
    assert_eq!(cond.len(), 2, "{t}");
    let t_edge = edges.iter().find(|e| e["kind"] == "cond-true").unwrap();
    let f_edge = edges.iter().find(|e| e["kind"] == "cond-false").unwrap();
    let test_block = t_edge["from"].as_u64().unwrap();
    assert_eq!(f_edge["from"].as_u64().unwrap(), test_block, "{t}");
    let (then_arm, else_arm) = (t_edge["to"].as_u64().unwrap(), f_edge["to"].as_u64().unwrap());
    assert!(then_arm != else_arm, "{t}");
    // the join is the block both arms lead to
    let next_of = |from: u64| -> Vec<u64> {
        edges.iter().filter(|e| e["from"] == from).map(|e| e["to"].as_u64().unwrap()).collect()
    };
    let join = next_of(then_arm)
        .into_iter()
        .find(|j| *j == else_arm || next_of(else_arm).contains(j))
        .unwrap_or_else(|| panic!("no join of {then_arm:#x} and {else_arm:#x}\n{t}"));
    let blocks = items(&o, "block");
    let of = |a: u64| blocks.iter().find(|b| b["start"] == a).unwrap_or_else(|| panic!("no block {a:#x}\n{t}"));
    assert_eq!(of(join)["idom"].as_u64(), Some(test_block), "{t}");
    assert!(t.contains(&format!("block {join:#x}")) && t.contains(&format!("idom {test_block:#x}")), "{t}");
    // each arm is dominated by the test block too
    assert_eq!(of(then_arm)["idom"].as_u64(), Some(test_block), "{t}");
    assert!(t.contains("cond-true") && t.contains("cond-false"), "{t}");
}

#[test]
fn a_switch_has_one_edge_per_target_with_its_case_values_when_the_table_is_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let flags = ["-O0", "-fno-pie", "-no-pie", "-fjump-tables", "--param", "case-values-threshold=1"];
    let Some(exe) = compile(dir.path(), SRC, &flags) else { return };
    let img = Image::open(&exe).unwrap();
    let o = cfg_op(&img, "sw").unwrap();
    let t = whole(&o);
    println!("{t}");
    let cases: Vec<Value> =
        items(&o, "edge").into_iter().filter(|e| e["kind"].as_str().unwrap().starts_with("case")).collect();
    assert!(!cases.is_empty(), "the jump table is resolved, so there are case edges\n{t}");
    let mut values: Vec<u64> =
        cases.iter().flat_map(|e| e["cases"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap())).collect();
    values.sort_unstable();
    values.dedup();
    assert_eq!(values, vec![0, 1, 2, 3, 4, 5], "{t}");
    assert!(t.contains("jump tables resolved"), "{t}");
}

#[test]
fn grep_and_from_page_the_rows_of_cfg() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = sample(dir.path()) else { return };
    let o = cfg_op(&img, "sum").unwrap();
    let all = o.rows.len();
    assert!(all >= 10, "{}", whole(&o));
    // --grep keeps the rows that match and says how many of how many
    let ask = |grep: Option<&str>, from: Option<u64>, lines: usize| PageAsk {
        lines: Some(Lines::Rows(lines)),
        from,
        grep: grep.map(String::from),
        json: false,
        sized: false,
    };
    let p = ask(Some("^edge .*cond-"), None, 50).page(&o).unwrap();
    assert_eq!(p.lines[0], format!("... --grep ^edge .*cond-: 2 of {all} rows match"), "{}", p.text());
    assert_eq!(p.lines.len(), 3, "{}", p.text());
    assert!(p.lines[1..].iter().all(|l| l.starts_with("edge ") && l.contains("cond-")));
    let p = ask(Some("^loop "), None, 50).page(&o).unwrap();
    assert_eq!(p.lines.len(), 2, "{}", p.text());
    assert_eq!(
        ask(Some("^nothing"), None, 50).page(&o).unwrap().lines[0],
        format!("... --grep ^nothing: 0 of {all} rows match")
    );
    // --from starts at a row position, and the cut line names the next one, with the same grep kept
    let o2 = cfg_op(&img, "sw").unwrap();
    let all2 = o2.rows.len();
    assert!(all2 > 10, "{}", whole(&o2));
    let first = ask(None, None, 10).page(&o2).unwrap();
    assert_eq!(first.lines.len(), 10);
    let next = first.next.clone().unwrap();
    assert!(next.starts_with("nexo bin cfg ") && next.contains("--from 0x9"), "{next}");
    let rest = ask(None, Some(9), 100).page(&o2).unwrap();
    assert_eq!(rest.lines.len(), all2 - 9, "{}", rest.text());
    assert_eq!(rest.lines[0], o2.rows[9].text);
    // the grep keeps the row positions: `--from 6` skips the first edge row (position 5) and keeps the other three
    let both = ask(Some("^edge"), Some(6), 100).page(&o).unwrap();
    assert_eq!(both.lines[0], format!("... --grep ^edge: 4 of {all} rows match"), "{}", both.text());
    assert_eq!(both.lines.len(), 1 + 3, "{}", both.text());
    assert!(both.lines[1..].iter().all(|l| l.starts_with("edge ")), "{}", both.text());
    // the same rows come back as JSON items
    assert_eq!(first.value["op"], "cfg");
    assert!(first.value["items"].as_array().unwrap().iter().any(|i| i["row"] == "block"));
}

#[test]
fn an_address_inside_the_function_names_it_and_arm64_is_refused_honestly() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = sample(dir.path()) else { return };
    let by_name = whole(&cfg_op(&img, "pick").unwrap());
    let start = img.symbols.iter().find(|s| s.name == "pick").unwrap().addr;
    let by_addr = whole(&cfg_op(&img, "pick+0x4").unwrap());
    assert_eq!(by_name.lines().next(), by_addr.lines().next());
    assert!(by_name.starts_with(&format!("cfg pick {start:#x}..")), "{by_name}");
    // no function there
    assert!(cfg_op(&img, "no_such_function").is_err());
    // AArch64: no graph is better than a wrong one
    let arm = dir.path().join("arm");
    std::fs::write(&arm, crate::a64::fixture::elf(true).bytes).unwrap();
    let img = Image::open(&arm).unwrap();
    let a = img.funcs()[0].start;
    let e = cfg_op(&img, &format!("{a:#x}")).unwrap_err().to_string();
    assert!(e.contains("cfg of") && e.contains("does not read AArch64 code yet"), "{e}");
}

// ---------------------------------------------------------------------------------------------
// Call-return edges and irreducible loops

const CALLS_SRC: &str = r#"
#include <stdio.h>
#include <stdlib.h>
__attribute__((noinline)) int greet(int n) { puts("hello"); if (n > 2) exit(3); return n + 1; }
__attribute__((noinline)) int viacb(int (*f)(int), int n) { return f(n) + 1; }
__attribute__((noinline)) int twice(int a) { return a * 2; }
int main(int argc, char **argv) { (void)argv; return greet(argc) + viacb(twice, argc); }
"#;

const IRR_SRC: &str = r#"
__attribute__((noinline)) int irr(int n, int k) {
    int s = 0;
    if (k) goto inside;
    for (;;) {
        s += n;
        if (s > 1000) return s;
    inside:
        s += 2;
        if (s > n * 50) return s;
    }
}
int main(int argc, char **argv) { (void)argv; return irr(argc, argc - 1); }
"#;

#[test]
fn a_call_gets_a_call_return_edge_named_by_its_callee_and_exit_gets_none() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = compile(dir.path(), CALLS_SRC, FLAGS) else { return };
    let img = Image::open(&exe).unwrap();
    let o = cfg_op(&img, "greet").unwrap();
    let t = whole(&o);
    println!("{t}");
    let ret: Vec<Value> = items(&o, "edge").into_iter().filter(|e| e["kind"] == "call-return").collect();
    assert_eq!(ret.len(), 1, "{t}");
    assert_eq!(ret[0]["callee"], "puts", "{t}");
    let from = ret[0]["from"].as_u64().unwrap();
    assert!(ret[0]["to"].as_u64().unwrap() > from);
    assert!(t.contains(&format!("edge {from:#x} -> {:#x}  call-return (puts)", ret[0]["to"].as_u64().unwrap())), "{t}");
    let calls = items(&o, "call");
    assert_eq!(calls.len(), 1, "{t}");
    assert_eq!(calls[0]["callee"], "exit");
    assert!(t.contains("noreturn (exit)  ; no return edge"), "{t}");
    assert!(t.contains("; ends: noreturn call exit"), "{t}");
    assert!(!t.contains("note: calls do not end"), "{t}");
    // the edge count of the header counts the call-return edge
    let n_edges = o.meta["edges"].as_u64().unwrap() as usize;
    assert_eq!(n_edges, items(&o, "edge").len(), "{t}");
    // an indirect call
    let o = cfg_op(&img, "viacb").unwrap();
    let t = whole(&o);
    assert!(t.contains("call-return (indirect)"), "{t}");
}

#[test]
fn a_goto_into_a_loop_body_is_an_irreducible_loop_with_two_entries() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = compile(dir.path(), IRR_SRC, FLAGS) else { return };
    let img = Image::open(&exe).unwrap();
    let o = cfg_op(&img, "irr").unwrap();
    let t = whole(&o);
    println!("{t}");
    let irr = items(&o, "irreducible");
    assert_eq!(irr.len(), 1, "{t}");
    assert!(irr[0]["blocks"].as_array().unwrap().len() >= 2);
    assert_eq!(irr[0]["entries"].as_array().unwrap().len(), 2, "{t}");
    assert!(t.contains("irreducible loop: blocks 0x") && t.contains("; entries 0x"), "{t}");
    // a reducible function has none
    assert!(items(&cfg_op(&img, "main").unwrap(), "irreducible").is_empty());
}

#[test]
fn irreducible_regions_on_hand_built_graphs() {
    use crate::cfg::irreducible_of;
    // 0 -> 1, 2 ; 1 -> 2 ; 2 -> 1, 3 : the cycle 1 2 is entered at both
    let r = irreducible_of(&[vec![1, 2], vec![2], vec![1, 3], vec![]], 0);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(r[0].1.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
    // a natural loop is not one
    assert!(irreducible_of(&[vec![1], vec![2, 3], vec![1], vec![]], 0).is_empty());
}
