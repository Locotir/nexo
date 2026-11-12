//! Data flow over the lifted blocks: reaching definitions, expression propagation, dead code,
//! call arguments, the return value, and the clean-up of the graph.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::ir::*;
use super::lift::{Abi, Lifted};

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

struct Plan {
    src: (usize, usize),
    blk: usize,
    idx: usize,
    var: Var,
    with: Expr,
}

fn count_occ(s: &Stmt, term: Option<&Term>, v: Var) -> usize {
    let mut n = 0;
    match term {
        Some(t) => t.exprs().iter().for_each(|e| n += e.uses(v)),
        None => s.exprs().iter().for_each(|e| n += e.uses(v)),
    }
    n
}

fn round(f: &mut Lifted) -> bool {
    let an = analyze(f);
    let nd = an.defs.len();
    let mut single: Vec<Vec<usize>> = vec![Vec::new(); nd];
    let mut blocked = vec![false; nd];
    for (ui, u) in an.uses.iter().enumerate() {
        if u.reach.len() == 1 {
            single[u.reach[0]].push(ui);
        } else {
            for &d in &u.reach {
                blocked[d] = true;
            }
        }
    }
    let mut plans: Vec<Plan> = Vec::new();
    // calls that moved into their only reader: the old statement must not run a second time
    let mut moved: Vec<(usize, usize)> = Vec::new();
    for d in an.nentry..nd {
        let site = an.defs[d];
        let Stmt::Assign(v, e) = &f.blocks[site.blk].stmts[site.idx].st else { continue };
        // the copy made for the flags stays a copy
        if matches!(v, Var::Tmp(n) if *n >= 16) && matches!(e, Expr::Var(..)) {
            continue;
        }
        if single[d].is_empty() {
            continue;
        }
        // the uses of this definition in the variable it defines
        let own: Vec<usize> = single[d].iter().copied().filter(|&u| an.uses[u].key == Key::V(*v)).collect();
        if own.is_empty() {
            continue;
        }
        let total = own.len() + 2 * usize::from(blocked[d]);
        let trivial = is_trivial(e);
        if !trivial && total != 1 {
            continue;
        }
        if matches!(e, Expr::Unknown(_)) {
            continue;
        }
        let is_call = e.has_call();
        let ops: Vec<Key> = expr_keys(e);
        let taint0 = ops.contains(&Key::V(*v));
        // walk forward from the definition
        let mut clean: HashSet<(usize, usize)> = HashSet::new();
        // reached by a path where an operand changed: the copy would read a different value
        let mut stale: HashSet<(usize, usize)> = HashSet::new();
        let mut seen: HashSet<(usize, bool)> = HashSet::new();
        let mut stack: Vec<(usize, usize, bool)> = vec![(site.blk, site.idx + 1, taint0)];
        let mut first = true;
        while let Some((b, start, taint0)) = stack.pop() {
            if !first && !seen.insert((b, taint0)) {
                continue;
            }
            first = false;
            let bl = &f.blocks[b];
            let mut taint = taint0;
            let mut killed = false;
            for idx in start..=bl.stmts.len() {
                for &u in &an.use_at[b][idx] {
                    let ur = &an.uses[u];
                    if ur.key == Key::V(*v) && ur.reach.len() == 1 && ur.reach[0] == d {
                        if taint {
                            stale.insert((b, idx));
                        } else {
                            clean.insert((b, idx));
                        }
                    }
                    if is_call && ur.key == Key::Mem {
                        taint = true;
                    }
                }
                if idx < bl.stmts.len() {
                    let defs: Vec<Key> = stmt_def_keys(&bl.stmts[idx].st);
                    if defs.contains(&Key::V(*v)) {
                        killed = true;
                        break;
                    }
                    if defs.iter().any(|k| ops.contains(k)) {
                        taint = true;
                    }
                }
            }
            if !killed {
                for s in bl.term.succs() {
                    stack.push((s, 0, taint));
                }
            }
        }
        clean.retain(|k| !stale.contains(k));
        let before = plans.len();
        for &u in &own {
            let ur = &an.uses[u];
            if !clean.contains(&(ur.blk, ur.idx)) {
                continue;
            }
            let bl = &f.blocks[ur.blk];
            let occ = if ur.idx < bl.stmts.len() {
                count_occ(&bl.stmts[ur.idx].st, None, *v)
            } else {
                count_occ(&Stmt::Clobber(vec![]), Some(&bl.term), *v)
            };
            if !trivial && occ != 1 {
                continue;
            }
            // a statement that changes the variable and reads it elsewhere is left alone
            plans.push(Plan { src: (site.blk, site.idx), blk: ur.blk, idx: ur.idx, var: *v, with: e.clone() });
        }
        if is_call && !trivial && plans.len() == before + 1 {
            moved.push((site.blk, site.idx));
        }
    }
    // a definition that is itself rewritten this round moves next round, with its new text
    let rewritten: HashSet<(usize, usize)> = plans.iter().map(|p| (p.blk, p.idx)).collect();
    plans.retain(|p| !rewritten.contains(&p.src));
    moved.retain(|m| plans.iter().any(|p| p.src == *m));
    let n = plans.len();
    let targets: HashSet<(usize, usize)> = plans.iter().map(|p| (p.blk, p.idx)).collect();
    for p in plans {
        let bl = &mut f.blocks[p.blk];
        if p.idx < bl.stmts.len() {
            for e in bl.stmts[p.idx].st.exprs_mut() {
                e.subst(p.var, &p.with);
            }
        } else {
            for e in bl.term.exprs_mut() {
                e.subst(p.var, &p.with);
            }
        }
    }
    for (blk, idx) in moved {
        if !targets.contains(&(blk, idx)) {
            f.blocks[blk].stmts[idx].st = Stmt::Clobber(Vec::new());
        }
    }
    n > 0
}

