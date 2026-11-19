//! Pseudo-C text from the structured tree.

use std::collections::{BTreeMap, HashMap};

use super::flow::{analyze, Facts, Key, ENTRY};
use super::ir::*;
use super::lift::Lifted;
use super::structure::Node;

/// What the printer asks the file about.
pub trait Names {
    fn func(&self, addr: u64) -> Option<String>;
    fn import(&self, i: usize) -> String;
    fn string(&self, addr: u64) -> Option<String>;
    fn global(&self, addr: u64) -> Option<String>;
    /// User names for variables: key like `a1`, `v3`, `local_18`.
    fn var_name(&self, _key: &str) -> Option<String> {
        None
    }
    /// Where the debugger saw the indirect call at `site` go, as a comment body.
    fn call_note(&self, _site: u64) -> Option<String> {
        None
    }
    /// The field at this offset of the struct the variable points to.
    fn field(&self, _key: &str, _off: u64) -> Option<String> {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub addr: Option<u64>,
    pub indent: usize,
    pub text: String,
}

pub struct WebInfo {
    pub names: HashMap<u32, String>,
    pub bits: HashMap<u32, Bits>,
    pub signed: HashMap<u32, bool>,
    pub float: HashMap<u32, bool>,
    /// Width (32 or 64) of every web that holds a float or a double.
    pub fwidth: HashMap<u32, Bits>,
    pub args: Vec<(String, u32)>,
}

// ---------------------------------------------------------------------------------------------
// Renaming registers into variables

fn find(p: &mut [usize], x: usize) -> usize {
    let mut r = x;
    while p[r] != r {
        r = p[r];
    }
    let mut c = x;
    while p[c] != r {
        let n = p[c];
        p[c] = r;
        c = n;
    }
    r
}

fn renamable(v: Var) -> bool {
    matches!(v, Var::Reg(_) | Var::Xmm(_) | Var::Tmp(_))
}

/// Give every live range of a register its own variable.
pub fn make_webs(f: &mut Lifted, facts: &Facts) -> WebInfo {
    let an = analyze(f);
    let nd = an.defs.len();
    let mut p: Vec<usize> = (0..nd).collect();
    for u in &an.uses {
        let Key::V(v) = u.key else { continue };
        if !renamable(v) {
            continue;
        }
        for w in u.reach.windows(2) {
            let (a, b) = (find(&mut p, w[0]), find(&mut p, w[1]));
            if a != b {
                p[a] = b;
            }
        }
    }
    // numbering in order of first definition
    let mut id_of: HashMap<usize, u32> = HashMap::new();
    let mut next = 0u32;
    let mut web_of = |p: &mut Vec<usize>, d: usize| -> u32 {
        let r = find(p, d);
        *id_of.entry(r).or_insert_with(|| {
            next += 1;
            next
        })
    };
    // entry definitions that are arguments come first
    let abi = f.abi;
    let mut arg_webs: Vec<(String, u32)> = Vec::new();
    let mut entry_name: HashMap<u32, String> = HashMap::new();
    for pos in 0..facts.argc {
        let reg = abi.arg_regs()[pos];
        if let Some(&ki) = an.key_ix.get(&Key::V(Var::Reg(reg))) {
            let w = web_of(&mut p, ki);
            let n = format!("a{}", pos + 1);
            entry_name.insert(w, n.clone());
            arg_webs.push((n, w));
        } else {
            arg_webs.push((format!("a{}", pos + 1), 0));
        }
    }
    for x in &facts.float_args {
        if let Some(&ki) = an.key_ix.get(&Key::V(Var::Xmm(*x))) {
            let w = web_of(&mut p, ki);
            let n = format!("f{}", x + 1);
            entry_name.insert(w, n.clone());
            arg_webs.push((n, w));
        }
    }
    if abi == super::lift::Abi::Win64 {
        // one slot per position: `aN` and `fN` share slot N-1, and the one that is read fills it
        let slot = |n: &str| n[1..].parse::<usize>().unwrap_or(0);
        let fslots: Vec<usize> = arg_webs.iter().filter(|(n, _)| n.starts_with('f')).map(|(n, _)| slot(n)).collect();
        arg_webs.retain(|(n, w)| !(n.starts_with('a') && *w == 0 && fslots.contains(&slot(n))));
        arg_webs.sort_by_key(|(n, _)| (slot(n), n.starts_with('f')));
    }
    // rewrite statements
    for bi in 0..f.blocks.len() {
        for si in 0..=f.blocks[bi].stmts.len() {
            let mut map: HashMap<Var, Var> = HashMap::new();
            for &u in &an.use_at[bi][si] {
                let ur = &an.uses[u];
                let Key::V(v) = ur.key else { continue };
                if !renamable(v) {
                    continue;
                }
                if let Some(&d) = ur.reach.first() {
                    map.insert(v, Var::Web(web_of(&mut p, d)));
                }
            }
            let rn = |e: &mut Expr| {
                if let Expr::Var(v, b) = e {
                    if let Some(&w) = map.get(v) {
                        *e = Expr::Var(w, *b);
                    }
                }
            };
            if si < f.blocks[bi].stmts.len() {
                let mut dnew: Option<Var> = None;
                if let Stmt::Assign(v, _) = &f.blocks[bi].stmts[si].st {
                    if renamable(*v) {
                        let d = an.stmt_defs[bi][si].iter().copied().find(|&d| an.defs[d].key == Key::V(*v));
                        if let Some(d) = d {
                            dnew = Some(Var::Web(web_of(&mut p, d)));
                        }
                    }
                }
                let st = &mut f.blocks[bi].stmts[si].st;
                for e in st.exprs_mut() {
                    e.rewrite(&mut { rn });
                }
                if let (Stmt::Assign(v, _), Some(n)) = (st, dnew) {
                    *v = n;
                }
            } else {
                for e in f.blocks[bi].term.exprs_mut() {
                    e.rewrite(&mut { rn });
                }
            }
        }
    }
    // names and widths
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut counter = 0;
    let mut bits: HashMap<u32, Bits> = HashMap::new();
    let mut signed: HashMap<u32, i32> = HashMap::new();
    for (i, id) in id_of.values().enumerate() {
        let _ = i;
        let _ = id;
    }
    fn note(e: &Expr, bits: &mut HashMap<u32, Bits>, signed: &mut HashMap<u32, i32>) {
        e.visit(&mut |x| match x {
            Expr::Var(Var::Web(w), b) => {
                let c = bits.entry(*w).or_insert(0);
                *c = (*c).max(*b);
            }
            Expr::Cmp(op, a, c) => {
                for s in [a, c] {
                    if let Expr::Var(Var::Web(w), _) = &**s {
                        if op.is_signed() {
                            *signed.entry(*w).or_insert(0) += 1;
                        } else if op.is_unsigned() {
                            *signed.entry(*w).or_insert(0) -= 1;
                        }
                    }
                }
            }
            _ => {}
        })
    }
    for bl in &f.blocks {
        for s in &bl.stmts {
            for e in s.st.exprs() {
                note(e, &mut bits, &mut signed);
            }
            if let Stmt::Assign(Var::Web(w), e) = &s.st {
                let c = bits.entry(*w).or_insert(0);
                *c = (*c).max(e.bits());
            }
        }
        for e in bl.term.exprs() {
            note(e, &mut bits, &mut signed);
        }
    }
    let fwidth = float_webs(f, &entry_name, &bits);
    let float: HashMap<u32, bool> = fwidth.keys().map(|k| (*k, true)).collect();
    let mut ids: Vec<u32> = bits.keys().copied().collect();
    ids.extend(entry_name.keys().copied());
    ids.sort();
    ids.dedup();
    for w in ids {
        if let Some(n) = entry_name.get(&w) {
            names.insert(w, n.clone());
        } else {
            counter += 1;
            names.insert(w, format!("v{counter}"));
        }
    }
    WebInfo { names, bits, signed: signed.into_iter().map(|(k, v)| (k, v >= 0)).collect(), float, fwidth, args: arg_webs }
}

/// The operands a float operation reads directly: the places that make a web a float.
fn float_operands<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
    fn walk<'e>(e: &'e Expr, out: &mut Vec<&'e Expr>) {
        match e {
            Expr::Bin(Bin::FAdd | Bin::FSub | Bin::FMul | Bin::FDiv, a, c) => out.extend([&**a, &**c]),
            Expr::Cmp(op, a, c) if op.is_float() => out.extend([&**a, &**c]),
            Expr::Flags(FlagKind::FCmp, a, c) => out.extend([&**a, &**c]),
            Expr::Un(Un::FNeg, a) => out.push(a),
            Expr::Select(_, a, d) if e.is_float_op() => out.extend([&**a, &**d]),
            Expr::Intr(n, args, _)
                if matches!(*n, "sqrt" | "sqrtf" | "fabs" | "fabsf" | "fext" | "ftrunc" | "ftoi" | "lrint") =>
            {
                out.extend(args.iter())
            }
            _ => {}
        }
        match e {
            Expr::Const(..) | Expr::Var(..) | Expr::Unknown(_) | Expr::StackAddr(_) | Expr::SegBase(_) => {}
            Expr::Load(a, _) | Expr::Un(_, a) | Expr::Trunc(a, _) | Expr::Zext(a, _) | Expr::Sext(a, _) => {
                walk(a, out)
            }
            Expr::Bin(_, a, c)
            | Expr::Insert(a, c)
            | Expr::Cmp(_, a, c)
            | Expr::Flags(_, a, c)
            | Expr::Cond(_, a, c) => {
                walk(a, out);
                walk(c, out);
            }
            Expr::Select(a, c, d) => {
                walk(a, out);
                walk(c, out);
                walk(d, out);
            }
            Expr::Call(c) => {
                if let Target::Indirect(t) = &c.target {
                    walk(t, out);
                }
                c.args.iter().for_each(|a| walk(a, out));
            }
            Expr::Intr(_, args, _) => args.iter().for_each(|a| walk(a, out)),
        }
    }
    walk(e, out);
}


