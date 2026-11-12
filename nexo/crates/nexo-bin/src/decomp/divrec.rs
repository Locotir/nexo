//! Division and remainder by a constant, recovered from what compilers emit at -O2: a multiply by
//! a "magic" number and shifts (`umulh`/`smulh` or a widening `imul`), with fix-ups for the sign.
//!
//! The pattern is not matched by shape. A small expression with one free input is run on test
//! values; if it behaves exactly like `x / C` or `x % C` (signed or unsigned) for one constant `C`
//! found by search, and for every boundary around the multiples of `C`, it is replaced by that.

use super::flow::{stmt_def_keys, Key};
use super::ir::*;
use super::lift::Lifted;
use std::collections::{HashMap, HashSet};

/// Largest expression kept while following values through a block.
const MAX_TRACK: usize = 120;

/// Recover divisions across the statements of each block: a value is followed through the
/// registers it passes through, so `x / 7` is found even when the multiply, the shifts and the
/// fix-ups are separate instructions that overwrite the input.
pub fn recover_fn(f: &mut Lifted) {
    let mut used: Vec<u8> = Vec::new();
    for bl in &f.blocks {
        for s in &bl.stmts {
            if let Stmt::Assign(Var::Tmp(n), _) = &s.st {
                used.push(*n);
            }
            for e in s.st.exprs() {
                e.vars(&mut |v| {
                    if let Var::Tmp(n) = v {
                        used.push(n);
                    }
                });
            }
        }
    }
    let mut free: Vec<u8> = (2..16u8).filter(|n| !used.contains(n)).collect();
    for bl in &mut f.blocks {
        recover_block(bl, &mut free);
    }
}

fn tracked(v: Var) -> bool {
    matches!(v, Var::Reg(_) | Var::Tmp(_))
}

/// The expression with every tracked variable replaced by what it holds at the block's start.
fn resolve(e: &Expr, env: &HashMap<Var, Expr>, dirty: &HashSet<Var>) -> Option<Expr> {
    let mut bad = false;
    let mut out = e.clone();
    out.rewrite(&mut |n| {
        if let Expr::Var(x, w) = n {
            if dirty.contains(x) {
                bad = true;
            } else if let Some(val) = env.get(x) {
                let w = *w;
                *n = if val.bits() == w { val.clone() } else { val.clone().trunc(w) };
            }
        }
    });
    (!bad).then_some(out)
}

fn recover_block(bl: &mut Block, free: &mut Vec<u8>) {
    let mut env: HashMap<Var, Expr> = HashMap::new();
    let mut dirty: HashSet<Var> = HashSet::new();
    let mut snaps: Vec<(Var, u8, Bits)> = Vec::new();
    let addr0 = bl.stmts.first().map_or(bl.term_addr, |s| s.addr);
    // a changed expression written in names of the block's start, ready to store back
    let mut fix = |full: &Expr,
                   env: &HashMap<Var, Expr>,
                   dirty: &HashSet<Var>,
                   snaps: &mut Vec<(Var, u8, Bits)>|
     -> Option<Expr> {
        let mut r = full.clone();
        recover(&mut r);
        if r == *full {
            return None;
        }
        let mut vs: Vec<Var> = Vec::new();
        r.vars(&mut |v| {
            if !vs.contains(&v) {
                vs.push(v);
            }
        });
        for v in vs {
            if env.contains_key(&v) || dirty.contains(&v) {
                let t = match snaps.iter().find(|s| s.0 == v) {
                    Some(s) => s.1,
                    None => {
                        let t = free.pop()?;
                        snaps.push((v, t, 0));
                        t
                    }
                };
                let mut wide = 0;
                r.visit(&mut |n| {
                    if let Expr::Var(x, w) = n {
                        if *x == v {
                            wide = wide.max(*w);
                        }
                    }
                });
                snaps.iter_mut().filter(|s| s.0 == v).for_each(|s| s.2 = s.2.max(wide));
                r.rewrite(&mut |n| {
                    if let Expr::Var(x, w) = n {
                        if *x == v {
                            *n = Expr::Var(Var::Tmp(t), *w);
                        }
                    }
                });
            }
        }
        Some(r)
    };
    for i in 0..bl.stmts.len() {
        let st = bl.stmts[i].st.clone();
        match &st {
            Stmt::Assign(v, e) if tracked(*v) => {
                let full = if e.has_load() || e.has_call() { None } else { resolve(e, &env, &dirty) };
                match full {
                    Some(full) if full.size() <= MAX_TRACK => {
                        let mut keep = full.clone();
                        if let Some(r) = fix(&full, &env, &dirty, &mut snaps) {
                            keep = {
                                let mut k = full.clone();
                                recover(&mut k);
                                k
                            };
                            bl.stmts[i].st = Stmt::Assign(*v, r);
                        }
                        dirty.remove(v);
                        env.insert(*v, keep);
                    }
                    _ => {
                        env.remove(v);
                        dirty.insert(*v);
                    }
                }
            }
            other => {
                for k in stmt_def_keys(other) {
                    if let Key::V(v) = k {
                        if tracked(v) {
                            env.remove(&v);
                            dirty.insert(v);
                        }
                    }
                }
            }
        }
    }
    // the value the block returns or branches on
    let mut changes: Vec<(usize, Expr)> = Vec::new();
    for (k, e) in bl.term.exprs().into_iter().enumerate() {
        if e.has_load() || e.has_call() {
            continue;
        }
        if let Some(full) = resolve(e, &env, &dirty) {
            if full.size() <= MAX_TRACK {
                if let Some(r) = fix(&full, &env, &dirty, &mut snaps) {
                    changes.push((k, r));
                }
            }
        }
    }
    for (j, r) in changes {
        if let Some(slot) = bl.term.exprs_mut().into_iter().nth(j) {
            *slot = r;
        }
    }
    for (v, t, wide) in snaps.into_iter().rev() {
        bl.stmts.insert(0, S { addr: addr0, st: Stmt::Assign(Var::Tmp(t), Expr::Var(v, wide.clamp(1, 64)).zext(64)) });
    }
}

