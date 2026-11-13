//! Copy propagation, dead-code removal and expression folding over lifted blocks.

use super::*;

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