/// Remove assignments nobody reads. A call whose value nobody reads stays as a statement.
pub fn dce(f: &mut Lifted) -> bool {
    let nb = f.blocks.len();
    let mut any = false;
    for _ in 0..30 {
        let mut ix: HashMap<Var, usize> = HashMap::new();
        let collect = |v: Var, ix: &mut HashMap<Var, usize>| {
            let n = ix.len();
            ix.entry(v).or_insert(n);
        };
        for bl in &f.blocks {
            for s in &bl.stmts {
                s.st.uses(&mut |v| collect(v, &mut ix));
                s.st.defs(&mut |v| collect(v, &mut ix));
            }
            for e in bl.term.exprs() {
                e.vars(&mut |v| collect(v, &mut ix));
            }
        }
        let nv = ix.len();
        let words = nv.div_ceil(64).max(1);
        let mut live_in: Vec<Vec<u64>> = vec![vec![0; words]; nb];
        let mut live_out: Vec<Vec<u64>> = vec![vec![0; words]; nb];
        let step = |bl: &Block, live: &mut Vec<u64>, ix: &HashMap<Var, usize>, remove: Option<&mut Vec<bool>>| {
            for e in bl.term.exprs() {
                e.vars(&mut |v| set(live, ix[&v]));
            }
            let mut rem = remove;
            for (si, s) in bl.stmts.iter().enumerate().rev() {
                let dead = match &s.st {
                    Stmt::Assign(v, _) => !bit(live, ix[v]),
                    _ => false,
                };
                if dead {
                    if let Some(r) = rem.as_deref_mut() {
                        r[si] = true;
                    }
                    // a call keeps its arguments alive
                    if let Stmt::Assign(_, e) = &s.st {
                        if e.has_call() {
                            e.vars(&mut |v| set(live, ix[&v]));
                        }
                    }
                    continue;
                }
                match &s.st {
                    Stmt::Assign(v, _) => live[ix[v] / 64] &= !(1 << (ix[v] % 64)),
                    Stmt::Clobber(vs) => vs.iter().for_each(|v| live[ix[v] / 64] &= !(1 << (ix[v] % 64))),
                    Stmt::Raw { defs, .. } => defs.iter().for_each(|v| live[ix[v] / 64] &= !(1 << (ix[v] % 64))),
                    _ => {}
                }
                s.st.uses(&mut |v| set(live, ix[&v]));
            }
        };
        let mut changed = true;
        let mut guard = 0;
        while changed && guard < 500 {
            changed = false;
            guard += 1;
            for bi in (0..nb).rev() {
                let mut lo = vec![0u64; words];
                for s in f.blocks[bi].term.succs() {
                    for w in 0..words {
                        lo[w] |= live_in[s][w];
                    }
                }
                let mut li = lo.clone();
                step(&f.blocks[bi], &mut li, &ix, None);
                if lo != live_out[bi] || li != live_in[bi] {
                    live_out[bi] = lo;
                    live_in[bi] = li;
                    changed = true;
                }
            }
        }
        let mut removed = false;
        for bi in 0..nb {
            let mut live = live_out[bi].clone();
            let mut rem = vec![false; f.blocks[bi].stmts.len()];
            step(&f.blocks[bi], &mut live, &ix, Some(&mut rem));
            if rem.iter().any(|&r| r) {
                removed = true;
                let old = std::mem::take(&mut f.blocks[bi].stmts);
                for (s, r) in old.into_iter().zip(rem) {
                    if !r {
                        f.blocks[bi].stmts.push(s);
                    } else if let Stmt::Assign(_, e) = &s.st {
                        if e.has_call() {
                            f.blocks[bi].stmts.push(S { addr: s.addr, st: Stmt::Eval(e.clone()) });
                        }
                    }
                }
            }
        }
        any |= removed;
        if !removed {
            break;
        }
    }
    any
}

