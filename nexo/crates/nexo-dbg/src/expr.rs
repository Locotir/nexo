//! Expressions for conditions, `print` and addresses: registers, numbers, names, C operators,
//! casts and reads of memory: `rax == 0`, `*(u32*)(rdi+8) > 3`, `main+0x10`, `rsp & 0xf`.

use crate::{fail, Result};

/// What an expression may ask the program.
pub trait Env {
    /// A register by name (`rax`, `eax`, `pc`).
    fn reg(&mut self, name: &str) -> Option<u64>;
    /// An address for a name in the program (a function, a symbol).
    fn sym(&mut self, name: &str) -> Option<u64>;
    /// `size` bytes at `addr`, little-endian; `None` when they cannot be read.
    fn load(&mut self, addr: u64, size: usize) -> Option<u64>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ty {
    pub bytes: u8,
    pub signed: bool,
    /// A pointer to something of this type (`*p` reads one).
    pub ptr: bool,
}

const U64: Ty = Ty { bytes: 8, signed: false, ptr: false };

#[derive(Debug, Clone, Copy)]
pub struct Val {
    pub v: u64,
    pub ty: Ty,
}

fn plain(v: u64) -> Val {
    Val { v, ty: U64 }
}

fn type_named(n: &str) -> Option<Ty> {
    let (bytes, signed) = match n {
        "u8" | "uint8_t" | "char" | "byte" => (1, false),
        "u16" | "uint16_t" | "word" => (2, false),
        "u32" | "uint32_t" | "dword" | "uint" => (4, false),
        "u64" | "uint64_t" | "qword" | "ptr" | "usize" | "size_t" | "long" => (8, false),
        "i8" | "int8_t" => (1, true),
        "i16" | "int16_t" | "short" => (2, true),
        "i32" | "int32_t" | "int" => (4, true),
        "i64" | "int64_t" | "isize" => (8, true),
        _ => return None,
    };
    Some(Ty { bytes, signed, ptr: false })
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(u64),
    Id(String),
    Op(&'static str),
}

const OPS: [&str; 26] = [
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "&", "|", "^", "~", "!", "<", ">", "(",
    ")", "[", "]", ",", "?",
];

fn lex(s: &str) -> Result<Vec<Tok>> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            let t: String = b[st..i].iter().collect();
            match nexo_bin::parse_number(&t) {
                Ok(n) => out.push(Tok::Num(n as u64)),
                Err(_) => return fail(format!("{t:?} is not a number")),
            }
        } else if c == '\'' && i + 2 < b.len() && b[i + 2] == '\'' {
            out.push(Tok::Num(b[i + 1] as u64));
            i += 3;
        } else if c.is_alphabetic() || matches!(c, '_' | '$' | '.' | '@' | ':') {
            let st = i;
            i += 1;
            while i < b.len()
                && (b[i].is_alphanumeric()
                    || matches!(b[i], '_' | '.' | '@' | '$' | ':')
                    || (b[i] == '!' && b.get(i + 1).is_some_and(|c| c.is_alphabetic())))
            {
                i += 1;
            }
            out.push(Tok::Id(b[st..i].iter().collect()));
        } else if let Some(op) = OPS.iter().find(|o| b[i..].iter().take(o.len()).collect::<String>() == **o) {
            out.push(Tok::Op(op));
            i += op.len();
        } else {
            return fail(format!("unexpected {c:?} in the expression"));
        }
    }
    Ok(out)
}

struct P<'a> {
    t: Vec<Tok>,
    i: usize,
    env: &'a mut dyn Env,
}

pub fn eval(src: &str, env: &mut dyn Env) -> Result<Val> {
    let t = lex(src)?;
    if t.is_empty() {
        return fail("the expression is empty");
    }
    let mut p = P { t, i: 0, env };
    let v = p.ternary()?;
    if p.i < p.t.len() {
        return fail(format!("unexpected {:?} after the expression", p.t[p.i]));
    }
    Ok(v)
}

/// The value of an expression as a number.
pub fn value(src: &str, env: &mut dyn Env) -> Result<u64> {
    Ok(eval(src, env)?.v)
}

