//! Tests for IR folding and simplification.

use super::*;

fn r(n: u8, w: Bits) -> Expr {
    Expr::Var(Var::Reg(n), w)
}
fn flags(kind: FlagKind, a: Expr, c: Expr) -> Expr {
    Expr::Flags(kind, b(a), b(c))
}
fn cond(cc: Cc, ops: Expr) -> Expr {
    simplify(Expr::Cond(cc, b(ops), b(Expr::Var(Var::FlagRes, 0))))
}

#[test]
fn constants_fold() {
    let e = simplify(Expr::bin(Bin::Add, Expr::Const(2, 32), Expr::Const(0xffff_ffff, 32)));
    assert_eq!(e, Expr::Const(1, 32));
    let e = simplify(Expr::bin(Bin::Sub, r(7, 64), Expr::Const(8, 64)));
    assert_eq!(e, Expr::bin(Bin::Add, r(7, 64), Expr::Const(0xffff_ffff_ffff_fff8, 64)));
}

#[test]
fn cmp_signed_and_unsigned() {
    let ops = flags(FlagKind::Sub, r(7, 32), r(6, 32));
    assert_eq!(cond(Cc::L, ops.clone()), Expr::Cmp(Cmp::Lt, b(r(7, 32)), b(r(6, 32))));
    assert_eq!(cond(Cc::B, ops.clone()), Expr::Cmp(Cmp::Ult, b(r(7, 32)), b(r(6, 32))));
    assert_eq!(cond(Cc::A, ops.clone()), Expr::Cmp(Cmp::Ugt, b(r(7, 32)), b(r(6, 32))));
    assert_eq!(cond(Cc::NE, ops), Expr::Cmp(Cmp::Ne, b(r(7, 32)), b(r(6, 32))));
}

#[test]
fn test_self_is_compare_with_zero() {
    let ops = flags(FlagKind::Logic, r(0, 32), r(0, 32));
    assert_eq!(cond(Cc::E, ops.clone()), Expr::Cmp(Cmp::Eq, b(r(0, 32)), b(Expr::Const(0, 32))));
    assert_eq!(cond(Cc::S, ops), Expr::Cmp(Cmp::Lt, b(r(0, 32)), b(Expr::Const(0, 32))));
}

#[test]
fn negation_flips_compare() {
    let c = Expr::Cmp(Cmp::Lt, b(r(0, 32)), b(r(1, 32)));
    assert_eq!(c.not(), Expr::Cmp(Cmp::Ge, b(r(0, 32)), b(r(1, 32))));
}

#[test]
fn partial_write_over_zero_is_zext() {
    let e = simplify(Expr::Insert(b(Expr::Const(0, 32)), b(r(0, 8))));
    assert_eq!(e, Expr::Zext(b(r(0, 8)), 32));
}

#[test]
fn subst_replaces_and_folds() {
    let mut e = Expr::add(r(0, 64), Expr::Const(4, 64));
    assert_eq!(e.subst(Var::Reg(0), &Expr::Const(8, 64)), 1);
    assert_eq!(e, Expr::Const(12, 64));
}