/// Run propagation and dead code removal until nothing changes.
pub fn propagate(f: &mut Lifted) {
    for _ in 0..60 {
        let a = round(f);
        recover_divs(f);
        let b = dce(f);
        if !a && !b {
            break;
        }
    }
}

/// Where a call reads its arguments from.
fn arg_vars(abi: Abi, n: usize) -> Vec<Expr> {
    abi.arg_regs().iter().take(n).map(|&r| Expr::Var(Var::Reg(r), 64)).collect()
}

fn each_call(f: &mut Lifted, mut g: impl FnMut(usize, usize, &mut Call)) {
    for bi in 0..f.blocks.len() {
        for si in 0..f.blocks[bi].stmts.len() {
            if let Stmt::Assign(_, Expr::Call(c)) | Stmt::Eval(Expr::Call(c)) = &mut f.blocks[bi].stmts[si].st {
                g(bi, si, c);
            }
        }
        let n = f.blocks[bi].stmts.len();
        if let Term::TailCall(Expr::Call(c)) = &mut f.blocks[bi].term {
            g(bi, n, c);
        }
    }
}

/// Fix the arguments of every call whose signature is known or guessed. `variadic` also adds the
/// arguments a constant format string asks for.
pub fn resolve_calls(f: &mut Lifted, sigs: &mut dyn Sigs, variadic: bool) {
    let an = analyze(f);
    let abi = f.abi;
    // for each call site: the number of leading argument registers that hold a value set in the function
    let mut prefix: HashMap<(usize, usize), usize> = HashMap::new();
    for u in &an.uses {
        if let Key::V(Var::Reg(r)) = u.key {
            if let Some(pos) = abi.arg_regs().iter().position(|&x| x == r) {
                let fresh = u.reach.iter().any(|&d| {
                    let s = an.defs[d];
                    if s.blk == ENTRY {
                        return false;
                    }
                    !matches!(f.blocks[s.blk].stmts[s.idx].st, Stmt::Clobber(_))
                });
                let entry = u.reach.iter().any(|&d| an.defs[d].blk == ENTRY);
                if fresh {
                    let e = prefix.entry((u.blk, u.idx)).or_insert(0);
                    *e = (*e).max(pos + 1);
                } else if entry {
                    prefix.entry((u.blk, u.idx)).or_insert(0);
                }
            }
        }
    }
    // the call's own uses are not recorded for the argument registers (it has no arguments yet), so look
    // at the definitions that reach the call instead
    let mut at_call: HashMap<(usize, usize), usize> = HashMap::new();
    {
        // reaching defs of each argument register right before the call
        for bi in 0..f.blocks.len() {
            let n = f.blocks[bi].stmts.len();
            for si in 0..=n {
                let is_call = if si < n {
                    matches!(&f.blocks[bi].stmts[si].st, Stmt::Assign(_, Expr::Call(_)) | Stmt::Eval(Expr::Call(_)))
                } else {
                    matches!(f.blocks[bi].term, Term::TailCall(_))
                };
                if !is_call {
                    continue;
                }
                // the register that holds the address called is not also an argument
                let target = if si < n {
                    match &f.blocks[bi].stmts[si].st {
                        Stmt::Assign(_, Expr::Call(c)) | Stmt::Eval(Expr::Call(c)) => Some(&c.target),
                        _ => None,
                    }
                } else {
                    match &f.blocks[bi].term {
                        Term::TailCall(Expr::Call(c)) => Some(&c.target),
                        _ => None,
                    }
                };
                let held = match target {
                    Some(Target::Indirect(Expr::Var(Var::Reg(r), _))) => Some(*r),
                    _ => None,
                };
                let mut best = 0;
                for (pos, &r) in abi.arg_regs().iter().enumerate() {
                    if Some(r) == held {
                        continue;
                    }
                    if fresh_reach(f, &an, bi, si, Var::Reg(r)) {
                        best = pos + 1;
                    }
                }
                at_call.insert((bi, si), best);
            }
        }
    }
    each_call(f, |bi, si, c| {
        if c.known && !variadic {
            return;
        }
        let sig = sigs.sig_of(&c.target);
        if c.known && sig.as_ref().is_none_or(|s| s.fmt.is_none()) {
            return;
        }
        let fixed = at_call.get(&(bi, si)).copied().unwrap_or(0);
        match sig {
            Some(s) => {
                let mut n = s.args;
                if let (Some(fi), true) = (s.fmt, variadic) {
                    // the format argument is a constant string: count its conversions
                    let fmt = c.args.get(fi).and_then(|a| match a {
                        Expr::Const(v, _) => sigs.cstr(*v),
                        _ => None,
                    });
                    match fmt {
                        Some(text) => n = fi + 1 + count_conversions(&text),
                        None => n = n.max(c.args.len()).max(fixed.min(abi.arg_count_max())),
                    }
                    let old = std::mem::take(&mut c.args);
                    let fresh = arg_vars(abi, n.min(abi.arg_count_max()));
                    c.args = fresh.into_iter().enumerate().map(|(i, v)| old.get(i).cloned().unwrap_or(v)).collect();
                    c.known = true;
                    return;
                }
                if s.fmt.is_some() {
                    // wait for the format string; keep every register set before the call alive until then
                    c.args = arg_vars(abi, n.max(fixed).min(abi.arg_count_max()));
                    c.known = false;
                    return;
                }
                c.args = arg_vars(abi, n.min(abi.arg_count_max()));
                c.known = true;
            }
            None => {
                c.args = arg_vars(abi, fixed);
                c.known = true;
            }
        }
    });
}

