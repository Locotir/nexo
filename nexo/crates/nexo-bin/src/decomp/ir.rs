//! The small language a function is lifted into: variables, expressions that know their width,
//! statements that remember the instruction they came from, and the rules that make an expression
//! smaller without changing its value.

/// Width in bits: 1 (a condition), 8, 16, 32, 64 or 128. 0 is a flags value, never printed alone.
pub type Bits = u8;

mod simplify;
pub use self::simplify::*;

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

#[cfg(test)]
mod tests;