/// The web a float operand names, looking through a narrowing cast.
fn web_of_operand(e: &Expr) -> Option<u32> {
    match e {
        Expr::Var(Var::Web(w), _) => Some(*w),
        Expr::Trunc(x, _) => web_of_operand(x),
        _ => None,
    }
}

/// Which webs hold floats, and how wide: float arguments, targets of float operations, direct operands of float
/// operations, and (to a fixed point) anything copied from or to such a web.
fn float_webs(f: &Lifted, entry_name: &HashMap<u32, String>, bits: &HashMap<u32, Bits>) -> HashMap<u32, Bits> {
    let mut fl: std::collections::HashSet<u32> =
        entry_name.iter().filter(|(_, n)| n.starts_with('f')).map(|(w, _)| *w).collect();
    let mut copies: Vec<(u32, u32)> = Vec::new();
    let scan = |e: &Expr, fl: &mut std::collections::HashSet<u32>| {
        let mut ops = Vec::new();
        float_operands(e, &mut ops);
        for o in ops {
            if let Some(w) = web_of_operand(o) {
                fl.insert(w);
            }
        }
    };
    for bl in &f.blocks {
        for s in &bl.stmts {
            for e in s.st.exprs() {
                scan(e, &mut fl);
            }
            if let Stmt::Assign(Var::Web(w), e) = &s.st {
                if e.is_float_op() || matches!(e, Expr::Intr("as_float", ..)) {
                    fl.insert(*w);
                } else if let Some(o) = web_of_operand(e) {
                    copies.push((*w, o));
                }
            }
        }
        for e in bl.term.exprs() {
            scan(e, &mut fl);
        }
    }
    for _ in 0..=copies.len() {
        let mut grew = false;
        for &(a, b) in &copies {
            if fl.contains(&a) != fl.contains(&b) {
                fl.insert(a);
                fl.insert(b);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    fl.into_iter().map(|w| (w, if bits.get(&w) == Some(&32) { 32 } else { 64 })).collect()
}

// ---------------------------------------------------------------------------------------------
// Expressions

pub struct Printer<'a> {
    /// The function returns a float or a double: the returned constants and loads are floats.
    pub fret: bool,
    pub names: &'a dyn Names,
    pub web: &'a WebInfo,
    pub abi_stack_arg: i64,
    /// AArch64 register names (x0..x30, sp, v0..) instead of the x86 ones.
    pub a64: bool,
    pub block_addr: Vec<u64>,
    pub lines: Vec<Line>,
    pub indent: usize,
    pub slots: BTreeMap<i64, Bits>,
}

fn hex(v: u64) -> String {
    if v < 10 {
        v.to_string()
    } else {
        format!("0x{v:x}")
    }
}

fn prec(op: Bin) -> u8 {
    match op {
        Bin::LOr => 1,
        Bin::LAnd => 2,
        Bin::Or => 3,
        Bin::Xor => 4,
        Bin::And => 5,
        Bin::Shl | Bin::Shr | Bin::Sar => 8,
        Bin::Add | Bin::Sub | Bin::FAdd | Bin::FSub => 9,
        _ => 10,
    }
}

fn op_text(op: Bin) -> &'static str {
    match op {
        Bin::Add | Bin::FAdd => "+",
        Bin::Sub | Bin::FSub => "-",
        Bin::Mul | Bin::FMul => "*",
        Bin::And => "&",
        Bin::Or => "|",
        Bin::Xor => "^",
        Bin::Shl => "<<",
        Bin::Shr | Bin::Sar => ">>",
        Bin::UDiv | Bin::SDiv | Bin::FDiv => "/",
        Bin::UMod | Bin::SMod => "%",
        Bin::LAnd => "&&",
        Bin::LOr => "||",
    }
}

fn utype(b: Bits) -> &'static str {
    match b {
        1 => "bool",
        8 => "u8",
        16 => "u16",
        32 => "u32",
        128 => "u128",
        _ => "u64",
    }
}