pub fn count_conversions(fmt: &str) -> usize {
    let b = fmt.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            i += 1;
            if i < b.len() && b[i] == b'%' {
                i += 1;
                continue;
            }
            while i < b.len() && !b[i].is_ascii_alphabetic() {
                if b[i] == b'*' {
                    n += 1;
                }
                i += 1;
            }
            // length modifiers
            while i < b.len() && matches!(b[i], b'h' | b'l' | b'L' | b'j' | b'z' | b't' | b'q') {
                i += 1;
            }
            if i < b.len() {
                n += 1;
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    n
}

/// True when a value written in the function (not at entry, not by a call's clobber) reaches the
/// point just before statement `si` of block `bi`.
fn fresh_reach(f: &Lifted, an: &Analysis, bi: usize, si: usize, v: Var) -> bool {
    let Some(&ki) = an.key_ix.get(&Key::V(v)) else { return false };
    // last local definition before si
    for j in (0..si.min(f.blocks[bi].stmts.len())).rev() {
        for &d in &an.stmt_defs[bi][j] {
            if an.defs[d].key == Key::V(v) {
                return !matches!(f.blocks[bi].stmts[j].st, Stmt::Clobber(_));
            }
        }
    }
    // walk back through predecessors: any path that brings a real definition counts
    let mut seen = HashSet::new();
    let mut stack: Vec<usize> = an.preds[bi].clone();
    let _ = ki;
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        let mut found_local = false;
        for j in (0..f.blocks[p].stmts.len()).rev() {
            for &d in &an.stmt_defs[p][j] {
                if an.defs[d].key == Key::V(v) {
                    if !matches!(f.blocks[p].stmts[j].st, Stmt::Clobber(_)) {
                        return true;
                    }
                    found_local = true;
                }
            }
            if found_local {
                break;
            }
        }
        if !found_local {
            stack.extend(an.preds[p].iter().copied());
        }
    }
    false
}

