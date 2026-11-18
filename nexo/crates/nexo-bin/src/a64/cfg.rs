//! The control-flow graph of an AArch64 function, from the decoder's own [`Flow`]: no lifting, no second pass over
//! the bytes. Blocks start at the entry, at every branch target inside the function and after every instruction
//! that ends a path (`b`, `b.cond`, `cbz`, `tbz`, `br`, `ret`, a trap, a call that never returns). A `bl` is a call
//! that falls through; `blr` has no known target, and nor has a `br` unless it reads like a switch (see
//! [`super::switch`]), which gets one `case` edge per target. The rows are printed by `crate::cfg`, as for x86.

use std::collections::{BTreeSet, HashMap};

use super::switch::{self, Table};
use super::{Flow, Ins};
use crate::cfg::{Blk, EdgeKind, Model, Site};
use crate::decomp::api::plain_name;
use crate::decomp::lift::is_noreturn;
use crate::funcs::Func;
use crate::{Error, Image, Result};

/// The block number of an address: a block of the function, or a stand-in for a place it leaves to (numbered after
/// the real blocks, in the order met).
fn node(a: u64, pos: &HashMap<u64, usize>, stubs: &mut Vec<u64>, real: usize) -> usize {
    if let Some(&b) = pos.get(&a) {
        return b;
    }
    match stubs.iter().position(|&s| s == a) {
        Some(p) => real + p,
        None => {
            stubs.push(a);
            real + stubs.len() - 1
        }
    }
}

pub(crate) fn model(img: &Image, func: &Func) -> Result<Model> {
    let (start, end) = (func.start, func.end);
    let name = img.func_name(func).to_string();
    let bytes = img.read_at(start, (end - start) as usize).filter(|b| b.len() >= 4);
    let mut ins: Vec<Ins> = Vec::new();
    if let Some(bytes) = bytes {
        super::view::walk(bytes, start, |i, _, _| ins.push(*i));
    }
    let n = ins.len();
    if n == 0 {
        return Err(Error::Bad(format!("{name} {start:#x}: no code to read there")));
    }
    let stop = start + 4 * n as u64;
    let in_code = |a: u64| (start..stop).contains(&a) && (a - start).is_multiple_of(4);
    let at = |a: u64| ((a - start) / 4) as usize;
    let named = |t: u64| plain_name(img, t).unwrap_or_else(|| format!("{t:#x}"));
    let callee = |i: &Ins| i.target.map_or_else(|| "indirect".to_string(), named);
    let noreturn = |i: &Ins| i.flow == Flow::Call && is_noreturn(&callee(i));
    // where blocks start, as instruction numbers
    let mut lead: BTreeSet<usize> = BTreeSet::from([0]);
    let read = |a: u64, len: usize| img.read_at(a, len).map(<[u8]>::to_vec);
    let mut tabs: HashMap<usize, Table> = HashMap::new();
    for (k, i) in ins.iter().enumerate() {
        if i.flow == Flow::JumpReg {
            if let Some(t) = switch::find(&ins, k, start, stop, &read) {
                lead.extend(t.targets.iter().map(|&a| at(a)));
                tabs.insert(k, t);
            }
        }
        let ends = matches!(i.flow, Flow::Jump | Flow::Cond | Flow::JumpReg | Flow::Ret | Flow::Trap) || noreturn(i);
        if ends && k + 1 < n {
            lead.insert(k + 1);
        }
        if matches!(i.flow, Flow::Jump | Flow::Cond) {
            if let Some(t) = i.target.filter(|&t| in_code(t)) {
                lead.insert(at(t));
            }
        }
    }
    let starts: Vec<usize> = lead.into_iter().collect();
    let real = starts.len();
    let pos: HashMap<u64, usize> = starts.iter().enumerate().map(|(b, &k)| (ins[k].pc, b)).collect();
    let mut stubs: Vec<u64> = Vec::new();
    let mut blocks: Vec<Blk> = Vec::new();
    let mut edges: Vec<Vec<(usize, EdgeKind)>> = Vec::new();
    let mut unresolved = 0;
    let mut tables = 0;
    for (b, &k) in starts.iter().enumerate() {
        let z = starts.get(b + 1).copied().unwrap_or(n);
        let last = &ins[z - 1];
        let next = start + 4 * z as u64;
        let mut sites: Vec<Site> = Vec::new();
        let mut calls: Vec<String> = Vec::new();
        for i in &ins[k..z] {
            let (indirect, c) = match i.flow {
                Flow::Call => (false, callee(i)),
                Flow::CallReg => (true, "indirect".to_string()),
                _ => continue,
            };
            if !calls.contains(&c) {
                calls.push(c.clone());
            }
            sites.push(Site { at: i.pc, ret: i.pc + 4, callee: c, indirect });
        }
        let mut out: Vec<(usize, EdgeKind)> = Vec::new();
        let mut ends: Option<String> = None;
        match last.flow {
            Flow::Cond => {
                if let Some(t) = last.target {
                    out.push((node(t, &pos, &mut stubs, real), EdgeKind::CondTrue));
                    out.push((node(next, &pos, &mut stubs, real), EdgeKind::CondFalse));
                }
            }
            Flow::Jump => match last.target {
                Some(t) if in_code(t) => out.push((node(t, &pos, &mut stubs, real), EdgeKind::Jump)),
                Some(t) => ends = Some(format!("tail call {}", named(t))),
                None => ends = Some("does not return".into()),
            },
            Flow::JumpReg => match tabs.get(&(z - 1)) {
                Some(t) => {
                    tables += 1;
                    // one edge per target, with every case value that lands there
                    let mut by_target: Vec<(u64, Vec<u64>)> = Vec::new();
                    for (v, &a) in t.targets.iter().enumerate() {
                        match by_target.iter_mut().find(|x| x.0 == a) {
                            Some(x) => x.1.push(v as u64),
                            None => by_target.push((a, vec![v as u64])),
                        }
                    }
                    for (a, v) in by_target {
                        out.push((node(a, &pos, &mut stubs, real), EdgeKind::Case(v)));
                    }
                }
                None => {
                    unresolved += 1;
                    ends = Some("indirect jump, target not resolved".into());
                }
            },
            Flow::Ret => ends = Some("ret".into()),
            Flow::Trap => ends = Some("does not return".into()),
            _ if noreturn(last) => ends = Some(format!("noreturn call {}", callee(last))),
            _ if z < n => out.push((node(next, &pos, &mut stubs, real), EdgeKind::Fallthrough)),
            _ => ends = Some("does not return".into()),
        }
        blocks.push(Blk { addr: ins[k].pc, end: next, insns: z - k, calls, sites, ends });
        edges.push(out);
    }
    for s in stubs {
        blocks.push(Blk { addr: s, end: s, insns: 0, calls: Vec::new(), sites: Vec::new(), ends: None });
        edges.push(Vec::new());
    }
    Ok(Model { name, start, end, blocks, edges, tables, unresolved })
}