fn itype(b: Bits) -> &'static str {
    match b {
        1 => "bool",
        8 => "i8",
        16 => "i16",
        32 => "i32",
        128 => "i128",
        _ => "i64",
    }
}

impl Printer<'_> {
    fn var_text(&self, v: Var, _bits: Bits) -> String {
        match v {
            Var::Web(w) => {
                let n = self.web.names.get(&w).cloned().unwrap_or_else(|| format!("v{w}"));
                self.names.var_name(&n).unwrap_or(n)
            }
            Var::Reg(31) if self.a64 => "sp".to_string(),
            Var::Reg(r) if self.a64 => format!("x{r}"),
            Var::Reg(r) => REG_NAMES[r as usize & 15].to_string(),
            Var::Xmm(x) if self.a64 => format!("vreg{x}"),
            Var::Xmm(x) => format!("xmm{x}"),
            Var::Stack(o) => self.slot_name(i64::from(o)),
            Var::FlagOps => "flags".into(),
            Var::FlagRes => "flags_res".into(),
            Var::Tmp(t) => format!("tmp{t}"),
        }
    }

    fn slot_key(&self, o: i64) -> String {
        if o < 0 {
            format!("local_{:x}", -o)
        } else if o >= self.abi_stack_arg {
            format!("stack_{:x}", o - self.abi_stack_arg)
        } else {
            format!("saved_{o:x}")
        }
    }

    fn slot_name(&self, o: i64) -> String {
        let n = self.slot_key(o);
        self.names.var_name(&n).unwrap_or(n)
    }

    fn konst(&self, v: u64, bits: Bits) -> String {
        if bits >= 32 {
            if let Some(s) = self.names.string(v) {
                return format!("\"{}\"", s);
            }
            if v >= 0x1000 {
                if let Some(g) = self.names.global(v) {
                    return format!("&{g}");
                }
            }
        }
        let s = signed(v, bits);
        if s < 0 && s > -0x10000 {
            format!("-{}", hex(s.unsigned_abs()))
        } else {
            hex(v & mask(bits))
        }
    }

    fn call_text(&self, c: &Call) -> String {
        let args: Vec<String> = c.args.iter().map(|a| self.expr(a, 0)).collect();
        let name = match &c.target {
            Target::Direct(a) => self.names.func(*a).unwrap_or_else(|| format!("sub_{a:x}")),
            Target::Import(i) => self.names.import(*i),
            Target::Indirect(e) => format!("(*{})", self.expr(e, 12)),
        };
        let call = format!("{name}({})", args.join(", "));
        match (&c.target, self.names.call_note(c.addr)) {
            (Target::Indirect(_), Some(n)) => format!("{call} /* {n} */"),
            _ => call,
        }
    }

    fn mem_text(&self, addr: &Expr, bits: Bits, ty: &str) -> String {
        let (base, off) = match addr {
            Expr::Bin(Bin::Add, b, k) if k.as_const().is_some() => (&**b, k.as_const().unwrap_or(0)),
            other => (other, 0),
        };
        if let Expr::Var(Var::Web(w), _) = base {
            if let Some(key) = self.web.names.get(w) {
                if let Some(fld) = self.names.field(key, off) {
                    let v = self.var_text(Var::Web(*w), 64);
                    return format!("{v}->{fld}");
                }
            }
        }
        if let Expr::Var(Var::Stack(o), _) = base {
            let key = self.slot_key(i64::from(*o));
            if let Some(fld) = self.names.field(&key, off) {
                return format!("{}->{fld}", self.slot_name(i64::from(*o)));
            }
        }
        match addr {
            Expr::StackAddr(o) => self.slot_name(*o),
            Expr::Const(a, _) => match self.names.global(*a) {
                Some(g) => g,
                None => format!("*({ty}*){}", hex(*a)),
            },
            // base + index * scale
            Expr::Bin(Bin::Add, b, i) => {
                if let Expr::Bin(Bin::Mul, x, s) = &**i {
                    if let Some(k) = s.as_const() {
                        if u64::from(bits) / 8 == k {
                            return format!("(({ty}*){})[{}]", self.expr(b, 12), self.expr(x, 0));
                        }
                    }
                }
                format!("*({ty}*)({})", self.expr(addr, 0))
            }
            _ => format!("*({ty}*){}", self.expr(addr, 11)),
        }
    }

    pub fn expr(&self, e: &Expr, min: u8) -> String {
        let paren = |p: u8, s: String| if p < min { format!("({s})") } else { s };
        match e {
            Expr::Const(v, b) => self.konst(*v, *b),
            Expr::Var(v, b) => {
                let t = self.var_text(*v, *b);
                if let Var::Web(w) = v {
                    if let Some(&wb) = self.web.bits.get(w) {
                        if *b < wb && *b > 1 {
                            return format!("({}){t}", utype(*b));
                        }
                    }
                }
                t
            }
            Expr::Unknown(_) => "undefined".into(),
            Expr::StackAddr(o) => format!("&{}", self.slot_name(*o)),
            Expr::SegBase(s) => (if *s == 0 { "fs_base" } else { "gs_base" }).into(),
            Expr::Load(a, b) => self.mem_text(a, *b, utype(*b)),
            Expr::Un(op, x) => {
                let c = match op {
                    Un::Neg => "-",
                    Un::Not => "~",
                    Un::LNot => "!",
                    Un::FNeg => return paren(11, format!("-{}", self.fexpr(x, 12))),
                };
                paren(11, format!("{c}{}", self.expr(x, 11)))
            }
            Expr::Bin(op, a, c) => {
                // `x + -n` reads better as `x - n`
                if let (Bin::Add, Some(k)) = (op, c.as_const()) {
                    let s = signed(k, c.bits());
                    if s < 0 && s > -0x10000 {
                        let p = prec(Bin::Sub);
                        return paren(p, format!("{} - {}", self.expr(a, p), hex(s.unsigned_abs())));
                    }
                }
                let p = prec(*op);
                if matches!(op, Bin::FAdd | Bin::FSub | Bin::FMul | Bin::FDiv) {
                    return paren(p, format!("{} {} {}", self.fexpr(a, p), op_text(*op), self.fexpr(c, p + 1)));
                }
                paren(p, format!("{} {} {}", self.expr(a, p), op_text(*op), self.expr(c, p + 1)))
            }
            Expr::Trunc(x, b) => {
                if let Expr::Var(..) = &**x {
                    format!("({}){}", utype(*b), self.expr(x, 11))
                } else {
                    format!("({})({})", utype(*b), self.expr(x, 0))
                }
            }
            Expr::Zext(x, b) => match &**x {
                Expr::Var(..) | Expr::Load(..) | Expr::Call(_) | Expr::Const(..) | Expr::Cmp(..) => self.expr(x, min),
                _ => format!("({})({})", utype(*b), self.expr(x, 0)),
            },
            Expr::Sext(x, b) => match &**x {
                Expr::Var(..) | Expr::Load(..) | Expr::Call(_) | Expr::Const(..) => self.expr(x, min),
                _ => format!("({})({})", itype(*b), self.expr(x, 0)),
            },
            Expr::Insert(a, c) => format!("INSERT({}, {})", self.expr(a, 0), self.expr(c, 0)),
            Expr::Cmp(op, a, c) if op.is_float() => {
                let (ta, tc) = (self.fexpr(a, 8), self.fexpr(c, 8));
                let call = |name: &str| format!("{name}({}, {})", self.fexpr(a, 0), self.fexpr(c, 0));
                match op {
                    Cmp::FOeq | Cmp::FUne => paren(6, format!("{ta} {} {tc}", op.text())),
                    Cmp::FOlt | Cmp::FOle | Cmp::FOgt | Cmp::FOge => paren(7, format!("{ta} {} {tc}", op.text())),
                    Cmp::FUlt => paren(11, format!("!({ta} >= {tc})")),
                    Cmp::FUle => paren(11, format!("!({ta} > {tc})")),
                    Cmp::FUge => paren(11, format!("!({ta} < {tc})")),
                    Cmp::FUgt => paren(11, format!("!({ta} <= {tc})")),
                    Cmp::FOne => call("islessgreater"),
                    Cmp::FUeq => paren(11, format!("!{}", call("islessgreater"))),
                    Cmp::FUno => call("isunordered"),
                    Cmp::FOrd => paren(11, format!("!{}", call("isunordered"))),
                    _ => paren(7, format!("{ta} {} {tc}", op.text())),
                }
            }
            Expr::Cmp(op, a, c) => {
                let (ta, tc) = if op.is_unsigned() {
                    let w = a.bits().max(c.bits());
                    let cast = |x: &Expr| {
                        if x.as_const().is_some() {
                            self.expr(x, 8)
                        } else {
                            format!("({}){}", utype(w), self.expr(x, 11))
                        }
                    };
                    (cast(a), cast(c))
                } else if op.is_signed() {
                    (self.signed_operand(a), self.signed_operand(c))
                } else {
                    (self.expr(a, 8), self.expr(c, 8))
                };
                let q = if matches!(op, Cmp::Eq | Cmp::Ne) { 6 } else { 7 };
                paren(q, format!("{ta} {} {tc}", op.text()))
            }
            Expr::Flags(k, a, c) => format!("flags_{k:?}({}, {})", self.expr(a, 0), self.expr(c, 0)).to_lowercase(),
            Expr::Cond(cc, a, c) => format!("cond_{}({}, {})", cc.name(), self.expr(a, 0), self.expr(c, 0)),
            Expr::Select(c, a, b) => {
                if e.is_float_op() {
                    return paren(0, format!("{} ? {} : {}", self.expr(c, 1), self.fexpr(a, 1), self.fexpr(b, 1)));
                }
                paren(0, format!("{} ? {} : {}", self.expr(c, 1), self.expr(a, 1), self.expr(b, 1)))
            }
            Expr::Call(c) => self.call_text(c),
            Expr::Intr(n, args, bits) => match (*n, args.as_slice()) {
                ("(double)" | "(float)", [x]) => paren(11, format!("{n}{}", self.expr(x, 12))),
                ("fext", [x]) => paren(11, format!("(double){}", self.fexpr(x, 12))),
                ("ftrunc", [x]) => paren(11, format!("(float){}", self.fexpr(x, 12))),
                ("ftoi", [x]) => paren(11, format!("({}){}", itype(*bits), self.fexpr(x, 12))),
                ("lrint", [x]) if *bits == 32 => paren(11, format!("(i32)lrint({})", self.fexpr(x, 0))),
                ("lrint" | "sqrt" | "sqrtf" | "fabs" | "fabsf", [x]) => format!("{n}({})", self.fexpr(x, 0)),
                _ => {
                    let a: Vec<String> = args.iter().map(|x| self.expr(x, 0)).collect();
                    format!("{n}({})", a.join(", "))
                }
            },
        }
    }

    /// The width of `e` when it holds a float: a float web or a float operation.
    fn float_width(&self, e: &Expr) -> Option<Bits> {
        match e {
            Expr::Var(Var::Web(w), _) => self.web.fwidth.get(w).copied(),
            Expr::Intr("as_float", ..) => Some(e.bits()),
            _ if e.is_float_op() => Some(e.bits()),
            _ => None,
        }
    }

    /// The text of `e` where a float is expected: constants as floats, loads as `double` or `float` loads.
    fn fexpr(&self, e: &Expr, min: u8) -> String {
        match e {
            Expr::Const(v, 64) => {
                let s = fmt_f64(*v);
                if s.starts_with('-') && min >= 11 {
                    format!("({s})")
                } else {
                    s
                }
            }
            Expr::Const(v, 32) => {
                let s = fmt_f32(*v as u32);
                if s.starts_with('-') && min >= 11 {
                    format!("({s})")
                } else {
                    s
                }
            }
            Expr::Load(a, b @ (32 | 64)) => self.mem_text(a, *b, if *b == 32 { "float" } else { "double" }),
            _ => self.expr(e, min),
        }
    }

    /// A signed predicate decides the sign of its operands: a narrowed operand is cast to `iN`, never `uN`.
    fn signed_operand(&self, x: &Expr) -> String {
        match x {
            Expr::Trunc(inner, b) => match &**inner {
                Expr::Var(..) => format!("({}){}", itype(*b), self.expr(inner, 11)),
                _ => format!("({})({})", itype(*b), self.expr(inner, 0)),
            },
            Expr::Var(Var::Web(w), b) if *b > 1 && self.web.bits.get(w).is_some_and(|&wb| *b < wb) => {
                format!("({}){}", itype(*b), self.var_text(Var::Web(*w), *b))
            }
            _ => self.expr(x, 8),
        }
    }

    fn push(&mut self, addr: Option<u64>, text: String) {
        self.lines.push(Line { addr, indent: self.indent, text });
    }

    fn assign_text(&self, lhs: String, rhs: &Expr, for_step: bool, fl: bool) -> String {
        if let Expr::Bin(op, a, c) = rhs {
            if let Expr::Var(v, _) = &**a {
                if self.var_text(*v, 0) == lhs
                    && matches!(
                        op,
                        Bin::Add | Bin::Sub | Bin::Mul | Bin::And | Bin::Or | Bin::Xor | Bin::Shl | Bin::Shr | Bin::Sar
                    )
                {
                    if matches!(op, Bin::Add | Bin::Sub) && c.as_const() == Some(1) {
                        return format!("{lhs}{}", if *op == Bin::Add { "++" } else { "--" });
                    }
                    let _ = for_step;
                    return format!("{lhs} {}= {}", op_text(*op), self.expr(c, 1));
                }
            }
        }
        if fl {
            return format!("{lhs} = {}", self.fexpr(rhs, 0));
        }
        format!("{lhs} = {}", self.expr(rhs, 0))
    }

    fn stmt_text(&self, st: &Stmt, for_step: bool) -> Option<String> {
        Some(match st {
            Stmt::Assign(v, e) => {
                // an assignment of an undefined value says nothing
                if matches!(e, Expr::Unknown(_)) {
                    return None;
                }
                self.assign_text(self.var_text(*v, 0), e, for_step, matches!(v, Var::Web(w) if self.web.fwidth.contains_key(w)))
            }
            Stmt::Store(a, b, v) => {
                let fl = self.float_width(v).is_some();
                let ty = if !fl { utype(*b) } else if *b == 32 { "float" } else { "double" };
                let lhs = self.mem_text(a, *b, ty);
                self.assign_text(lhs, v, for_step, fl)
            }
            Stmt::Eval(e) => self.expr(e, 0),
            Stmt::Clobber(_) => return None,
            Stmt::Raw { text, .. } => format!("/* not lifted: {text} */"),
        })
    }

    fn label(&self, b: usize) -> String {
        format!("loc_{:x}", self.block_addr.get(b).copied().unwrap_or(0))
    }

    pub fn nodes(&mut self, v: &[Node]) {
        for n in v {
            self.node(n);
        }
    }

    fn block(&mut self, v: &[Node]) {
        self.indent += 1;
        self.nodes(v);
        self.indent -= 1;
    }

    fn node(&mut self, n: &Node) {
        match n {
            Node::Stmt(a, st) => {
                if let Some(t) = self.stmt_text(st, false) {
                    self.push(Some(*a), format!("{t};"));
                }
            }
            Node::If(a, c, t, e) => {
                self.push(Some(*a), format!("if ({}) {{", self.expr(c, 0)));
                self.block(t);
                let mut e = e.as_slice();
                // else-if chains
                loop {
                    match e {
                        [] => {
                            self.push(None, "}".into());
                            break;
                        }
                        [Node::If(a2, c2, t2, e2)] => {
                            self.push(Some(*a2), format!("}} else if ({}) {{", self.expr(c2, 0)));
                            self.block(t2);
                            e = e2.as_slice();
                        }
                        _ => {
                            self.push(None, "} else {".into());
                            self.block(e);
                            self.push(None, "}".into());
                            break;
                        }
                    }
                }
            }
            Node::Loop(b) => {
                self.push(None, "while (1) {".into());
                self.block(b);
                self.push(None, "}".into());
            }
            Node::While(c, b) => {
                self.push(None, format!("while ({}) {{", self.expr(c, 0)));
                self.block(b);
                self.push(None, "}".into());
            }
            Node::DoWhile(b, c) => {
                self.push(None, "do {".into());
                self.block(b);
                self.push(None, format!("}} while ({});", self.expr(c, 0)));
            }
            Node::For(i, c, s, b) => {
                let init = if let Node::Stmt(_, st) = &**i {
                    self.stmt_text(st, false).unwrap_or_default()
                } else {
                    String::new()
                };
                let step = if let Node::Stmt(_, st) = &**s {
                    self.stmt_text(st, true).unwrap_or_default()
                } else {
                    String::new()
                };
                let a = if let Node::Stmt(a, _) = &**i { Some(*a) } else { None };
                self.push(a, format!("for ({init}; {}; {step}) {{", self.expr(c, 0)));
                self.block(b);
                self.push(None, "}".into());
            }
            Node::Switch(a, v, arms, d) => {
                self.push(Some(*a), format!("switch ({}) {{", self.expr(v, 0)));
                self.indent += 1;
                for (vals, body) in arms {
                    let labels: Vec<String> = vals.iter().map(|x| format!("case {}:", hex(*x))).collect();
                    self.push(None, labels.join(" "));
                    self.block(body);
                    if !super::structure::ends_terminal(body) {
                        self.indent += 1;
                        self.push(None, "break;".into());
                        self.indent -= 1;
                    }
                }
                if let Some(d) = d {
                    self.push(None, "default:".into());
                    self.block(d);
                }
                self.indent -= 1;
                self.push(None, "}".into());
            }
            Node::Break => self.push(None, "break;".into()),
            Node::Continue => self.push(None, "continue;".into()),
            Node::Goto(b) => {
                let l = self.label(*b);
                self.push(None, format!("goto {l};"))
            }
            Node::Label(b) => {
                let l = self.label(*b);
                let i = self.indent;
                self.indent = 0;
                self.push(None, format!("{l}:"));
                self.indent = i;
            }
            Node::Return(a, e) => match e {
                Some(e) if self.fret => self.push(Some(*a), format!("return {};", self.fexpr(e, 0))),
                Some(e) => self.push(Some(*a), format!("return {};", self.expr(e, 0))),
                None => self.push(Some(*a), "return;".into()),
            },
            Node::TailCall(a, e) => {
                self.push(Some(*a), format!("return {};", self.expr(e, 0)));
            }
            Node::Indirect(a, e) => {
                self.push(Some(*a), format!("goto *{}; /* jump not resolved */", self.expr(e, 11)));
            }
            Node::Stop(a) => self.push(Some(*a), "/* does not return */".into()),
        }
    }
}

