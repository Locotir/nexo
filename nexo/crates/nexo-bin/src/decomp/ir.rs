//! The small language a function is lifted into: variables, expressions that know their width,
//! statements that remember the instruction they came from, and the rules that make an expression
//! smaller without changing its value.

/// Width in bits: 1 (a condition), 8, 16, 32, 64 or 128. 0 is a flags value, never printed alone.
pub type Bits = u8;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Var {
    /// A general register by number (0 rax, 1 rcx, 2 rdx, 3 rbx, 4 rsp, 5 rbp, 6 rsi, 7 rdi, 8..15).
    Reg(u8),
    Xmm(u8),
    /// A stack slot nothing takes the address of: offset from the stack pointer at entry.
    Stack(i32),
    /// The operands of the last instruction that set the flags.
    FlagOps,
    /// The result of the last instruction that set the flags.
    FlagRes,
    /// Scratch of one instruction (`div`, `xchg`).
    Tmp(u8),
    /// One live range of a register, after renaming (see render).
    Web(u32),
}

pub const RAX: Var = Var::Reg(0);
pub const RCX: Var = Var::Reg(1);
pub const RDX: Var = Var::Reg(2);
pub const RSP: Var = Var::Reg(4);
pub const RSI: Var = Var::Reg(6);
pub const RDI: Var = Var::Reg(7);

pub const REG_NAMES: [&str; 16] =
    ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Un {
    Neg,
    Not,
    /// Logical not of a condition.
    LNot,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Bin {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
    UDiv,
    SDiv,
    UMod,
    SMod,
    LAnd,
    LOr,
    FAdd,
    FSub,
    FMul,
    FDiv,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Ult,
    Ule,
    Ugt,
    Uge,
    Flt,
    Fle,
    Fgt,
    Fge,
    Feq,
    Fne,
}

impl Cmp {
    pub fn negate(self) -> Cmp {
        use Cmp::*;
        match self {
            Eq => Ne,
            Ne => Eq,
            Lt => Ge,
            Ge => Lt,
            Le => Gt,
            Gt => Le,
            Ult => Uge,
            Uge => Ult,
            Ule => Ugt,
            Ugt => Ule,
            // A float compare is false when unordered, so these are only printed, never negated twice.
            Flt => Fge,
            Fge => Flt,
            Fle => Fgt,
            Fgt => Fle,
            Feq => Fne,
            Fne => Feq,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, Cmp::Flt | Cmp::Fle | Cmp::Fgt | Cmp::Fge | Cmp::Feq | Cmp::Fne)
    }

    pub fn is_signed(self) -> bool {
        matches!(self, Cmp::Lt | Cmp::Le | Cmp::Gt | Cmp::Ge)
    }

    pub fn is_unsigned(self) -> bool {
        matches!(self, Cmp::Ult | Cmp::Ule | Cmp::Ugt | Cmp::Uge)
    }

    pub fn text(self) -> &'static str {
        use Cmp::*;
        match self {
            Eq | Feq => "==",
            Ne | Fne => "!=",
            Lt | Ult | Flt => "<",
            Le | Ule | Fle => "<=",
            Gt | Ugt | Fgt => ">",
            Ge | Uge | Fge => ">=",
        }
    }
}

/// An x86 condition code, in the order of the instruction encoding (`cc ^ 1` is the opposite).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Cc(pub u8);

impl Cc {
    pub const O: Cc = Cc(0);
    pub const NO: Cc = Cc(1);
    pub const B: Cc = Cc(2);
    pub const AE: Cc = Cc(3);
    pub const E: Cc = Cc(4);
    pub const NE: Cc = Cc(5);
    pub const BE: Cc = Cc(6);
    pub const A: Cc = Cc(7);
    pub const S: Cc = Cc(8);
    pub const NS: Cc = Cc(9);
    pub const P: Cc = Cc(10);
    pub const NP: Cc = Cc(11);
    pub const L: Cc = Cc(12);
    pub const GE: Cc = Cc(13);
    pub const LE: Cc = Cc(14);
    pub const G: Cc = Cc(15);

    pub fn negate(self) -> Cc {
        Cc(self.0 ^ 1)
    }

    pub fn name(self) -> &'static str {
        ["o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g"][self.0 as usize & 15]
    }
}

