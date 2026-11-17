//! `cfg`: the control-flow graph of one function as rows with addresses: its basic blocks, the edges between
//! them, its loops and the immediate dominator of each block.
//!
//! The blocks are the decompiler's own: [`crate::decomp::lift::lift`] finds them (recursive descent from the
//! entry, split at branch targets and after branches, jump tables resolved), and the dominators and loops come
//! from [`crate::decomp::structure`], the code that structures the decompiler's output. Nothing here splits
//! code or decodes it a second time except to count the instructions of each block.
//!
//! x86-64 goes through the lifter; AArch64 builds the same [`Model`] from the decoder's own flow (`a64/cfg.rs`), and
//! one renderer prints both. x86-32 is refused by [`Image::require_x86`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Value};

use crate::decomp::api::{abi_of, plain_name, ImgEnv};
use crate::decomp::ir::{Expr, Stmt, Target, Term};
use crate::decomp::lift::{is_noreturn, lift, Lifted};
use crate::decomp::structure::{dominates, idoms, irreducible_regions, natural_loops};
use crate::ops::{command, resolve};
use crate::page::shell_word;
use crate::{Error, Image, Output, Result, Row};

/// Calls named on a block's row before the rest is counted.
const CALLS_SHOWN: usize = 4;
/// Blocks named on a loop's row before the rest is counted.
const LOOP_BLOCKS_SHOWN: usize = 12;

/// What one block's last instruction does, as the edge it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EdgeKind {
    /// The next instruction starts another block: no branch instruction.
    Fallthrough,
    /// An unconditional `jmp`.
    Jump,
    /// A conditional branch that is taken.
    CondTrue,
    /// A conditional branch that is not taken (the next instruction).
    CondFalse,
    /// A jump through a table that was resolved: the case values that land here.
    Case(Vec<u64>),
}

impl EdgeKind {
    fn word(&self) -> String {
        match self {
            EdgeKind::Fallthrough => "fallthrough".into(),
            EdgeKind::Jump => "jump".into(),
            EdgeKind::CondTrue => "cond-true".into(),
            EdgeKind::CondFalse => "cond-false".into(),
            EdgeKind::Case(v) => {
                let shown: Vec<String> = v.iter().take(8).map(u64::to_string).collect();
                let more = if v.len() > 8 { format!(",+{}", v.len() - 8) } else { String::new() };
                format!("case {}{more}", shown.join(","))
            }
        }
    }
}

/// The edges of one function by block number (the lifter's), each with its kind, in the order the terminator gives
/// them.
fn graph_of(f: &Lifted, synthetic: &[bool]) -> Vec<Vec<(usize, EdgeKind)>> {
    let mut edges: Vec<Vec<(usize, EdgeKind)>> = Vec::with_capacity(f.blocks.len());
    for (i, b) in f.blocks.iter().enumerate() {
        let e = match &b.term {
            Term::Jump(t) if synthetic[i] => vec![(*t, EdgeKind::Fallthrough)],
            Term::Jump(t) => vec![(*t, EdgeKind::Jump)],
            Term::Branch(_, t, e) => vec![(*t, EdgeKind::CondTrue), (*e, EdgeKind::CondFalse)],
            Term::Switch(_, cases, def) => {
                // one edge per target, with every case value that lands there
                let mut by_target: Vec<(usize, Vec<u64>)> = Vec::new();
                for &(v, t) in cases {
                    match by_target.iter_mut().find(|x| x.0 == t) {
                        Some(x) => x.1.push(v),
                        None => by_target.push((t, vec![v])),
                    }
                }
                let mut out: Vec<(usize, EdgeKind)> =
                    by_target.into_iter().map(|(t, v)| (t, EdgeKind::Case(v))).collect();
                if let Some(d) = def {
                    out.push((*d, EdgeKind::Jump));
                }
                out
            }
            Term::Return(_) | Term::TailCall(_) | Term::Indirect(_) | Term::Stop => Vec::new(),
        };
        edges.push(e);
    }
    edges
}