/// Decide whether the function returns a value, and which.
pub fn fix_returns(f: &mut Lifted) -> Option<Var> {
    let an = analyze(f);
    let mut rets: Vec<(usize, Vec<usize>)> = Vec::new();
    for (bi, bl) in f.blocks.iter().enumerate() {
        if matches!(bl.term, Term::Return(_)) {
            let reach: Vec<usize> = an.use_at[bi][bl.stmts.len()]
                .iter()
                .filter(|&&u| an.uses[u].key == Key::V(RAX))
                .flat_map(|&u| an.uses[u].reach.clone())
                .collect();
            rets.push((bi, reach));
        }
    }
    let real = |d: &usize| {
        let s = an.defs[*d];
        s.blk != ENTRY && !matches!(f.blocks[s.blk].stmts[s.idx].st, Stmt::Clobber(_))
    };
    let all_real = !rets.is_empty() && rets.iter().all(|(_, r)| !r.is_empty() && r.iter().all(real));
    if all_real {
        return Some(RAX);
    }
    // a float result
    let mut xmm_ok = !rets.is_empty();
    for (bi, _) in &rets {
        let bl = &f.blocks[*bi];
        let x0 = Var::Xmm(0);
        // the nearest definition of xmm0 must be a real one
        let mut ok = false;
        'o: for j in (0..bl.stmts.len()).rev() {
            for &d in &an.stmt_defs[*bi][j] {
                if an.defs[d].key == Key::V(x0) {
                    ok = !matches!(bl.stmts[j].st, Stmt::Clobber(_));
                    break 'o;
                }
            }
        }
        xmm_ok &= ok;
    }
    let rax_defined_somewhere = rets.iter().any(|(_, r)| r.iter().any(real));
    if xmm_ok && !rax_defined_somewhere {
        return Some(Var::Xmm(0));
    }
    for bl in &mut f.blocks {
        if let Term::Return(e) = &mut bl.term {
            *e = None;
        }
    }
    None
}