impl P<'_> {
    fn peek_op(&self, o: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Op(x)) if *x == o)
    }

    fn eat(&mut self, o: &str) -> bool {
        let r = self.peek_op(o);
        if r {
            self.i += 1;
        }
        r
    }

    fn ternary(&mut self) -> Result<Val> {
        let c = self.bin(0)?;
        if self.eat("?") {
            let a = self.ternary()?;
            if !matches!(self.t.get(self.i), Some(Tok::Id(x)) if x == ":") {
                return fail("`?` needs a `:`");
            }
            self.i += 1;
            let b = self.ternary()?;
            return Ok(if c.v != 0 { a } else { b });
        }
        Ok(c)
    }

    fn bin(&mut self, level: usize) -> Result<Val> {
        const LEVELS: [&[&str]; 9] = [
            &["||"],
            &["&&"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!="],
            &["<", "<=", ">", ">="],
            &["<<", ">>"],
            &["+", "-"],
        ];
        if level == LEVELS.len() {
            return self.mul();
        }
        let mut l = self.bin(level + 1)?;
        'outer: loop {
            for op in LEVELS[level] {
                if self.eat(op) {
                    let r = self.bin(level + 1)?;
                    l = apply(op, l, r)?;
                    continue 'outer;
                }
            }
            return Ok(l);
        }
    }

    fn mul(&mut self) -> Result<Val> {
        let mut l = self.unary()?;
        'outer: loop {
            for op in ["*", "/", "%"] {
                if self.eat(op) {
                    let r = self.unary()?;
                    l = apply(op, l, r)?;
                    continue 'outer;
                }
            }
            return Ok(l);
        }
    }

    fn unary(&mut self) -> Result<Val> {
        if self.eat("-") {
            let v = self.unary()?;
            return Ok(Val { v: v.v.wrapping_neg(), ty: Ty { signed: true, ..v.ty } });
        }
        if self.eat("!") {
            return Ok(plain((self.unary()?.v == 0) as u64));
        }
        if self.eat("~") {
            let v = self.unary()?;
            return Ok(Val { v: !v.v, ..v });
        }
        if self.eat("*") {
            let p = self.unary()?;
            let ty = if p.ty.ptr { Ty { ptr: false, ..p.ty } } else { U64 };
            return self.deref(p.v, ty);
        }
        if self.peek_op("(") {
            // a cast: `(u32)x` or `(u32*)x`
            if let (Some(Tok::Id(n)), true) = (self.t.get(self.i + 1), true) {
                if let Some(ty) = type_named(n) {
                    let star = matches!(self.t.get(self.i + 2), Some(Tok::Op("*")));
                    let close = self.i + 2 + star as usize;
                    if matches!(self.t.get(close), Some(Tok::Op(")"))) {
                        self.i = close + 1;
                        let v = self.unary()?;
                        let cut = if star || ty.bytes == 8 {
                            v.v
                        } else {
                            v.v & (u64::MAX >> (64 - 8 * u32::from(ty.bytes)))
                        };
                        return Ok(Val { v: cut, ty: Ty { ptr: star, ..ty } });
                    }
                }
            }
        }
        self.postfix()
    }

    fn deref(&mut self, addr: u64, ty: Ty) -> Result<Val> {
        match self.env.load(addr, ty.bytes as usize) {
            Some(raw) => {
                let v = if ty.signed && ty.bytes < 8 {
                    let sh = 64 - 8 * u32::from(ty.bytes);
                    (((raw << sh) as i64) >> sh) as u64
                } else {
                    raw
                };
                Ok(Val { v, ty })
            }
            None => fail(format!("cannot read {} bytes at {addr:#x}", ty.bytes)),
        }
    }

    fn postfix(&mut self) -> Result<Val> {
        let mut v = self.atom()?;
        // `p[i]` reads element i of what a typed pointer points at
        while self.eat("[") {
            let i = self.ternary()?;
            if !self.eat("]") {
                return fail("`[` needs a `]`");
            }
            let ty = if v.ty.ptr { Ty { ptr: false, ..v.ty } } else { U64 };
            v = self.deref(v.v.wrapping_add(i.v.wrapping_mul(u64::from(ty.bytes))), ty)?;
        }
        Ok(v)
    }

    fn atom(&mut self) -> Result<Val> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(plain(n))
            }
            Some(Tok::Id(n)) => {
                self.i += 1;
                if let Some(v) = self.env.reg(&n) {
                    return Ok(plain(v));
                }
                match self.env.sym(&n) {
                    Some(a) => Ok(plain(a)),
                    None => fail(format!("{n:?} is not a register, a name in the program or a number")),
                }
            }
            Some(Tok::Op("(")) => {
                self.i += 1;
                let v = self.ternary()?;
                if !self.eat(")") {
                    return fail("`(` needs a `)`");
                }
                Ok(v)
            }
            Some(t) => fail(format!("unexpected {t:?} in the expression")),
            None => fail("the expression ends too soon"),
        }
    }
}