/// The whole function: signature, locals, body.
pub fn render(
    f: &Lifted,
    facts: &Facts,
    web: &WebInfo,
    body: &[Node],
    names: &dyn Names,
    name: &str,
    proto: Option<&super::types::Proto>,
) -> Vec<Line> {
    let mut pr = Printer {
        fret: matches!(facts.ret, Some(Var::Xmm(_))),
        names,
        web,
        abi_stack_arg: f.abi.stack_arg_base(),
        a64: f.abi == super::lift::Abi::Aapcs64,
        block_addr: f.blocks.iter().map(|b| b.addr).collect(),
        lines: Vec::new(),
        indent: 0,
        slots: BTreeMap::new(),
    };
    let ty = |w: u32| -> String {
        let b = web.bits.get(&w).copied().unwrap_or(64);
        if let Some(fw) = web.fwidth.get(&w) {
            return if *fw == 32 { "float" } else { "double" }.into();
        }
        if web.signed.get(&w).copied().unwrap_or(true) { itype(b) } else { utype(b) }.to_string()
    };
    let ret = match facts.ret {
        _ if proto.is_some() => proto.map(|p| p.ret.clone()).unwrap_or_default(),
        None => "void".to_string(),
        Some(Var::Xmm(_)) => {
            fn f32_ret(v: &[Node], web: &WebInfo, narrow: &mut bool) {
                for n in v {
                    match n {
                        Node::Return(_, Some(e)) => {
                            let w = match e {
                                Expr::Var(Var::Web(w), _) => web.fwidth.get(w).copied().unwrap_or(64),
                                other => other.bits(),
                            };
                            *narrow |= w == 32;
                        }
                        Node::If(_, _, x, y) => {
                            f32_ret(x, web, narrow);
                            f32_ret(y, web, narrow);
                        }
                        Node::Loop(x) | Node::While(_, x) | Node::DoWhile(x, _) | Node::For(_, _, _, x) => {
                            f32_ret(x, web, narrow)
                        }
                        Node::Switch(_, _, arms, d) => {
                            arms.iter().for_each(|a| f32_ret(&a.1, web, narrow));
                            if let Some(d) = d {
                                f32_ret(d, web, narrow);
                            }
                        }
                        _ => {}
                    }
                }
            }
            let mut narrow = false;
            f32_ret(body, web, &mut narrow);
            if narrow { "float" } else { "double" }.to_string()
        }
        Some(_) => {
            // the width the returned expression has
            let mut b = 0;
            fn ret_bits(v: &[Node], b: &mut Bits) {
                for n in v {
                    match n {
                        Node::Return(_, Some(e)) => {
                            let inner = match e {
                                Expr::Zext(x, _) | Expr::Sext(x, _) => x.bits(),
                                other => other.bits(),
                            };
                            *b = (*b).max(inner);
                        }
                        Node::If(_, _, x, y) => {
                            ret_bits(x, b);
                            ret_bits(y, b);
                        }
                        Node::Loop(x) | Node::While(_, x) | Node::DoWhile(x, _) | Node::For(_, _, _, x) => {
                            ret_bits(x, b)
                        }
                        Node::Switch(_, _, arms, d) => {
                            arms.iter().for_each(|a| ret_bits(&a.1, b));
                            if let Some(d) = d {
                                ret_bits(d, b);
                            }
                        }
                        _ => {}
                    }
                }
            }
            ret_bits(body, &mut b);
            if b == 0 {
                "i64".into()
            } else {
                itype(b.max(8)).to_string()
            }
        }
    };
    let args: Vec<String> = web
        .args
        .iter()
        .map(|(n, w)| {
            let by_name = proto.and_then(|p| {
                let i = n[1..].parse::<usize>().ok()?;
                if n.starts_with('f') {
                    // Win64 numbers floats by slot; SysV counts the float-typed arguments
                    if f.abi == super::lift::Abi::Win64 {
                        p.args.get(i.checked_sub(1)?)
                    } else {
                        p.args.iter().filter(|(t, _)| t == "float" || t == "double").nth(i.checked_sub(1)?)
                    }
                } else {
                    p.args.get(i.wrapping_sub(1))
                }
            });
            let t = match by_name {
                Some((t, _)) => t.clone(),
                None if *w == 0 => "i64".to_string(),
                None => ty(*w),
            };
            let n = names.var_name(n).unwrap_or_else(|| n.clone());
            if t.ends_with('*') {
                format!("{t}{n}")
            } else {
                format!("{t} {n}")
            }
        })
        .collect();
    let sig = format!("{ret} {name}({})", if args.is_empty() { "void".to_string() } else { args.join(", ") });
    pr.push(Some(f.start), sig);
    pr.push(None, "{".into());
    pr.indent = 1;
    // locals
    let mut ids: Vec<(&u32, &String)> = web.names.iter().filter(|(_, n)| n.starts_with('v')).collect();
    ids.sort_by_key(|(w, _)| **w);
    let mut declared: Vec<(String, String)> = ids.iter().map(|(w, n)| (ty(**w), (*n).clone())).collect();
    declared.sort_by_key(|d| d.1[1..].parse::<u32>().unwrap_or(0));
    for (t, n) in &declared {
        let n2 = names.var_name(n).unwrap_or_else(|| n.clone());
        pr.push(None, format!("{t} {n2};"));
    }
    if !declared.is_empty() {
        pr.push(None, String::new());
    }
    let mut body = body.to_vec();
    // a `return;` at the very end of a void function says nothing
    if facts.ret.is_none() {
        if let Some(Node::Return(_, None)) = body.last() {
            body.pop();
        }
    }
    pr.nodes(&body);
    pr.indent = 0;
    pr.push(None, "}".into());
    let _ = ENTRY;
    pr.lines
}

/// A double as C text: exact, and `INFINITY` and `NAN` for those; any other NaN keeps its bits.
fn fmt_f64(bits: u64) -> String {
    let v = f64::from_bits(bits);
    if v.is_nan() {
        return match bits {
            0x7ff8_0000_0000_0000 => "NAN".into(),
            0xfff8_0000_0000_0000 => "-NAN".into(),
            _ => format!("as_float(0x{bits:x})"),
        };
    }
    if v.is_infinite() {
        return if v > 0.0 { "INFINITY" } else { "-INFINITY" }.into();
    }
    // the shortest text that reads back to the same bits; it is valid C (`1.0`, `1e300`, `-0.0`)
    format!("{v:?}")
}

/// The same for a float, with the `f` suffix on digits.
fn fmt_f32(bits: u32) -> String {
    let v = f32::from_bits(bits);
    if v.is_nan() {
        return match bits {
            0x7fc0_0000 => "NAN".into(),
            0xffc0_0000 => "-NAN".into(),
            _ => format!("as_float(0x{bits:x})"),
        };
    }
    if v.is_infinite() {
        return if v > 0.0 { "INFINITY" } else { "-INFINITY" }.into();
    }
    format!("{v:?}f")
}