/// Immediate dominators and natural loops of a graph given by its successor lists: `idom[b]` is `None` for a
/// block that cannot be reached from `entry`, and the entry is its own.
pub(crate) fn dominators_and_loops(
    succs: &[Vec<usize>],
    entry: usize,
) -> (Vec<Option<usize>>, HashMap<usize, BTreeSet<usize>>) {
    let n = succs.len();
    let (idom, rpo) = idoms(n, succs, entry);
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, ss) in succs.iter().enumerate() {
        for &s in ss {
            preds[s].push(b);
        }
    }
    let loops = natural_loops(succs, &preds, &idom, &rpo);
    (idom, loops)
}

/// Irreducible regions of a graph given by its successor lists: the blocks of each and its entries.
pub(crate) fn irreducible_of(succs: &[Vec<usize>], entry: usize) -> Vec<(BTreeSet<usize>, BTreeSet<usize>)> {
    let n = succs.len();
    let (idom, rpo) = idoms(n, succs, entry);
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, ss) in succs.iter().enumerate() {
        for &s in ss {
            preds[s].push(b);
        }
    }
    irreducible_regions(succs, &preds, &idom, &rpo, entry)
}

/// The name a call goes by: its symbol or import, a bare address, or `indirect`.
fn callee_name(img: &Image, c: &crate::decomp::ir::Call) -> String {
    match &c.target {
        Target::Direct(a) => plain_name(img, *a).unwrap_or_else(|| format!("{a:#x}")),
        Target::Import(i) => img.imports.get(*i).map_or_else(|| "import".into(), |x| x.name.clone()),
        Target::Indirect(_) => "indirect".into(),
    }
}

/// The calls of a block with their instruction addresses, in order.
fn call_sites(img: &Image, f: &Lifted, b: usize) -> Vec<(u64, String, bool)> {
    let mut out = Vec::new();
    for s in &f.blocks[b].stmts {
        let e = match &s.st {
            Stmt::Assign(_, e) | Stmt::Eval(e) => e,
            _ => continue,
        };
        let Expr::Call(c) = e else { continue };
        out.push((c.addr, callee_name(img, c), matches!(c.target, Target::Indirect(_))));
    }
    out
}