/// For the return statements of a function that returns a value, make the expression the variable.
pub fn set_return_var(f: &mut Lifted, v: Option<Var>) {
    for bl in &mut f.blocks {
        if let Term::Return(e) = &mut bl.term {
            *e = v.map(|x| Expr::Var(x, 64));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Graph clean-up

/// Drop blocks nothing reaches and renumber the rest.
pub fn prune(f: &mut Lifted) {
    let n = f.blocks.len();
    let mut seen = vec![false; n];
    let mut stack = vec![0usize];
    while let Some(b) = stack.pop() {
        if b >= n || seen[b] {
            continue;
        }
        seen[b] = true;
        stack.extend(f.blocks[b].term.succs());
    }
    if seen.iter().all(|&s| s) {
        return;
    }
    let mut map = vec![usize::MAX; n];
    let mut next = 0;
    for i in 0..n {
        if seen[i] {
            map[i] = next;
            next += 1;
        }
    }
    let old = std::mem::take(&mut f.blocks);
    for (i, mut bl) in old.into_iter().enumerate() {
        if seen[i] {
            bl.term.retarget(|t| map[t]);
            f.blocks.push(bl);
        }
    }
}

/// Thread jumps through empty blocks, fold branches with equal arms, merge straight lines.
pub fn tidy(f: &mut Lifted) {
    loop {
        let mut changed = false;
        // empty jump-only blocks
        let n = f.blocks.len();
        let mut fwd: Vec<usize> = (0..n).collect();
        for i in 1..n {
            if f.blocks[i].stmts.is_empty() {
                if let Term::Jump(t) = f.blocks[i].term {
                    if t != i {
                        fwd[i] = t;
                    }
                }
            }
        }
        let resolve = |mut x: usize| {
            let mut guard = 0;
            while fwd[x] != x && guard < 50 {
                x = fwd[x];
                guard += 1;
            }
            x
        };
        for i in 0..n {
            let mut t = std::mem::replace(&mut f.blocks[i].term, Term::Stop);
            let before = t.clone();
            t.retarget(resolve);
            if let Term::Branch(_, a, c) = &t {
                if a == c {
                    t = Term::Jump(*a);
                }
            }
            if t != before {
                changed = true;
            }
            f.blocks[i].term = t;
        }
        // merge A -> B when B has no other predecessor
        let preds = preds_of(f);
        for a in 0..f.blocks.len() {
            if let Term::Jump(b) = f.blocks[a].term {
                if b != a && b != 0 && preds[b].len() == 1 && preds[b][0] == a {
                    let bb = std::mem::replace(
                        &mut f.blocks[b],
                        Block { addr: 0, stmts: Vec::new(), term: Term::Stop, term_addr: 0 },
                    );
                    f.blocks[a].stmts.extend(bb.stmts);
                    f.blocks[a].term = bb.term;
                    f.blocks[a].term_addr = bb.term_addr;
                    changed = true;
                    break;
                }
            }
        }
        prune(f);
        if !changed {
            break;
        }
    }
}

/// `if (x > N) goto default; switch table` becomes one switch.
pub fn merge_switches(f: &mut Lifted) {
    let preds = preds_of(f);
    for p in 0..f.blocks.len() {
        let Term::Branch(c, t, e) = f.blocks[p].term.clone() else { continue };
        let Expr::Cmp(op, x, n) = &c else { continue };
        let Some(bound) = n.as_const() else { continue };
        for (sw, other, taken_default) in [(e, t, true), (t, e, false)] {
            if sw == p || preds[sw].len() != 1 || !f.blocks[sw].stmts.is_empty() {
                continue;
            }
            let Term::Switch(_, cases, None) = f.blocks[sw].term.clone() else { continue };
            // the compare must send out-of-range values to `other`
            let outside = if taken_default { *op } else { op.negate() };
            let max_ok = match outside {
                Cmp::Ugt => Some(bound),
                Cmp::Uge => bound.checked_sub(1),
                _ => None,
            };
            let Some(max) = max_ok else { continue };
            let cases: Vec<(u64, BlockId)> = cases.into_iter().filter(|(v, _)| *v <= max).collect();
            // `x - c` as the index shifts the case values
            let (scr, off) = match &**x {
                Expr::Bin(Bin::Add, y, k) if k.as_const().is_some() => ((**y).clone(), k.as_const().unwrap_or(0)),
                other => (other.clone(), 0),
            };
            let w = scr.bits();
            let cases: Vec<(u64, BlockId)> =
                cases.into_iter().map(|(v, b)| (v.wrapping_sub(off) & mask(w), b)).collect();
            f.blocks[p].term = Term::Switch(scr, cases, Some(other));
            f.blocks[sw].term = Term::Stop;
            break;
        }
    }
    prune(f);
}

/// Hide the stack protector: the canary check can never fail in the code we show.
pub fn drop_canary(f: &mut Lifted) {
    let has = |e: &Expr| {
        e.has(&|x| {
            matches!(x, Expr::Intr("canary", ..))
                || matches!(x, Expr::Load(a, _) if matches!(**a, Expr::SegBase(_)))
                || matches!(x, Expr::Bin(_, a, _) if matches!(**a, Expr::SegBase(_)))
        })
    };
    for i in 0..f.blocks.len() {
        if let Term::Branch(c, t, e) = f.blocks[i].term.clone() {
            if has(&c) {
                // the side that ends in a call that does not return is the failure
                let fail = |b: usize| matches!(f.blocks[b].term, Term::Stop) && f.blocks[b].stmts.len() <= 3;
                let keep = if fail(t) {
                    e
                } else if fail(e) {
                    t
                } else {
                    continue;
                };
                f.blocks[i].term = Term::Jump(keep);
            }
        }
    }
    prune(f);
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

/// `eax` after a `setcc al` that only the low byte of is read: return the byte.
fn narrow_returns(f: &mut Lifted) {
    fn strip(e: Expr) -> Expr {
        match e {
            Expr::Insert(_, n) if n.bits() <= 16 => *n,
            Expr::Trunc(x, _) | Expr::Zext(x, _) if matches!(*x, Expr::Insert(_, ref n) if n.bits() <= 16) => strip(*x),
            other => other,
        }
    }
    for bl in &mut f.blocks {
        if let Term::Return(Some(e)) = &mut bl.term {
            let old = std::mem::replace(e, Expr::Unknown(0));
            *e = strip(old);
        }
    }
}

/// A flag definition reads registers; when one of them is written before the flags are used, keep a copy
/// so that the condition can still be written in terms of the operands.
fn snapshot_flags(f: &mut Lifted) {
    let mut next: u8 = 16;
    for bi in 0..f.blocks.len() {
        let mut i = 0;
        while i < f.blocks[bi].stmts.len() {
            let is_def = matches!(&f.blocks[bi].stmts[i].st, Stmt::Assign(Var::FlagOps, _));
            if !is_def {
                i += 1;
                continue;
            }
            // the registers both flag definitions read
            let mut regs: Vec<u8> = Vec::new();
            let mut j = i;
            let mut end = f.blocks[bi].stmts.len();
            let mut defs = Vec::new();
            while j < end {
                match &f.blocks[bi].stmts[j].st {
                    Stmt::Assign(Var::FlagOps | Var::FlagRes, e) if j <= i + 1 => {
                        defs.push(j);
                        e.visit(&mut |x| {
                            if let Expr::Var(Var::Reg(r), _) = x {
                                if !regs.contains(r) {
                                    regs.push(*r);
                                }
                            }
                        });
                    }
                    Stmt::Assign(Var::FlagOps, _) if j > i + 1 => {
                        end = j;
                    }
                    _ => {}
                }
                j += 1;
            }
            let mut need: Vec<u8> = Vec::new();
            // the last place in the block that reads the flags; the statement that reads them reads before it writes
            let mut last = i + 1;
            for k in i + 1..end {
                let mut reads = false;
                f.blocks[bi].stmts[k].st.uses(&mut |v| reads |= matches!(v, Var::FlagOps | Var::FlagRes));
                if reads {
                    last = k;
                }
            }
            if end == f.blocks[bi].stmts.len()
                && f.blocks[bi].term.exprs().iter().any(|e| e.uses(Var::FlagOps) > 0 || e.uses(Var::FlagRes) > 0)
            {
                last = end;
            }
            for st in &f.blocks[bi].stmts[i + 1..last] {
                match &st.st {
                    Stmt::Assign(Var::Reg(r), _) if regs.contains(r) && !need.contains(r) => need.push(*r),
                    Stmt::Clobber(vs) => {
                        for v in vs {
                            if let Var::Reg(r) = v {
                                if regs.contains(r) && !need.contains(r) {
                                    need.push(*r);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut ins = 0;
            // a value read from memory that memory writes may change before the flags are used
            if next < u8::MAX && f.blocks[bi].stmts[i + 1..last].iter().any(|s| s.st.writes_mem()) {
                let mut load: Option<Expr> = None;
                if let Stmt::Assign(_, e) = &f.blocks[bi].stmts[i].st {
                    e.visit(&mut |x| {
                        if load.is_none() && matches!(x, Expr::Load(..)) {
                            load = Some(x.clone());
                        }
                    });
                }
                if let Some(l) = load {
                    let t = Var::Tmp(next);
                    next += 1;
                    let lb = l.bits();
                    let addr = f.blocks[bi].stmts[i].addr;
                    for &d in &defs {
                        if let Stmt::Assign(_, e) = &mut f.blocks[bi].stmts[d].st {
                            e.rewrite(&mut |x| {
                                if *x == l {
                                    *x = Expr::Var(t, lb);
                                }
                            });
                        }
                    }
                    f.blocks[bi].stmts.insert(i, S { addr, st: Stmt::Assign(t, l.zext(64)) });
                    ins += 1;
                }
            }
            for r in need {
                if next == u8::MAX {
                    break;
                }
                let t = Var::Tmp(next);
                next += 1;
                let addr = f.blocks[bi].stmts[i].addr;
                for &d in &defs {
                    if let Stmt::Assign(_, e) = &mut f.blocks[bi].stmts[d + ins].st {
                        e.subst(Var::Reg(r), &Expr::Var(t, 64));
                    }
                }
                f.blocks[bi].stmts.insert(i, S { addr, st: Stmt::Assign(t, Expr::Var(Var::Reg(r), 64)) });
                ins += 1;
            }
            i += ins + 2;
        }
    }
}

/// `al = cond; eax = zx(al)`: the upper bits written by the first are overwritten by the second, so the first
/// can write the whole register.
fn drop_dead_inserts(f: &mut Lifted) {
    for bl in &mut f.blocks {
        for i in 0..bl.stmts.len().saturating_sub(1) {
            let Stmt::Assign(Var::Reg(r), Expr::Insert(old, new)) = &bl.stmts[i].st else { continue };
            if !matches!(&**old, Expr::Var(Var::Reg(q), 64) if q == r) {
                continue;
            }
            let nb = new.bits();
            let Stmt::Assign(Var::Reg(r2), rhs) = &bl.stmts[i + 1].st else { continue };
            if r2 != r {
                continue;
            }
            let mut narrow = true;
            rhs.visit(&mut |x| {
                if let Expr::Var(Var::Reg(q), w) = x {
                    if q == r && *w > nb {
                        narrow = false;
                    }
                }
            });
            // the second statement must write the whole register
            let whole = matches!(rhs, Expr::Zext(_, 64) | Expr::Sext(_, 64));
            if narrow && whole {
                let v = Var::Reg(*r);
                let n = (**new).clone();
                bl.stmts[i].st = Stmt::Assign(v, n.zext(64));
            }
        }
    }
}

/// Turn multiply-and-shift sequences into divisions and remainders by their constant.
fn recover_divs(f: &mut Lifted) {
    super::divrec::recover_fn(f);
}

/// A callee that never writes a register leaves it as it was: the caller must not treat it as lost.
/// (Compilers do this on purpose for functions of the same file, "ipa-ra".)
fn keep_across_calls(f: &mut Lifted, sigs: &mut dyn Sigs) {
    for bl in &mut f.blocks {
        for i in 0..bl.stmts.len().saturating_sub(1) {
            let target = match &bl.stmts[i].st {
                Stmt::Assign(_, Expr::Call(c)) | Stmt::Eval(Expr::Call(c)) => c.target.clone(),
                _ => continue,
            };
            if matches!(target, Target::Indirect(_)) {
                continue;
            }
            let keep = sigs.keeps(&target);
            if keep.is_empty() {
                continue;
            }
            if let Stmt::Clobber(vs) = &mut bl.stmts[i + 1].st {
                vs.retain(|v| !matches!(v, Var::Reg(r) if keep.contains(r)));
            }
        }
    }
}

/// The caller-saved registers that code from `start` to `end` never writes (empty when it ends in
/// a tail call, which hands the question to another function).
pub fn unwritten_regs(env: &dyn super::lift::Env, start: u64, end: u64, abi: super::lift::Abi) -> Vec<u8> {
    let f = super::lift::lift(env, start, end, abi);
    let mut written: HashSet<u8> = HashSet::new();
    for bl in &f.blocks {
        if matches!(bl.term, Term::TailCall(_) | Term::Indirect(_)) {
            return Vec::new();
        }
        for s in &bl.stmts {
            for k in stmt_def_keys(&s.st) {
                if let Key::V(Var::Reg(r)) = k {
                    written.insert(r);
                }
            }
        }
    }
    abi.caller_saved().iter().copied().filter(|r| !written.contains(r)).collect()
}