/// Largest expression tried (nodes).
const MAX_SIZE: usize = 80;

/// Rewrite every sub-expression of `e` that is a division or remainder by a constant.
pub fn recover(e: &mut Expr) {
    e.rewrite(&mut |n| {
        if candidate(n) {
            if let Some(r) = try_node(n) {
                *n = r;
            }
        }
    });
}

/// Cheap tests before the search: a shape that could hold a magic multiply or a rounding shift.
fn candidate(e: &Expr) -> bool {
    if !matches!(e.bits(), 32 | 64) || e.size() < 4 || e.size() > MAX_SIZE {
        return false;
    }
    if matches!(e, Expr::Bin(Bin::UDiv | Bin::SDiv | Bin::UMod | Bin::SMod, _, c) if c.as_const().is_some()) {
        return false;
    }
    if !matches!(e, Expr::Bin(..) | Expr::Select(..) | Expr::Trunc(..) | Expr::Zext(..) | Expr::Sext(..)) {
        return false;
    }
    let mut sig = false;
    e.visit(&mut |n| match n {
        Expr::Intr("umulh" | "smulh", ..) => sig = true,
        Expr::Bin(Bin::UDiv | Bin::SDiv, _, c) if c.as_const().is_some() => sig = true,
        Expr::Bin(Bin::Mul, _, c) if c.as_const().is_some_and(|v| v > 0xff) => sig = true,
        Expr::Bin(Bin::Sar | Bin::Shr, _, c) if c.as_const().is_some() => sig = true,
        _ => {}
    });
    sig
}

/// The one free input of the expression, when there is exactly one.
fn leaf_of(e: &Expr) -> Option<Expr> {
    let mut leaf: Option<Expr> = None;
    let mut ok = true;
    let mut seen = |n: &Expr, ok: &mut bool| {
        match &leaf {
            None => leaf = Some(n.clone()),
            Some(l) if l == n => {}
            Some(_) => *ok = false,
        };
    };
    fn walk(e: &Expr, seen: &mut impl FnMut(&Expr, &mut bool), ok: &mut bool) {
        use Expr::*;
        match e {
            Const(..) => {}
            Var(x, _) => seen(&Expr::Var(*x, 64), ok),
            Load(..) | Call(_) | Unknown(_) | StackAddr(_) | SegBase(_) => seen(e, ok),
            Un(_, a) | Trunc(a, _) | Zext(a, _) | Sext(a, _) => walk(a, seen, ok),
            Bin(op, a, c) if !matches!(op, self::Bin::FAdd | self::Bin::FSub | self::Bin::FMul | self::Bin::FDiv) => {
                walk(a, seen, ok);
                walk(c, seen, ok);
            }
            Cmp(op, a, c) if !op.is_float() => {
                walk(a, seen, ok);
                walk(c, seen, ok);
            }
            Select(c, a, d) => {
                walk(c, seen, ok);
                walk(a, seen, ok);
                walk(d, seen, ok);
            }
            Intr("umulh" | "smulh", args, _) => args.iter().for_each(|a| walk(a, seen, ok)),
            _ => *ok = false,
        }
    }
    walk(e, &mut seen, &mut ok);
    if ok {
        leaf
    } else {
        None
    }
}