/// What the block's calls are called, in order, without repeats.
fn calls_in(img: &Image, f: &Lifted, b: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in &f.blocks[b].stmts {
        let e = match &s.st {
            Stmt::Assign(_, e) | Stmt::Eval(e) => e,
            _ => continue,
        };
        let Expr::Call(c) = e else { continue };
        let name = match &c.target {
            Target::Direct(a) => plain_name(img, *a).unwrap_or_else(|| format!("{a:#x}")),
            Target::Import(i) => img.imports.get(*i).map_or_else(|| "import".into(), |x| x.name.clone()),
            Target::Indirect(_) => "indirect".into(),
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// One call of a block: the instruction, where it returns to, what it calls.
pub(crate) struct Site {
    pub at: u64,
    pub ret: u64,
    pub callee: String,
    pub indirect: bool,
}

/// One basic block as the rows print it.
pub(crate) struct Blk {
    pub addr: u64,
    /// One past the block's last instruction (the next block's start for a block with no branch of its own).
    pub end: u64,
    pub insns: usize,
    pub calls: Vec<String>,
    pub sites: Vec<Site>,
    /// What stops the block when it has no edge out (`ret`, a tail call, ...).
    pub ends: Option<String>,
}

/// A function's graph with no architecture left in it: block 0 is the entry, a block outside `start..end` is a
/// place the function leaves to.
pub(crate) struct Model {
    pub name: String,
    pub start: u64,
    pub end: u64,
    pub blocks: Vec<Blk>,
    pub edges: Vec<Vec<(usize, EdgeKind)>>,
    /// Jump tables resolved, and indirect jumps with no known target.
    pub tables: usize,
    pub unresolved: usize,
}

/// The control-flow graph of the function a target names, as rows (see the module text).
pub fn cfg_op(img: &Image, target: &str) -> Result<Output> {
    let _ = img.refs();
    let func = resolve(img, target)?;
    let model = if img.arm64 {
        crate::a64::cfg::model(img, func)?
    } else {
        img.require_x86(&format!("cfg of {} ({:#x})", img.func_name(func), func.start))?;
        x86_model(img, func)?
    };
    render(img, target, model)
}

fn x86_model(img: &Image, func: &crate::funcs::Func) -> Result<Model> {
    let (start, end) = (func.start, func.end);
    let name = img.func_name(func).to_string();
    let lifted = lift(&ImgEnv(img), start, end, abi_of(img));
    let n = lifted.blocks.len();
    if n == 0 || img.read_at(start, 1).is_none() {
        return Err(Error::Bad(format!("{name} {start:#x}: no code to read there")));
    }
    // the instruction starts and lengths of the function, to end each block and count its instructions
    let mut insns: Vec<(u64, u64)> = Vec::new();
    if let Some(bytes) = img.read_at(start, (end - start) as usize) {
        crate::disasm::walk(bytes, start, |i, _| insns.push((i.ip(), i.len() as u64)));
    }
    let len_at = |a: u64| insns.binary_search_by_key(&a, |x| x.0).ok().map(|k| insns[k].1);
    let local: HashSet<u64> =
        lifted.blocks.iter().filter(|b| b.addr >= start && b.addr < end).map(|b| b.addr).collect();
    // a block that ends because the next one starts there has no branch instruction of its own
    let synthetic: Vec<bool> = lifted
        .blocks
        .iter()
        .map(|b| matches!(b.term, Term::Jump(_)) && b.term_addr != b.addr && local.contains(&b.term_addr))
        .collect();
    let count = |lo: u64, hi: u64| -> usize {
        let a = insns.partition_point(|x| x.0 < lo);
        let z = insns.partition_point(|x| x.0 < hi);
        z.saturating_sub(a)
    };
    let mut blocks = Vec::with_capacity(n);
    for (b, bl) in lifted.blocks.iter().enumerate() {
        let hi = if synthetic[b] { bl.term_addr } else { bl.term_addr + len_at(bl.term_addr).unwrap_or(0) };
        let sites: Vec<Site> = call_sites(img, &lifted, b)
            .into_iter()
            .map(|(at, callee, indirect)| Site { at, ret: at + len_at(at).unwrap_or(0), callee, indirect })
            .collect();
        let ends = match &bl.term {
            Term::Return(_) => Some("ret".to_string()),
            Term::TailCall(Expr::Call(c)) => Some(match &c.target {
                Target::Direct(a) => format!("tail call {}", plain_name(img, *a).unwrap_or_else(|| format!("{a:#x}"))),
                Target::Import(i) => format!("tail call {}", img.imports.get(*i).map_or("import", |x| x.name.as_str())),
                Target::Indirect(_) => "tail call (indirect)".into(),
            }),
            Term::TailCall(_) => Some("tail call".into()),
            Term::Indirect(_) => Some("indirect jump, target not resolved".into()),
            Term::Stop => Some(match sites.last() {
                Some(s) if !s.indirect && is_noreturn(&s.callee) => format!("noreturn call {}", s.callee),
                _ => "does not return".into(),
            }),
            _ => None,
        };
        blocks.push(Blk {
            addr: bl.addr,
            end: hi,
            insns: count(bl.addr, hi),
            calls: calls_in(img, &lifted, b),
            sites,
            ends,
        });
    }
    let edges = graph_of(&lifted, &synthetic);
    Ok(Model { name, start, end, blocks, edges, tables: lifted.tables, unresolved: lifted.unresolved_jumps })
}

/// The rows of a graph: its blocks, edges, loops and irreducible regions.
fn render(img: &Image, target: &str, m: Model) -> Result<Output> {
    let (start, end, name) = (m.start, m.end, m.name.clone());
    let n = m.blocks.len();
    let succs: Vec<Vec<usize>> = m.edges.iter().map(|e| e.iter().map(|x| x.0).collect()).collect();
    let in_fn = |a: u64| a >= start && a < end;
    let (idom, loops) = dominators_and_loops(&succs, 0);
    let reach = |b: usize| in_fn(m.blocks[b].addr) && idom[b].is_some();
    let mut order: Vec<usize> = (0..n).filter(|&b| reach(b)).collect();
    order.sort_by_key(|&b| m.blocks[b].addr);
    let addr = |b: usize| m.blocks[b].addr;
    // the innermost loop of each block: the smallest loop that holds it
    let innermost = |b: usize| -> Option<usize> {
        loops.iter().filter(|(_, set)| set.contains(&b)).min_by_key(|(h, set)| (set.len(), addr(**h))).map(|(h, _)| *h)
    };
    let mut irr: Vec<(Vec<u64>, Vec<u64>)> = irreducible_of(&succs, 0)
        .into_iter()
        .map(|(set, ent)| {
            let mut mem: Vec<u64> = set.iter().map(|&b| addr(b)).collect();
            let mut e: Vec<u64> = ent.iter().map(|&b| addr(b)).collect();
            mem.sort_unstable();
            e.sort_unstable();
            (mem, e)
        })
        .collect();
    irr.sort();
    let n_returns: usize =
        order.iter().map(|&b| m.blocks[b].sites.iter().filter(|s| s.indirect || !is_noreturn(&s.callee)).count()).sum();
    let n_edges: usize = order.iter().map(|&b| m.edges[b].len()).sum::<usize>() + n_returns;
    let n_insns: usize = order.iter().map(|&b| m.blocks[b].insns).sum();
    let mut rows: Vec<Row> = Vec::new();
    let mut push = |text: String, j: Option<Value>| {
        let key = rows.len() as u64;
        rows.push(Row { key, text, json: j });
    };
    let mut head = format!(
        "cfg {name} {start:#x}..{end:#x}  ; blocks {}, edges {n_edges}, loops {}, instructions {n_insns}",
        order.len(),
        loops.len()
    );
    if !irr.is_empty() {
        head.push_str(&format!(", irreducible loops {}", irr.len()));
    }
    if m.tables > 0 {
        head.push_str(&format!(", jump tables resolved: {}", m.tables));
    }
    if m.unresolved > 0 {
        head.push_str(&format!(", indirect jumps with no known target: {}", m.unresolved));
    }
    push(head, None);
    // blocks, by address
    for &b in &order {
        let bl = &m.blocks[b];
        let (lo, hi, ni) = (bl.addr, bl.end, bl.insns);
        let dom = if b == 0 { None } else { idom[b].map(addr) };
        let inner = innermost(b).map(addr);
        let calls = &bl.calls;
        let ends = &bl.ends;
        let mut text = format!(
            "block {lo:#x}..{hi:#x}  insns {ni}  idom {}",
            match dom {
                Some(d) => format!("{d:#x}"),
                None => "-".into(),
            }
        );
        if b == 0 {
            text.push_str("  entry");
        }
        if let Some(h) = inner {
            text.push_str(&format!("  in loop {h:#x}"));
        }
        if !calls.is_empty() {
            let more = calls.len().saturating_sub(CALLS_SHOWN);
            let shown = calls.iter().take(CALLS_SHOWN).cloned().collect::<Vec<_>>().join(", ");
            text.push_str(&format!("  calls {shown}{}", if more > 0 { format!(", +{more}") } else { String::new() }));
        }
        if let Some(e) = ends {
            text.push_str(&format!("  ; ends: {e}"));
        }
        push(
            text,
            Some(json!({"row": "block", "start": lo, "end": hi, "insns": ni, "idom": dom, "entry": b == 0,
                "loop": inner, "calls": calls, "ends": ends})),
        );
    }
    // edges, by source address, then in the order of the terminator
    for &b in &order {
        for Site { at: call, ret, callee, indirect } in &m.blocks[b].sites {
            let (call, ret, indirect) = (*call, *ret, *indirect);
            if !indirect && is_noreturn(callee) {
                push(
                    format!("call {call:#x} noreturn ({callee})  ; no return edge"),
                    Some(json!({"row": "call", "from": call, "callee": callee, "noreturn": true})),
                );
                continue;
            }
            let label = if indirect { "indirect".to_string() } else { callee.clone() };
            push(
                format!("edge {call:#x} -> {ret:#x}  call-return ({label})"),
                Some(json!({"row": "edge", "from": call, "to": ret, "kind": "call-return",
                    "callee": if indirect { Value::Null } else { json!(callee) }, "indirect": indirect,
                    "cases": Value::Null, "back": false, "outside": false, "to_name": Value::Null})),
            );
        }
        for (t, kind) in &m.edges[b] {
            let t_addr = addr(*t);
            let outside = !in_fn(t_addr);
            let back = !outside && dominates(&idom, *t, b);
            let tname = if outside { plain_name(img, t_addr) } else { None };
            let mut text = format!("edge {:#x} -> {t_addr:#x}  {}", addr(b), kind.word());
            if outside {
                text.push_str(&format!(
                    "  ; leaves the function{}",
                    tname.as_ref().map_or(String::new(), |n| format!(" to {n}"))
                ));
            } else if back {
                text.push_str("  ; back edge");
            }
            push(
                text,
                Some(json!({"row": "edge", "from": addr(b), "to": t_addr, "kind": kind.word(),
                    "cases": if let EdgeKind::Case(v) = kind { json!(v) } else { Value::Null },
                    "back": back, "outside": outside, "to_name": tname})),
            );
        }
    }
    // loops, by header address
    let by_header: BTreeMap<u64, usize> = loops.keys().map(|&h| (addr(h), h)).collect();
    for (&h_addr, &h) in &by_header {
        let set = &loops[&h];
        let mut members: Vec<u64> = set.iter().map(|&b| addr(b)).collect();
        members.sort_unstable();
        let back: Vec<u64> = {
            let mut v: Vec<u64> = set.iter().filter(|&&b| succs[b].contains(&h)).map(|&b| addr(b)).collect();
            v.sort_unstable();
            v
        };
        // the loop that holds this one: the smallest other loop with the header in it
        let parent = loops
            .iter()
            .filter(|(o, s)| **o != h && s.contains(&h))
            .min_by_key(|(o, s)| (s.len(), addr(**o)))
            .map(|(o, _)| addr(*o));
        let hex = |v: &[u64]| v.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(" ");
        let more = members.len().saturating_sub(LOOP_BLOCKS_SHOWN);
        let mut text = format!(
            "loop header {h_addr:#x}  {} blocks: {}{}  back edge from {}",
            members.len(),
            hex(&members[..members.len().min(LOOP_BLOCKS_SHOWN)]),
            if more > 0 { format!(" +{more}") } else { String::new() },
            hex(&back)
        );
        if let Some(p) = parent {
            text.push_str(&format!("  inside loop {p:#x}"));
        }
        push(
            text,
            Some(json!({"row": "loop", "header": h_addr, "blocks": members, "back_edges": back, "parent": parent})),
        );
    }
    // irreducible regions: cycles that can be entered at more than one block
    for (mem, e) in &irr {
        let hex = |v: &[u64]| v.iter().map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(", ");
        let more = mem.len().saturating_sub(LOOP_BLOCKS_SHOWN);
        push(
            format!(
                "irreducible loop: blocks {}{}; entries {}",
                hex(&mem[..mem.len().min(LOOP_BLOCKS_SHOWN)]),
                if more > 0 { format!(" +{more}") } else { String::new() },
                hex(e)
            ),
            Some(json!({"row": "irreducible", "blocks": mem, "entries": e})),
        );
    }
    let meta = json!({"function": name, "start": start, "end": end, "blocks": order.len(), "edges": n_edges,
        "loops": loops.len(), "irreducible": irr.len(), "instructions": n_insns});
    Ok(Output { op: "cfg", meta, rows, tail: 0, command: command(img, "cfg", &shell_word(target.trim())) })
}