fn apply(op: &str, l: Val, r: Val) -> Result<Val> {
    let signed = l.ty.signed || r.ty.signed;
    let (a, b) = (l.v, r.v);
    let cmp = |f: fn(i64, i64) -> bool, g: fn(u64, u64) -> bool| {
        (if signed { f(a as i64, b as i64) } else { g(a, b) }) as u64
    };
    let v = match op {
        "||" => (a != 0 || b != 0) as u64,
        "&&" => (a != 0 && b != 0) as u64,
        "|" => a | b,
        "^" => a ^ b,
        "&" => a & b,
        "==" => (a == b) as u64,
        "!=" => (a != b) as u64,
        "<" => cmp(|x, y| x < y, |x, y| x < y),
        "<=" => cmp(|x, y| x <= y, |x, y| x <= y),
        ">" => cmp(|x, y| x > y, |x, y| x > y),
        ">=" => cmp(|x, y| x >= y, |x, y| x >= y),
        "<<" => a.wrapping_shl(b as u32),
        ">>" => a.wrapping_shr(b as u32),
        "+" => a.wrapping_add(b),
        "-" => a.wrapping_sub(b),
        "*" => a.wrapping_mul(b),
        "/" | "%" => {
            if b == 0 {
                return fail("division by zero");
            }
            match (op, signed) {
                ("/", true) => (a as i64).wrapping_div(b as i64) as u64,
                ("/", false) => a / b,
                (_, true) => (a as i64).wrapping_rem(b as i64) as u64,
                _ => a % b,
            }
        }
        _ => return fail(format!("unknown operator {op}")),
    };
    // a pointer plus a number stays a pointer of the same type
    let ty = if l.ty.ptr && matches!(op, "+" | "-") { l.ty } else { Ty { signed, ..U64 } };
    Ok(Val { v, ty })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct T;
    impl Env for T {
        fn reg(&mut self, n: &str) -> Option<u64> {
            match n {
                "rax" => Some(7),
                "rdi" => Some(0x1000),
                _ => None,
            }
        }
        fn sym(&mut self, n: &str) -> Option<u64> {
            (n == "main").then_some(0x4000)
        }
        fn load(&mut self, a: u64, size: usize) -> Option<u64> {
            (0x1000..0x1100).contains(&a).then(|| (0xffff_ff00u64 + (a & 0xff)) & (u64::MAX >> (64 - 8 * size as u32)))
        }
    }

    #[test]
    fn operators_registers_names_and_memory() {
        let mut e = T;
        let v = |s: &str, e: &mut T| value(s, e).unwrap();
        assert_eq!(v("rax == 7 && rax * 2 > 13", &mut e), 1);
        assert_eq!(v("main + 0x10", &mut e), 0x4010);
        assert_eq!(v("(rdi + 8) & 0xf", &mut e), 8);
        assert_eq!(v("*(u32*)(rdi+8)", &mut e), 0xffff_ff08);
        assert_eq!(v("*(u8*)(rdi+8)", &mut e), 8);
        assert_eq!(v("*(i32*)rdi < 0", &mut e), 1);
        assert_eq!(v("((u32*)rdi)[2]", &mut e), 0xffff_ff08);
        assert_eq!(v("-1 < 0", &mut e), 1);
        assert_eq!(v("1 << 4 | 3", &mut e), 0x13);
        assert!(value("rax +", &mut e).is_err());
        assert!(value("nope", &mut e).is_err());
        assert!(value("*0x5000", &mut e).unwrap_err().to_string().contains("cannot read"));
    }
}