/// The value of `e` when the free input is `x`; None when something cannot be computed.
fn eval(e: &Expr, leaf: &Expr, x: u64) -> Option<u64> {
    use Expr::*;
    if e == leaf || matches!((e, leaf), (Expr::Var(p, _), Expr::Var(q, 64)) if p == q) {
        return Some(x & mask(e.bits()));
    }
    let w = e.bits();
    let v = match e {
        Const(v, _) => *v,
        Un(op, a) => {
            let a = eval(a, leaf, x)?;
            match op {
                self::Un::Neg => a.wrapping_neg(),
                self::Un::Not => !a,
                self::Un::LNot => u64::from(a == 0),
            }
        }
        Bin(op, a, c) => {
            let (p, q) = (eval(a, leaf, x)?, eval(c, leaf, x)?);
            match op {
                self::Bin::LAnd => u64::from(p != 0 && q != 0),
                self::Bin::LOr => u64::from(p != 0 || q != 0),
                _ => fold_bin(*op, p, q, a.bits())?,
            }
        }
        Cmp(op, a, c) => u64::from(fold_cmp(*op, eval(a, leaf, x)?, eval(c, leaf, x)?, a.bits())?),
        Trunc(a, _) => eval(a, leaf, x)?,
        Zext(a, _) => eval(a, leaf, x)?,
        Sext(a, _) => signed(eval(a, leaf, x)?, a.bits()) as u64,
        Select(c, a, d) => {
            if eval(c, leaf, x)? != 0 {
                eval(a, leaf, x)?
            } else {
                eval(d, leaf, x)?
            }
        }
        Intr(name @ ("umulh" | "smulh"), args, bits) if args.len() == 2 => {
            let (p, q) = (eval(&args[0], leaf, x)?, eval(&args[1], leaf, x)?);
            let wide = if *name == "umulh" {
                (u128::from(p) * u128::from(q)) >> *bits
            } else {
                ((i128::from(signed(p, *bits)) * i128::from(signed(q, *bits))) >> *bits) as u128
            };
            wide as u64
        }
        _ => return None,
    };
    Some(v & mask(w))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    UDiv,
    SDiv,
    UMod,
    SMod,
    /// `x / -C`
    NSDiv,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// What `kind` gives for `x` over `c`, in `w` bits.
fn expect(kind: Kind, x: u64, c: u64, w: Bits) -> u64 {
    let r = match kind {
        Kind::UDiv => x / c,
        Kind::UMod => x % c,
        Kind::SDiv => signed(x, w).wrapping_div(signed(c, w)) as u64,
        Kind::NSDiv => signed(x, w).wrapping_div(signed(c, w)).wrapping_neg() as u64,
        Kind::SMod => signed(x, w).wrapping_rem(signed(c, w)) as u64,
    };
    r & mask(w)
}

fn try_node(e: &Expr) -> Option<Expr> {
    let leaf = leaf_of(e)?;
    let nb = e.bits();
    let widths: &[Bits] = if leaf.bits() >= 64 {
        &[32, 64]
    } else if leaf.bits() == 32 {
        &[32]
    } else {
        return None;
    };
    for &wx in widths {
        if nb < wx {
            continue;
        }
        for kind in [Kind::UDiv, Kind::SDiv, Kind::UMod, Kind::SMod, Kind::NSDiv] {
            if let Some(c) = find_c(e, &leaf, kind, wx, nb) {
                if verify(e, &leaf, kind, wx, nb, c) {
                    return Some(build(&leaf, kind, wx, nb, c));
                }
            }
        }
    }
    None
}

/// The expression's result for the input `x` (low `wx` bits; the rest of a wider leaf is zero).
fn run(e: &Expr, leaf: &Expr, x: u64) -> Option<u64> {
    eval(e, leaf, x)
}

/// The result read as a `wx`-bit value of the same kind as `expect` (None when the extra bits of a
/// wider result are not the extension of it).
fn narrow(r: u64, kind: Kind, wx: Bits, nb: Bits) -> Option<u64> {
    if nb == wx {
        return Some(r);
    }
    let low = r & mask(wx);
    let ext = match kind {
        Kind::SDiv | Kind::SMod | Kind::NSDiv => signed(low, wx) as u64 & mask(nb),
        _ => low,
    };
    (ext == r).then_some(low)
}

/// Search the constant from the behaviour near zero.
fn find_c(e: &Expr, leaf: &Expr, kind: Kind, wx: Bits, nb: Bits) -> Option<u64> {
    let get = |x: u64| run(e, leaf, x).and_then(|r| narrow(r, kind, wx, nb));
    let top = match kind {
        Kind::UDiv | Kind::UMod => mask(wx),
        Kind::SDiv | Kind::SMod | Kind::NSDiv => mask(wx) >> 1,
    };
    if get(0)? != 0 {
        return None;
    }
    // the predicate that is false below C and true from C on
    let past = |x: u64| -> Option<bool> {
        let r = get(x)?;
        Some(match kind {
            Kind::UDiv | Kind::SDiv | Kind::NSDiv => r != 0,
            Kind::UMod | Kind::SMod => r != x,
        })
    };
    if !past(top)? || past(1)? {
        return None;
    }
    let (mut lo, mut hi) = (1u64, top);
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if past(mid)? {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (hi >= 2).then_some(hi)
}

fn verify(e: &Expr, leaf: &Expr, kind: Kind, wx: Bits, nb: Bits, c: u64) -> bool {
    let m = mask(wx);
    let signed_kind = matches!(kind, Kind::SDiv | Kind::SMod | Kind::NSDiv);
    let mut xs: Vec<u64> = Vec::new();
    for v in 0..=70u64 {
        xs.push(v);
    }
    for k in 1..=70u64 {
        if let Some(kc) = k.checked_mul(c) {
            for d in [kc.wrapping_sub(1), kc, kc.wrapping_add(1)] {
                xs.push(d);
            }
        }
    }
    let top = if signed_kind { m >> 1 } else { m };
    for d in 0..=70u64 {
        xs.push(top.wrapping_sub(d));
    }
    let mut r = Rng(0x9e37_79b9_7f4a_7c15 ^ c);
    for _ in 0..300 {
        let v = r.next();
        xs.push(v);
        xs.push(v >> (r.next() % u64::from(wx)));
    }
    if signed_kind {
        let neg: Vec<u64> = xs.iter().map(|v| v.wrapping_neg()).collect();
        xs.extend(neg);
        xs.push(1u64 << (wx - 1));
        xs.push((1u64 << (wx - 1)) + 1);
    }
    for v in xs {
        let x = v & m;
        let want = expect(kind, x, c, wx);
        // the bits above `wx` of a wider input must not matter
        let uppers: &[u64] = if leaf.bits() > wx { &[0, r.next() << wx, u64::MAX << wx] } else { &[0] };
        for up in uppers {
            let Some(got) = run(e, leaf, x | (up & !m)) else { return false };
            if narrow(got, kind, wx, nb) != Some(want) {
                return false;
            }
        }
    }
    true
}

fn build(leaf: &Expr, kind: Kind, wx: Bits, nb: Bits, c: u64) -> Expr {
    let x = match leaf {
        Expr::Var(v, _) => Expr::Var(*v, wx),
        _ => leaf.clone().trunc(wx),
    };
    let op = match kind {
        Kind::UDiv => Bin::UDiv,
        Kind::SDiv => Bin::SDiv,
        Kind::UMod => Bin::UMod,
        Kind::SMod => Bin::SMod,
        Kind::NSDiv => Bin::SDiv,
    };
    let q = Expr::bin(op, x, Expr::Const(c, wx));
    match kind {
        Kind::NSDiv => Expr::Un(self::Un::Neg, Box::new(q)).sext(nb),
        Kind::SDiv | Kind::SMod => q.sext(nb),
        _ => q.zext(nb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(bits: Bits) -> Expr {
        Expr::Var(Var::Reg(7), bits)
    }

    #[test]
    fn unsigned_div10_from_a_wide_multiply() {
        // (zext(edi) * 0xcccccccd) >> 35
        let x = Expr::Zext(Box::new(Expr::Trunc(Box::new(v(64)), 32)), 64);
        let mut e = Expr::bin(Bin::Shr, Expr::bin(Bin::Mul, x, Expr::Const(0xcccc_cccd, 64)), Expr::Const(35, 64));
        recover(&mut e);
        let want = Expr::Zext(Box::new(Expr::bin(Bin::UDiv, Expr::Var(Var::Reg(7), 32), Expr::Const(10, 32))), 64);
        assert_eq!(e, want, "{e:?}");
    }

    #[test]
    fn a_plain_shift_stays() {
        let mut e = Expr::bin(Bin::Shr, Expr::bin(Bin::Add, v(32), Expr::Const(3, 32)), Expr::Const(2, 32));
        let before = e.clone();
        recover(&mut e);
        assert_eq!(e, before);
    }

    const C_SRC: &str = r#"
#include <stdint.h>
#define F(n,t,op,c) __attribute__((noinline)) t n(t x){return x op c;}
F(u32_div10,uint32_t,/,10)
F(u32_mod10,uint32_t,%,10)
F(u32_div7,uint32_t,/,7)
F(u32_div1000,uint32_t,/,1000)
F(u64_div10,uint64_t,/,10)
F(u64_mod1000,uint64_t,%,1000)
F(u64_div7,uint64_t,/,7)
F(i32_div10,int32_t,/,10)
F(i32_mod10,int32_t,%,10)
F(i32_div7,int32_t,/,7)
F(i32_div1000,int32_t,/,1000)
F(i64_div10,int64_t,/,10)
F(i64_mod7,int64_t,%,7)
F(i64_div1000,int64_t,/,1000)
F(u32_div8,uint32_t,/,8)
F(u32_mod8,uint32_t,%,8)
F(i32_div8,int32_t,/,8)
F(i32_mod8,int32_t,%,8)
F(i64_div16,int64_t,/,16)
F(i64_mod16,int64_t,%,16)
F(u64_div3,uint64_t,/,3)
F(i32_divm3,int32_t,/,-3)
int main(void) { return 0; }
"#;

    fn build_o2(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let src = dir.join("d.c");
        std::fs::write(&src, C_SRC).ok()?;
        let exe = dir.join("d");
        let ok = ["cc", "gcc", "clang"].iter().any(|cc| {
            std::process::Command::new(cc)
                .args(["-O2", "-fno-pie", "-no-pie", "-o"])
                .arg(&exe)
                .arg(&src)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        });
        if !ok {
            eprintln!("skipped: no C compiler");
            return None;
        }
        Some(exe)
    }

    fn has(text: &str, want: &str) -> bool {
        let (op, n) = want.split_once(" ").unwrap();
        if op == "&" {
            return text.contains(want);
        }
        let n: i64 = n.parse().unwrap();
        let hex = if n < 0 { format!("-{:#x}", -n) } else { format!("{n:#x}") };
        text.contains(&format!("{op} {n}")) || text.contains(&format!("{op} {hex}"))
    }

    /// Every function of the C sample must read back as its source operator and constant.
    #[test]
    fn compiled_divisions_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = build_o2(dir.path()) else { return };
        let img = crate::image::Image::open(&exe).unwrap();
        let data_dir = dir.path().join("data");
        let cases = [
            ("u32_div10", "/ 10"),
            ("u32_mod10", "% 10"),
            ("u32_div7", "/ 7"),
            ("u32_div1000", "/ 1000"),
            ("u64_div10", "/ 10"),
            ("u64_mod1000", "% 1000"),
            ("u64_div7", "/ 7"),
            ("i32_div10", "/ 10"),
            ("i32_mod10", "% 10"),
            ("i32_div7", "/ 7"),
            ("i32_div1000", "/ 1000"),
            ("i64_div10", "/ 10"),
            ("i64_mod7", "% 7"),
            ("i64_div1000", "/ 1000"),
            ("u32_div8", "/ 8"),
            ("u32_mod8", "& 7"),
            ("i32_div8", "/ 8"),
            ("i32_mod8", "% 8"),
            ("i64_div16", "/ 16"),
            ("i64_mod16", "% 16"),
            ("u64_div3", "/ 3"),
            ("i32_divm3", "/ 3"),
        ];
        let mut bad = Vec::new();
        for (name, want) in cases {
            let o = super::super::api::decomp_window(&img, &data_dir, &[name.to_string()], false, None, 200).unwrap();
            let text = o.text_all();
            if !has(&text, want) || text.contains(">>") || text.contains("umulh") || text.contains("smulh") {
                bad.push(format!("{name} wants `{want}`:\n{text}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
