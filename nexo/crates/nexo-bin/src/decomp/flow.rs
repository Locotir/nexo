//! Data flow over the lifted blocks: reaching definitions, expression propagation, dead code,
//! call arguments, the return value, and the clean-up of the graph.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::ir::*;
use super::lift::{Abi, Lifted};

mod calls;
mod flag_and_div_passes;
mod propagate;
mod returns_and_tidy;
pub use self::calls::*;
pub use self::flag_and_div_passes::*;
pub use self::propagate::*;
pub use self::returns_and_tidy::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    V(Var),
    /// Memory that is not a promoted slot.
    Mem,
}

/// What the flow pass needs to know about the world around the function.
pub trait Sigs {
    fn sig_of(&mut self, t: &Target) -> Option<Sig>;
    /// The text of the C string at an address, if there is one.
    fn cstr(&self, addr: u64) -> Option<String>;
    /// Caller-saved registers the callee never writes: after the call they still hold what they held before.
    fn keeps(&mut self, _t: &Target) -> Vec<u8> {
        Vec::new()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Sig {
    pub args: usize,
    /// For a variadic function: the position of the format argument.
    pub fmt: Option<usize>,
    pub void: bool,
    /// The names of the arguments that are floating point (position).
    pub float: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct DefSite {
    pub blk: usize,
    pub idx: usize,
    pub key: Key,
}

pub const ENTRY: usize = usize::MAX;

#[derive(Debug)]
pub struct UseRec {
    pub blk: usize,
    /// Statement index; `stmts.len()` is the terminator.
    pub idx: usize,
    pub key: Key,
    pub reach: Vec<usize>,
}

pub struct Analysis {
    pub keys: Vec<Key>,
    pub key_ix: HashMap<Key, usize>,
    pub defs: Vec<DefSite>,
    pub nentry: usize,
    /// [block][stmt] -> def ids
    pub stmt_defs: Vec<Vec<Vec<usize>>>,
    pub uses: Vec<UseRec>,
    /// [block][stmt or terminator] -> use indices
    pub use_at: Vec<Vec<Vec<usize>>>,
    pub preds: Vec<Vec<usize>>,
}

pub fn stmt_use_keys(s: &Stmt) -> Vec<Key> {
    let mut out: Vec<Key> = Vec::new();
    let mut mem = false;
    for e in s.exprs() {
        e.vars(&mut |v| out.push(Key::V(v)));
        mem |= e.has_load() || e.has_call();
    }
    if let Stmt::Raw { uses, .. } = s {
        out.extend(uses.iter().map(|v| Key::V(*v)));
        mem = true;
    }
    if mem {
        out.push(Key::Mem);
    }
    out.sort_by_key(|k| format!("{k:?}"));
    out.dedup();
    out
}

pub fn stmt_def_keys(s: &Stmt) -> Vec<Key> {
    let mut out = Vec::new();
    match s {
        Stmt::Assign(v, e) => {
            out.push(Key::V(*v));
            if e.has_call() {
                out.push(Key::Mem);
            }
        }
        Stmt::Store(..) => out.push(Key::Mem),
        Stmt::Eval(_) => out.push(Key::Mem),
        Stmt::Clobber(vs) => out.extend(vs.iter().map(|v| Key::V(*v))),
        Stmt::Raw { defs, mem, .. } => {
            out.extend(defs.iter().map(|v| Key::V(*v)));
            if *mem {
                out.push(Key::Mem);
            }
        }
    }
    out
}

pub fn term_use_keys(t: &Term) -> Vec<Key> {
    let mut out: Vec<Key> = Vec::new();
    let mut mem = false;
    for e in t.exprs() {
        e.vars(&mut |v| out.push(Key::V(v)));
        mem |= e.has_load() || e.has_call();
    }
    if mem {
        out.push(Key::Mem);
    }
    out.sort_by_key(|k| format!("{k:?}"));
    out.dedup();
    out
}

pub fn preds_of(f: &Lifted) -> Vec<Vec<usize>> {
    let mut preds = vec![Vec::new(); f.blocks.len()];
    for (i, bl) in f.blocks.iter().enumerate() {
        for s in bl.term.succs() {
            if !preds[s].contains(&i) {
                preds[s].push(i);
            }
        }
    }
    preds
}

fn bit(v: &[u64], i: usize) -> bool {
    v[i / 64] >> (i % 64) & 1 == 1
}

fn set(v: &mut [u64], i: usize) {
    v[i / 64] |= 1 << (i % 64);
}

pub fn analyze(f: &Lifted) -> Analysis {
    let nb = f.blocks.len();
    let mut keys: Vec<Key> = Vec::new();
    let mut key_ix: HashMap<Key, usize> = HashMap::new();
    let intern = |k: Key, keys: &mut Vec<Key>, key_ix: &mut HashMap<Key, usize>| -> usize {
        *key_ix.entry(k).or_insert_with(|| {
            keys.push(k);
            keys.len() - 1
        })
    };
    // keys
    for bl in &f.blocks {
        for s in &bl.stmts {
            for k in stmt_use_keys(&s.st).into_iter().chain(stmt_def_keys(&s.st)) {
                intern(k, &mut keys, &mut key_ix);
            }
        }
        for k in term_use_keys(&bl.term) {
            intern(k, &mut keys, &mut key_ix);
        }
    }
    let nentry = keys.len();
    let mut defs: Vec<DefSite> = keys.iter().map(|&key| DefSite { blk: ENTRY, idx: 0, key }).collect();
    let mut stmt_defs: Vec<Vec<Vec<usize>>> = Vec::with_capacity(nb);
    for (bi, bl) in f.blocks.iter().enumerate() {
        let mut per = Vec::with_capacity(bl.stmts.len());
        for (si, s) in bl.stmts.iter().enumerate() {
            let mut ids = Vec::new();
            for k in stmt_def_keys(&s.st) {
                defs.push(DefSite { blk: bi, idx: si, key: k });
                ids.push(defs.len() - 1);
            }
            per.push(ids);
        }
        stmt_defs.push(per);
    }
    let nd = defs.len();
    let words = nd.div_ceil(64).max(1);
    let mut by_key: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    for (i, d) in defs.iter().enumerate() {
        by_key[key_ix[&d.key]].push(i);
    }
    // gen / kill per block
    let mut gen_: Vec<Vec<u64>> = vec![vec![0; words]; nb];
    let mut kill: Vec<Vec<u64>> = vec![vec![0; words]; nb];
    for bi in 0..nb {
        let mut last: HashMap<usize, usize> = HashMap::new();
        for ids in &stmt_defs[bi] {
            for &d in ids {
                last.insert(key_ix[&defs[d].key], d);
            }
        }
        for (&k, &d) in &last {
            set(&mut gen_[bi], d);
            for &o in &by_key[k] {
                if o != d {
                    set(&mut kill[bi], o);
                }
            }
        }
    }
    let preds = preds_of(f);
    let mut inn: Vec<Vec<u64>> = vec![vec![0; words]; nb];
    let mut out: Vec<Vec<u64>> = vec![vec![0; words]; nb];
    if nb > 0 {
        for d in 0..nentry {
            set(&mut inn[0], d);
        }
    }
    let mut changed = true;
    let mut rounds = 0;
    while changed && rounds < 200 {
        changed = false;
        rounds += 1;
        for bi in 0..nb {
            let mut new_in = if bi == 0 { inn[0].clone() } else { vec![0; words] };
            for &p in &preds[bi] {
                for w in 0..words {
                    new_in[w] |= out[p][w];
                }
            }
            let mut new_out = vec![0u64; words];
            for w in 0..words {
                new_out[w] = gen_[bi][w] | (new_in[w] & !kill[bi][w]);
            }
            if new_in != inn[bi] || new_out != out[bi] {
                inn[bi] = new_in;
                out[bi] = new_out;
                changed = true;
            }
        }
    }
    // uses with their reaching definitions
    let mut uses: Vec<UseRec> = Vec::new();
    let mut use_at: Vec<Vec<Vec<usize>>> = Vec::with_capacity(nb);
    for (bi, bl) in f.blocks.iter().enumerate() {
        let mut cur: HashMap<usize, usize> = HashMap::new();
        let mut at: Vec<Vec<usize>> = Vec::with_capacity(bl.stmts.len() + 1);
        for si in 0..=bl.stmts.len() {
            let ks = if si < bl.stmts.len() { stmt_use_keys(&bl.stmts[si].st) } else { term_use_keys(&bl.term) };
            let mut here = Vec::new();
            for k in ks {
                let ki = key_ix[&k];
                let reach: Vec<usize> = if let Some(&d) = cur.get(&ki) {
                    vec![d]
                } else {
                    by_key[ki].iter().copied().filter(|&d| bit(&inn[bi], d)).collect()
                };
                uses.push(UseRec { blk: bi, idx: si, key: k, reach });
                here.push(uses.len() - 1);
            }
            at.push(here);
            if si < bl.stmts.len() {
                for &d in &stmt_defs[bi][si] {
                    cur.insert(key_ix[&defs[d].key], d);
                }
            }
        }
        use_at.push(at);
    }
    Analysis { keys, key_ix, defs, nentry, stmt_defs, uses, use_at, preds }
}

fn is_simple(e: &Expr) -> bool {
    match e {
        Expr::Const(..) | Expr::Var(..) | Expr::StackAddr(_) | Expr::SegBase(_) | Expr::Unknown(_) => true,
        Expr::Zext(x, _) | Expr::Trunc(x, _) | Expr::Sext(x, _) => is_simple(x),
        _ => false,
    }
}

/// An expression cheap enough to copy to every place that reads it.
fn is_trivial(e: &Expr) -> bool {
    if e.has_call() || e.has_load() {
        return false;
    }
    match e {
        Expr::Flags(_, a, c) => is_simple(a) && is_simple(c),
        Expr::Bin(Bin::Add | Bin::Sub, a, c) => is_simple(a) && is_simple(c) && e.size() <= 5,
        Expr::Insert(..) => false,
        Expr::Bin(Bin::UDiv | Bin::SDiv | Bin::UMod | Bin::SMod, a, c) => is_simple(a) && c.as_const().is_some(),
        Expr::Zext(x, _) | Expr::Sext(x, _)
            if matches!(**x, Expr::Bin(Bin::UDiv | Bin::SDiv | Bin::UMod | Bin::SMod, ..)) =>
        {
            is_trivial(x)
        }
        _ => is_simple(e) && e.size() <= 4,
    }
}

fn expr_keys(e: &Expr) -> Vec<Key> {
    let mut out: Vec<Key> = Vec::new();
    e.vars(&mut |v| out.push(Key::V(v)));
    if e.has_load() || e.has_call() {
        out.push(Key::Mem);
    }
    out
}

// ---------------------------------------------------------------------------------------------

/// Everything the flow pass learns about the function.
#[derive(Debug, Default, Clone)]
pub struct Facts {
    pub ret: Option<Var>,
    /// Number of register arguments the function reads.
    pub argc: usize,
    /// Stack argument slots read (offsets from the entry stack pointer).
    pub stack_args: BTreeSet<i32>,
    pub float_args: BTreeSet<u8>,
}

pub fn run(f: &mut Lifted, sigs: &mut dyn Sigs) -> Facts {
    let dbg = std::env::var("NEXO_DECOMP_IR").is_ok_and(|v| v == "2");
    let show = |f: &Lifted, s: &str| {
        if dbg {
            eprintln!("STAGE {s}\n{}", super::ir_text(f));
        }
    };
    prune(f);
    drop_canary(f);
    tidy(f);
    snapshot_flags(f);
    drop_dead_inserts(f);
    show(f, "tidy");
    keep_across_calls(f, sigs);
    resolve_calls(f, sigs, false);
    show(f, "resolve1");
    let ret = fix_returns(f);
    set_return_var(f, ret);
    propagate(f);
    show(f, "propagate1");
    resolve_calls(f, sigs, true);
    show(f, "resolve2");
    propagate(f);
    // a canary compare is only visible after propagation
    drop_canary(f);
    propagate(f);
    merge_switches(f);
    tidy(f);
    propagate(f);
    tidy(f);
    narrow_returns(f);
    let mut facts = Facts { ret, ..Facts::default() };
    entry_facts(f, &mut facts);
    facts
}

pub fn entry_facts(f: &Lifted, facts: &mut Facts) {
    let an = analyze(f);
    let abi = f.abi;
    for u in &an.uses {
        let entry = u.reach.iter().any(|&d| an.defs[d].blk == ENTRY);
        if !entry {
            continue;
        }
        match u.key {
            Key::V(Var::Reg(r)) => {
                if let Some(pos) = abi.arg_regs().iter().position(|&x| x == r) {
                    facts.argc = facts.argc.max(pos + 1);
                }
            }
            Key::V(Var::Stack(k)) if i64::from(k) >= abi.stack_arg_base() => {
                facts.stack_args.insert(k);
            }
            Key::V(Var::Xmm(x)) if x < 8 => {
                facts.float_args.insert(x);
            }
            _ => {}
        }
    }
}