/// What kind of instruction set the flags.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FlagKind {
    /// `cmp a, b` or `sub`: every condition is a compare of the two operands.
    Sub,
    /// `inc`/`dec` seen as `a - b`: the carry flag is not theirs.
    SubNoCf,
    Add,
    /// `test`/`and`/`or`/`xor`: the first operand is the result, carry and overflow are clear.
    Logic,
    /// `ucomisd a, b` and friends.
    FCmp,
    /// `bt a, b`: the carry flag is bit `b` of `a`.
    Bt,
    /// The result of an arithmetic instruction: only zero and sign are known.
    Res,
    /// The result of a logic instruction: carry and overflow are clear too.
    ResLogic,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Target {
    Direct(u64),
    /// Index into the image's imports.
    Import(usize),
    Indirect(Expr),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Call {
    pub target: Target,
    pub args: Vec<Expr>,
    /// The call instruction.
    pub addr: u64,
    /// False while the number of arguments is a guess from the registers that were set.
    pub known: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr {
    Const(u64, Bits),
    Var(Var, Bits),
    /// A value nothing in the function defines (a register after a call that does not keep it).
    Unknown(Bits),
    /// The stack pointer at entry plus an offset: the address of a local.
    StackAddr(i64),
    /// The base of `fs` (0) or `gs` (1).
    SegBase(u8),
    Load(Box<Expr>, Bits),
    Un(Un, Box<Expr>),
    Bin(Bin, Box<Expr>, Box<Expr>),
    Trunc(Box<Expr>, Bits),
    Zext(Box<Expr>, Bits),
    Sext(Box<Expr>, Bits),
    /// The first value with its low bits replaced by the second (a write to `al` or `ax`).
    Insert(Box<Expr>, Box<Expr>),
    Cmp(Cmp, Box<Expr>, Box<Expr>),
    Flags(FlagKind, Box<Expr>, Box<Expr>),
    /// A condition code over (operands, result) that no rule has turned into a compare yet.
    Cond(Cc, Box<Expr>, Box<Expr>),
    Select(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Call>),
    /// Anything else, by name (`rotl`, `bswap`, `(double)`).
    Intr(&'static str, Vec<Expr>, Bits),
}

pub fn mask(bits: Bits) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// The value of `v` (of `bits` bits) as a signed number.
pub fn signed(v: u64, bits: Bits) -> i64 {
    if bits == 0 || bits >= 64 {
        return v as i64;
    }
    let sh = 64 - bits as u32;
    ((v << sh) as i64) >> sh
}

pub fn konst(v: u64, bits: Bits) -> Expr {
    Expr::Const(v & mask(bits), bits)
}

fn b(e: Expr) -> Box<Expr> {
    Box::new(e)
}

impl Expr {
    pub fn bits(&self) -> Bits {
        use Expr::*;
        match self {
            Const(_, w)
            | Var(_, w)
            | Unknown(w)
            | Load(_, w)
            | Trunc(_, w)
            | Zext(_, w)
            | Sext(_, w)
            | Intr(_, _, w) => *w,
            StackAddr(_) | SegBase(_) | Call(_) => 64,
            Un(self::Un::LNot, _) | Cmp(..) | Cond(..) => 1,
            Un(_, e) => e.bits(),
            Bin(self::Bin::LAnd | self::Bin::LOr, ..) => 1,
            Bin(_, a, _) => a.bits(),
            Insert(a, _) => a.bits(),
            Flags(..) => 0,
            Select(_, a, _) => a.bits(),
        }
    }

    pub fn reg(n: u8) -> Expr {
        Expr::Var(Var::Reg(n), 64)
    }

    pub fn bin(op: Bin, a: Expr, c: Expr) -> Expr {
        Expr::Bin(op, b(a), b(c))
    }

    pub fn add(a: Expr, c: Expr) -> Expr {
        Expr::bin(Bin::Add, a, c)
    }

    pub fn trunc(self, bits: Bits) -> Expr {
        if self.bits() == bits {
            self
        } else {
            Expr::Trunc(b(self), bits)
        }
    }

    pub fn zext(self, bits: Bits) -> Expr {
        if self.bits() >= bits {
            self
        } else {
            Expr::Zext(b(self), bits)
        }
    }

    pub fn sext(self, bits: Bits) -> Expr {
        if self.bits() >= bits {
            self
        } else {
            Expr::Sext(b(self), bits)
        }
    }

    pub fn load(self, bits: Bits) -> Expr {
        Expr::Load(b(self), bits)
    }

    pub fn not(self) -> Expr {
        simplify(Expr::Un(Un::LNot, b(self)))
    }

    pub fn as_const(&self) -> Option<u64> {
        match self {
            Expr::Const(v, _) => Some(*v),
            _ => None,
        }
    }

    /// Call `f` on this expression and every one inside it.
    pub fn visit(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        self.children(|c| c.visit(f));
    }

    fn children(&self, mut f: impl FnMut(&Expr)) {
        use Expr::*;
        match self {
            Const(..) | Var(..) | Unknown(_) | StackAddr(_) | SegBase(_) => {}
            Load(a, _) | Un(_, a) | Trunc(a, _) | Zext(a, _) | Sext(a, _) => f(a),
            Bin(_, a, c) | Insert(a, c) | Cmp(_, a, c) | Flags(_, a, c) | Cond(_, a, c) => {
                f(a);
                f(c);
            }
            Select(a, c, d) => {
                f(a);
                f(c);
                f(d);
            }
            Call(c) => {
                if let Target::Indirect(t) = &c.target {
                    f(t);
                }
                c.args.iter().for_each(f);
            }
            Intr(_, args, _) => args.iter().for_each(f),
        }
    }

    /// The same for changing: children first, then the node.
    pub fn rewrite(&mut self, f: &mut impl FnMut(&mut Expr)) {
        use Expr::*;
        match self {
            Const(..) | Var(..) | Unknown(_) | StackAddr(_) | SegBase(_) => {}
            Load(a, _) | Un(_, a) | Trunc(a, _) | Zext(a, _) | Sext(a, _) => a.rewrite(f),
            Bin(_, a, c) | Insert(a, c) | Cmp(_, a, c) | Flags(_, a, c) | Cond(_, a, c) => {
                a.rewrite(f);
                c.rewrite(f);
            }
            Select(a, c, d) => {
                a.rewrite(f);
                c.rewrite(f);
                d.rewrite(f);
            }
            Call(c) => {
                if let Target::Indirect(t) = &mut c.target {
                    t.rewrite(f);
                }
                c.args.iter_mut().for_each(|a| a.rewrite(f));
            }
            Intr(_, args, _) => args.iter_mut().for_each(|a| a.rewrite(f)),
        }
        f(self);
    }

    pub fn vars(&self, f: &mut impl FnMut(Var)) {
        self.visit(&mut |e| {
            if let Expr::Var(v, _) = e {
                f(*v)
            }
        });
    }

    pub fn uses(&self, v: Var) -> usize {
        let mut n = 0;
        self.vars(&mut |x| n += usize::from(x == v));
        n
    }

    pub fn has(&self, pred: &impl Fn(&Expr) -> bool) -> bool {
        let mut found = false;
        self.visit(&mut |e| found |= pred(e));
        found
    }

    pub fn has_call(&self) -> bool {
        self.has(&|e| matches!(e, Expr::Call(_)))
    }

    pub fn has_load(&self) -> bool {
        self.has(&|e| matches!(e, Expr::Load(..)))
    }

    pub fn size(&self) -> usize {
        let mut n = 0;
        self.visit(&mut |_| n += 1);
        n
    }

    /// Put `with` where `v` is read. Returns how many places changed.
    pub fn subst(&mut self, v: Var, with: &Expr) -> usize {
        let mut n = 0;
        self.rewrite(&mut |e| {
            if let Expr::Var(x, w) = e {
                if *x == v {
                    let w = *w;
                    *e = if with.bits() == w || with.bits() == 0 { with.clone() } else { with.clone().trunc(w) };
                    n += 1;
                }
            }
        });
        if n > 0 {
            let old = std::mem::replace(self, Expr::Unknown(0));
            *self = simplify(old);
        }
        n
    }
}

/// One statement and the instruction it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct S {
    pub addr: u64,
    pub st: Stmt,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Assign(Var, Expr),
    /// `*(type*)addr = value`
    Store(Expr, Bits, Expr),
    /// A call whose value nobody reads.
    Eval(Expr),
    /// The values these hold are gone (a call does not keep them).
    Clobber(Vec<Var>),
    /// An instruction this version does not lift, with the variables it reads and writes.
    Raw {
        text: String,
        uses: Vec<Var>,
        defs: Vec<Var>,
        mem: bool,
    },
}

impl Stmt {
    /// The expressions this statement reads.
    pub fn exprs(&self) -> Vec<&Expr> {
        match self {
            Stmt::Assign(_, e) | Stmt::Eval(e) => vec![e],
            Stmt::Store(a, _, v) => vec![a, v],
            Stmt::Clobber(_) | Stmt::Raw { .. } => vec![],
        }
    }

    pub fn exprs_mut(&mut self) -> Vec<&mut Expr> {
        match self {
            Stmt::Assign(_, e) | Stmt::Eval(e) => vec![e],
            Stmt::Store(a, _, v) => vec![a, v],
            Stmt::Clobber(_) | Stmt::Raw { .. } => vec![],
        }
    }

    pub fn defs(&self, f: &mut impl FnMut(Var)) {
        match self {
            Stmt::Assign(v, _) => f(*v),
            Stmt::Clobber(vs) => vs.iter().copied().for_each(f),
            Stmt::Raw { defs, .. } => defs.iter().copied().for_each(f),
            Stmt::Store(..) | Stmt::Eval(_) => {}
        }
    }

    pub fn uses(&self, f: &mut impl FnMut(Var)) {
        if let Stmt::Raw { uses, .. } = self {
            uses.iter().copied().for_each(&mut *f);
        }
        for e in self.exprs() {
            e.vars(f);
        }
    }

    /// True when the statement may change memory that is not a promoted slot.
    pub fn writes_mem(&self) -> bool {
        match self {
            Stmt::Store(..) => true,
            Stmt::Assign(_, e) | Stmt::Eval(e) => e.has_call(),
            Stmt::Raw { mem, .. } => *mem,
            Stmt::Clobber(_) => false,
        }
    }
}

pub type BlockId = usize;

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    Jump(BlockId),
    /// `cond` true goes to the first block.
    Branch(Expr, BlockId, BlockId),
    /// The value, the (case value, block) pairs and the default block.
    Switch(Expr, Vec<(u64, BlockId)>, Option<BlockId>),
    Return(Option<Expr>),
    /// `return f(args)`: a jump to another function.
    TailCall(Expr),
    /// A jump through a value that was not resolved to a table.
    Indirect(Expr),
    /// The last statement does not come back (a call to `exit`), or the bytes end.
    Stop,
}

impl Term {
    pub fn succs(&self) -> Vec<BlockId> {
        match self {
            Term::Jump(t) => vec![*t],
            Term::Branch(_, t, e) => {
                if t == e {
                    vec![*t]
                } else {
                    vec![*t, *e]
                }
            }
            Term::Switch(_, cases, def) => {
                let mut v: Vec<BlockId> = Vec::new();
                for t in cases.iter().map(|c| c.1).chain(*def) {
                    if !v.contains(&t) {
                        v.push(t);
                    }
                }
                v
            }
            Term::Return(_) | Term::TailCall(_) | Term::Indirect(_) | Term::Stop => vec![],
        }
    }

    pub fn exprs(&self) -> Vec<&Expr> {
        match self {
            Term::Branch(e, ..)
            | Term::Switch(e, ..)
            | Term::TailCall(e)
            | Term::Indirect(e)
            | Term::Return(Some(e)) => {
                vec![e]
            }
            _ => vec![],
        }
    }

    pub fn exprs_mut(&mut self) -> Vec<&mut Expr> {
        match self {
            Term::Branch(e, ..)
            | Term::Switch(e, ..)
            | Term::TailCall(e)
            | Term::Indirect(e)
            | Term::Return(Some(e)) => {
                vec![e]
            }
            _ => vec![],
        }
    }

    pub fn retarget(&mut self, f: impl Fn(BlockId) -> BlockId) {
        match self {
            Term::Jump(t) => *t = f(*t),
            Term::Branch(_, t, e) => {
                *t = f(*t);
                *e = f(*e);
            }
            Term::Switch(_, cases, def) => {
                cases.iter_mut().for_each(|c| c.1 = f(c.1));
                if let Some(d) = def {
                    *d = f(*d);
                }
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    /// Address of the first instruction.
    pub addr: u64,
    pub stmts: Vec<S>,
    pub term: Term,
    /// Address of the instruction that ends the block.
    pub term_addr: u64,
}

fn fold_bin(op: Bin, x: u64, y: u64, w: Bits) -> Option<u64> {
    let (sx, sy) = (signed(x, w), signed(y, w));
    let sh = (y & if w > 32 { 63 } else { 31 }) as u32;
    Some(
        match op {
            Bin::Add => x.wrapping_add(y),
            Bin::Sub => x.wrapping_sub(y),
            Bin::Mul => x.wrapping_mul(y),
            Bin::And => x & y,
            Bin::Or => x | y,
            Bin::Xor => x ^ y,
            Bin::Shl => x << sh,
            Bin::Shr => x >> sh,
            Bin::Sar => (sx >> sh) as u64,
            Bin::UDiv if y != 0 => x / y,
            Bin::UMod if y != 0 => x % y,
            Bin::SDiv if y != 0 => sx.wrapping_div(sy) as u64,
            Bin::SMod if y != 0 => sx.wrapping_rem(sy) as u64,
            _ => return None,
        } & mask(w),
    )
}

fn fold_cmp(op: Cmp, x: u64, y: u64, w: Bits) -> Option<bool> {
    let (sx, sy) = (signed(x, w), signed(y, w));
    Some(match op {
        Cmp::Eq => x == y,
        Cmp::Ne => x != y,
        Cmp::Lt => sx < sy,
        Cmp::Le => sx <= sy,
        Cmp::Gt => sx > sy,
        Cmp::Ge => sx >= sy,
        Cmp::Ult => x < y,
        Cmp::Ule => x <= y,
        Cmp::Ugt => x > y,
        Cmp::Uge => x >= y,
        _ => return None,
    })
}

fn cmp(op: Cmp, a: Expr, c: Expr) -> Expr {
    simplify_node(Expr::Cmp(op, b(a), b(c)))
}

fn zero(w: Bits) -> Expr {
    Expr::Const(0, w)
}

/// Turn a condition code over flags into a compare, when the flags are known.
fn fold_cond(cc: Cc, ops: &Expr, res: &Expr) -> Option<Expr> {
    if let Expr::Flags(kind, x, y) = ops {
        let (x, y) = ((**x).clone(), (**y).clone());
        let w = x.bits();
        return match kind {
            FlagKind::Sub | FlagKind::SubNoCf => {
                let cf = *kind == FlagKind::Sub;
                let op = match cc {
                    Cc::E => Cmp::Eq,
                    Cc::NE => Cmp::Ne,
                    Cc::L => Cmp::Lt,
                    Cc::GE => Cmp::Ge,
                    Cc::LE => Cmp::Le,
                    Cc::G => Cmp::Gt,
                    Cc::B if cf => Cmp::Ult,
                    Cc::AE if cf => Cmp::Uge,
                    Cc::BE if cf => Cmp::Ule,
                    Cc::A if cf => Cmp::Ugt,
                    Cc::S => return Some(cmp(Cmp::Lt, simplify_node(Expr::bin(Bin::Sub, x, y)), zero(w))),
                    Cc::NS => return Some(cmp(Cmp::Ge, simplify_node(Expr::bin(Bin::Sub, x, y)), zero(w))),
                    _ => return None,
                };
                Some(cmp(op, x, y))
            }
            FlagKind::Logic => fold_result(cc, x, true),
            FlagKind::Add => {
                let sum = simplify_node(Expr::bin(Bin::Add, x.clone(), y));
                match cc {
                    Cc::B => Some(cmp(Cmp::Ult, sum, x)),
                    Cc::AE => Some(cmp(Cmp::Uge, sum, x)),
                    _ => fold_result(cc, sum, false),
                }
            }
            FlagKind::FCmp => Some(match cc {
                Cc::A => cmp(Cmp::Fgt, x, y),
                Cc::AE => cmp(Cmp::Fge, x, y),
                Cc::B => cmp(Cmp::Flt, x, y),
                Cc::BE => cmp(Cmp::Fle, x, y),
                Cc::E => cmp(Cmp::Feq, x, y),
                Cc::NE => cmp(Cmp::Fne, x, y),
                Cc::P => Expr::Intr("unordered", vec![x, y], 1),
                Cc::NP => Expr::Un(Un::LNot, b(Expr::Intr("unordered", vec![x, y], 1))),
                _ => return None,
            }),
            FlagKind::Bt => {
                let bit =
                    simplify_node(Expr::bin(Bin::And, simplify_node(Expr::bin(Bin::Shr, x, y)), Expr::Const(1, w)));
                match cc {
                    Cc::B => Some(cmp(Cmp::Ne, bit, zero(w))),
                    Cc::AE => Some(cmp(Cmp::Eq, bit, zero(w))),
                    _ => None,
                }
            }
            FlagKind::Res => fold_result(cc, x, false),
            FlagKind::ResLogic => fold_result(cc, x, true),
        };
    }
    if matches!(ops, Expr::Var(Var::FlagOps, _)) {
        if let Expr::Flags(kind @ (FlagKind::Res | FlagKind::ResLogic), r, _) = res {
            return fold_result(cc, (**r).clone(), *kind == FlagKind::ResLogic);
        }
    }
    None
}

/// Conditions that only need the result `r`; `logic` says carry and overflow are clear.
fn fold_result(cc: Cc, r: Expr, logic: bool) -> Option<Expr> {
    let w = r.bits();
    let op = match cc {
        Cc::E => Cmp::Eq,
        Cc::NE => Cmp::Ne,
        Cc::S => Cmp::Lt,
        Cc::NS => Cmp::Ge,
        Cc::L if logic => Cmp::Lt,
        Cc::GE if logic => Cmp::Ge,
        Cc::LE if logic => Cmp::Le,
        Cc::G if logic => Cmp::Gt,
        Cc::BE if logic => Cmp::Eq,
        Cc::A if logic => Cmp::Ne,
        Cc::B | Cc::O if logic => return Some(Expr::Const(0, 1)),
        Cc::AE | Cc::NO if logic => return Some(Expr::Const(1, 1)),
        _ => return None,
    };
    Some(cmp(op, r, zero(w)))
}

/// Make the whole expression smaller: children first.
pub fn simplify(mut e: Expr) -> Expr {
    e.rewrite(&mut |n| {
        let old = std::mem::replace(n, Expr::Unknown(0));
        *n = simplify_node(old);
    });
    e
}

/// Push a truncation into the expression when the low bits of the result only need the low bits
/// of the operands.
fn trunc_of(e: Expr, w: Bits) -> Expr {
    use Expr::*;
    let from = e.bits();
    if from == w {
        return e;
    }
    if from < w {
        return Zext(b(e), w);
    }
    match e {
        Const(v, _) => konst(v, w),
        Trunc(x, _) => trunc_of(*x, w),
        Zext(x, _) | Sext(x, _) if x.bits() >= w => trunc_of(*x, w),
        Zext(x, _) => Zext(x, w),
        Sext(x, _) => Sext(x, w),
        Insert(_, new) if new.bits() >= w => trunc_of(*new, w),
        Bin(
            op @ (self::Bin::Add | self::Bin::Sub | self::Bin::Mul | self::Bin::And | self::Bin::Or | self::Bin::Xor),
            x,
            y,
        ) if w >= 8 => simplify_node(Bin(op, b(trunc_of(*x, w)), b(trunc_of(*y, w)))),
        Bin(self::Bin::Shl, x, y) if w >= 8 && y.as_const().is_some_and(|c| c < w as u64) => {
            simplify_node(Bin(self::Bin::Shl, b(trunc_of(*x, w)), y))
        }
        Un(op @ (self::Un::Neg | self::Un::Not), x) if w >= 8 => simplify_node(Un(op, b(trunc_of(*x, w)))),
        Select(c, x, y) => Select(c, b(trunc_of(*x, w)), b(trunc_of(*y, w))),
        Load(a, _) if w >= 8 => Load(a, w),
        Unknown(_) => Unknown(w),
        other => Trunc(b(other), w),
    }
}

/// True when the value is known to fit `bits` bits (the rest are zero).
fn fits(e: &Expr, bits: Bits) -> bool {
    match e {
        Expr::Const(v, _) => *v <= mask(bits),
        Expr::Zext(x, _) => x.bits() <= bits,
        Expr::Cmp(..) | Expr::Cond(..) => true,
        _ => e.bits() <= bits,
    }
}

/// One step: the children are already as small as they get.
pub fn simplify_node(e: Expr) -> Expr {
    use Expr::*;
    match e {
        Const(v, w) => konst(v, w),
        Trunc(x, w) => trunc_of(*x, w),
        Zext(x, w) => match *x {
            _ if x.bits() >= w => trunc_of(*x, w),
            Const(v, _) => konst(v, w),
            Zext(y, _) => Zext(y, w),
            other => Zext(b(other), w),
        },
        Sext(x, w) => match *x {
            _ if x.bits() >= w => trunc_of(*x, w),
            Const(v, from) => konst(signed(v, from) as u64, w),
            Sext(y, _) => Sext(y, w),
            Zext(y, _) => Zext(y, w),
            other => Sext(b(other), w),
        },
        Insert(old, new) => {
            let w = old.bits();
            if new.bits() >= w {
                return trunc_of(*new, w);
            }
            match *old {
                ref o if fits(o, new.bits()) => simplify_node(Zext(new, w)),
                Insert(o, n) if n.bits() <= new.bits() => Insert(o, new),
                other => Insert(b(other), new),
            }
        }
        Un(op, x) => match (op, *x) {
            (self::Un::Neg, Const(v, w)) => konst(v.wrapping_neg(), w),
            (self::Un::Not, Const(v, w)) => konst(!v, w),
            (self::Un::LNot, Const(v, _)) => Const(u64::from(v == 0), 1),
            (self::Un::Neg, Un(self::Un::Neg, y))
            | (self::Un::Not, Un(self::Un::Not, y))
            | (self::Un::LNot, Un(self::Un::LNot, y)) => *y,
            (self::Un::LNot, Cmp(c, l, r)) if !c.is_float() => Cmp(c.negate(), l, r),
            (self::Un::LNot, Cond(cc, o, r)) => Cond(cc.negate(), o, r),
            (self::Un::LNot, Bin(self::Bin::LAnd, l, r)) => {
                Bin(self::Bin::LOr, b(simplify_node(Un(self::Un::LNot, l))), b(simplify_node(Un(self::Un::LNot, r))))
            }
            (self::Un::LNot, Bin(self::Bin::LOr, l, r)) => {
                Bin(self::Bin::LAnd, b(simplify_node(Un(self::Un::LNot, l))), b(simplify_node(Un(self::Un::LNot, r))))
            }
            (op, other) => Un(op, b(other)),
        },
        Bin(op, x, y) => simplify_bin(op, *x, *y),
        Cmp(op, x, y) => simplify_cmp(op, *x, *y),
        Cond(cc, o, r) => match fold_cond(cc, &o, &r) {
            Some(folded) => folded,
            None => Cond(cc, o, r),
        },
        Select(c, x, y) => match *c {
            Const(v, _) => {
                if v != 0 {
                    *x
                } else {
                    *y
                }
            }
            _ if x == y => *x,
            other => Select(b(other), x, y),
        },
        Load(a, w) => Load(a, w),
        other => other,
    }
}

fn simplify_bin(op: Bin, x: Expr, y: Expr) -> Expr {
    use self::Bin::*;
    use Expr::{Bin as B, Const, StackAddr};
    let w = x.bits();
    if let (Const(p, _), Const(q, _)) = (&x, &y) {
        if matches!(op, LAnd | LOr) {
            return Const(u64::from(if op == LAnd { *p != 0 && *q != 0 } else { *p != 0 || *q != 0 }), 1);
        }
        if let Some(v) = fold_bin(op, *p, *q, w) {
            return Const(v, w);
        }
    }
    // A constant goes to the right of an operator that allows it.
    if matches!(op, Add | Mul | And | Or | Xor) && matches!(x, Const(..)) && !matches!(y, Const(..)) {
        return simplify_bin(op, y, x);
    }
    match (op, x, y) {
        (Sub, x, Const(c, cw)) => simplify_bin(Add, x, konst(c.wrapping_neg(), cw)),
        (Sub, x, y) if x == y && !x.has_call() => Const(0, w),
        (Sub, StackAddr(p), StackAddr(q)) => Const((p - q) as u64, 64),
        (Add | Or | Xor | Shl | Shr | Sar, x, Const(0, _)) => x,
        (Add, StackAddr(p), Const(c, _)) => StackAddr(p.wrapping_add(c as i64)),
        (Add, B(Add, p, q), Const(c, _)) if matches!(*q, Const(..)) => {
            let Const(d, _) = *q else { unreachable!() };
            simplify_bin(Add, *p, konst(c.wrapping_add(d), w))
        }
        // (x + c) + y: keep the constant outside.
        (Add, B(Add, p, q), y) if matches!(*q, Const(..)) => simplify_bin(Add, simplify_bin(Add, *p, y), *q),
        (Add, x, B(Add, p, q)) if matches!(*q, Const(..)) => simplify_bin(Add, simplify_bin(Add, x, *p), *q),
        (Sub, B(Add, p, q), y) if matches!(*q, Const(..)) => simplify_bin(Add, simplify_bin(Sub, *p, y), *q),
        (Mul, x, Const(1, _)) => x,
        (Mul | And, _, Const(0, _)) => Const(0, w),
        (Mul, B(Mul, p, q), Const(c, _)) if matches!(*q, Const(..)) => {
            let Const(d, _) = *q else { unreachable!() };
            simplify_bin(Mul, *p, konst(c.wrapping_mul(d), w))
        }
        (And, x, Const(c, _)) if c == mask(w) => x,
        (And, x, Const(c, _)) if matches!(x, Expr::Zext(ref i, _) if c & mask(i.bits()) == mask(i.bits())) => x,
        (And | Or, x, y) if x == y && !x.has_call() => x,
        (Xor, x, y) if x == y && !x.has_call() => Const(0, w),
        (LAnd, Const(c, _), y) => {
            if c != 0 {
                y
            } else {
                Const(0, 1)
            }
        }
        (LOr, Const(c, _), y) => {
            if c != 0 {
                Const(1, 1)
            } else {
                y
            }
        }
        (LAnd, x, Const(c, _)) if c != 0 => x,
        (LOr, x, Const(0, _)) => x,
        (op, x, y) => B(op, b(x), b(y)),
    }
}

fn simplify_cmp(op: Cmp, x: Expr, y: Expr) -> Expr {
    use Expr::{Bin as B, Const};
    let w = x.bits();
    if let (Const(p, _), Const(q, _)) = (&x, &y) {
        if let Some(v) = fold_cmp(op, *p, *q, w) {
            return Const(u64::from(v), 1);
        }
    }
    let eq = matches!(op, Cmp::Eq | Cmp::Ne);
    match (op, x, y) {
        // A condition tested against zero is the condition itself.
        (Cmp::Ne, x, Const(0, _)) if x.bits() == 1 => x,
        (Cmp::Eq, x, Const(0, _)) if x.bits() == 1 => simplify_node(Expr::Un(Un::LNot, b(x))),
        (Cmp::Ne, Expr::Zext(i, _), Const(0, _)) if i.bits() == 1 => *i,
        (Cmp::Eq, Expr::Zext(i, _), Const(0, _)) if i.bits() == 1 => simplify_node(Expr::Un(Un::LNot, i)),
        (_, B(Bin::Sub, p, q), Const(0, _)) if eq => simplify_cmp(op, *p, *q),
        (_, B(Bin::Xor, p, q), Const(0, _)) if eq => simplify_cmp(op, *p, *q),
        (_, B(Bin::Add, p, q), Const(c, cw)) if eq && matches!(*q, Const(..)) => {
            let Const(d, _) = *q else { unreachable!() };
            simplify_cmp(op, *p, konst(c.wrapping_sub(d), cw))
        }
        (_, Expr::Zext(i, _), Const(c, _)) if (eq || op.is_unsigned()) && c <= mask(i.bits()) && i.bits() >= 8 => {
            let iw = i.bits();
            simplify_cmp(op, *i, Const(c, iw))
        }
        (_, Expr::Sext(i, _), Const(c, cw))
            if (eq || op.is_signed()) && i.bits() >= 8 && {
                let s = signed(c, cw);
                s == signed(c & mask(i.bits()), i.bits())
            } =>
        {
            let iw = i.bits();
            simplify_cmp(op, *i, konst(c, iw))
        }
        (Cmp::Ult, x, Const(1, cw)) | (Cmp::Ule, x, Const(0, cw)) => Expr::Cmp(Cmp::Eq, b(x), b(Const(0, cw))),
        (Cmp::Uge, x, Const(1, cw)) | (Cmp::Ugt, x, Const(0, cw)) => Expr::Cmp(Cmp::Ne, b(x), b(Const(0, cw))),
        (op, x, y) => Expr::Cmp(op, b(x), b(y)),
    }
}

#[cfg(test)]
mod tests {
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
}
