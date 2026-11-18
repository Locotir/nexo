//! Expressions for conditions, `print` and addresses: registers, numbers, names, C operators,
//! casts and reads of memory: `rax == 0`, `*(u32*)(rdi+8) > 3`, `main+0x10`, `rsp & 0xf`.
//! The gdb spellings work too: `$rbp-0x310`, `(void*)($rbp-8)`, `*(void**)$v`, `$v+8`, `$k = *(long*)$rsp`.

use crate::{fail, Result};

/// What an expression may ask the program.
pub trait Env {
    /// A register by name (`rax`, `eax`, `pc`).
    fn reg(&mut self, name: &str) -> Option<u64>;
    /// An address for a name in the program (a function, a symbol).
    fn sym(&mut self, name: &str) -> Option<u64>;
    /// `size` bytes at `addr`, little-endian; `None` when they cannot be read.
    fn load(&mut self, addr: u64, size: usize) -> Option<u64>;
    /// A convenience variable (`$v`, name given without the `$`) set earlier.
    fn var(&mut self, _name: &str) -> Option<Val> {
        None
    }
    /// Keep a convenience variable; false when this environment keeps none.
    fn set_var(&mut self, _name: &str, _v: Val) -> bool {
        false
    }
    /// Why a register has no value here (a caller's frame, where the call may have changed it); `None` when it has
    /// one or is no register.
    fn missing(&mut self, _name: &str) -> Option<String> {
        None
    }
    /// Run the function at `target` in the program with these arguments and give back what it returns in rax.
    fn call(&mut self, _target: u64, _args: &[Arg]) -> Result<u64> {
        fail("a call into the program runs from `call <expr>` and `print <expr>`, not from here")
    }
    /// Bytes of a pointer in the program: 8, or 4 in a 32-bit program. `long`, `size_t` and the other
    /// pointer-wide C types, and a word read through a plain number (`*esp`), have this size too.
    fn ptr_bytes(&mut self) -> u8 {
        8
    }
}

/// An argument of a call into the program: a number, or text in quotes that the call gets the address of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Num(u64),
    Text(Vec<u8>),
}

/// A C type as far as an address calculation needs it: the size of what is at the end of the pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ty {
    /// Bytes of the base type (1 for `void`).
    pub bytes: u8,
    pub signed: bool,
    /// How many `*` are on the type: 0 is a plain number, 1 a pointer to the base type, 2 a pointer to a pointer.
    pub stars: u8,
    /// The base type is a character type, so a pointer to it points at text.
    pub ch: bool,
}

impl Ty {
    /// Bytes one step of pointer arithmetic moves: the base size for one star, a pointer (`ptr` bytes) for more, 1
    /// for a plain number (a plain number is not scaled).
    pub fn step(self, ptr: u8) -> u64 {
        match self.stars {
            0 => 1,
            1 => u64::from(self.bytes),
            _ => u64::from(ptr),
        }
    }

    /// The type of `*p`.
    pub fn pointee(self) -> Ty {
        Ty { stars: self.stars.saturating_sub(1), ..self }
    }

    pub fn is_ptr(self) -> bool {
        self.stars > 0
    }
}

const U64: Ty = Ty { bytes: 8, signed: false, stars: 0, ch: false };

#[derive(Debug, Clone, Copy)]
pub struct Val {
    pub v: u64,
    pub ty: Ty,
}

fn plain(v: u64) -> Val {
    Val { v, ty: U64 }
}

/// The type of a plain word of memory in a program whose pointers are `ptr` bytes.
fn word_ty(ptr: u8) -> Ty {
    Ty { bytes: ptr, ..U64 }
}

/// A type written with one word, or a few (`unsigned long long`, `const char`): its base size and signedness,
/// and whether it is a character type. `long` and the pointer-wide types (`size_t`, `intptr_t`) are `ptr` bytes,
/// the size of a pointer in the program. `None` when a word is not a C type.
pub fn base_type(words: &[&str], ptr: u8) -> Option<(u8, bool, bool)> {
    let w: Vec<&str> =
        words.iter().copied().filter(|w| !matches!(*w, "const" | "volatile" | "struct" | "union" | "enum")).collect();
    let unsigned = w.contains(&"unsigned");
    let signed = w.contains(&"signed");
    let rest: Vec<&str> = w.iter().copied().filter(|w| !matches!(*w, "unsigned" | "signed")).collect();
    let (bytes, sign, ch) = match rest.as_slice() {
        [] => (4, true, false),
        ["char"] => (1, signed, true),
        ["short"] | ["short", "int"] => (2, true, false),
        ["int"] => (4, true, false),
        ["long"] | ["long", "int"] => (ptr, true, false),
        ["long", "long"] | ["long", "long", "int"] => (8, true, false),
        ["void"] => (1, false, false),
        ["bool"] | ["_Bool"] => (1, false, false),
        [one] => {
            let (b, s, c) = match *one {
                "u8" | "uint8_t" | "byte" | "i8" | "int8_t" => (1, *one == "i8" || *one == "int8_t", true),
                "u16" | "uint16_t" | "word" => (2, false, false),
                "u32" | "uint32_t" | "dword" | "uint" => (4, false, false),
                "u64" | "uint64_t" | "qword" => (8, false, false),
                "ptr" | "usize" | "size_t" | "uintptr_t" => (ptr, false, false),
                "i16" | "int16_t" => (2, true, false),
                "i32" | "int32_t" | "wchar_t" => (4, true, false),
                "i64" | "int64_t" => (8, true, false),
                "isize" | "ssize_t" | "intptr_t" | "ptrdiff_t" | "off_t" => (ptr, true, false),
                _ => return None,
            };
            if unsigned || signed {
                return None;
            }
            return Some((b, s, c));
        }
        _ => return None,
    };
    Some((bytes, if unsigned { false } else { sign }, ch))
}

fn is_type_word(w: &str) -> bool {
    matches!(
        w,
        "unsigned"
            | "signed"
            | "const"
            | "volatile"
            | "struct"
            | "union"
            | "enum"
            | "char"
            | "short"
            | "int"
            | "long"
            | "void"
            | "bool"
            | "_Bool"
            | "u8"
            | "uint8_t"
            | "byte"
            | "i8"
            | "int8_t"
            | "u16"
            | "uint16_t"
            | "word"
            | "u32"
            | "uint32_t"
            | "dword"
            | "uint"
            | "u64"
            | "uint64_t"
            | "qword"
            | "ptr"
            | "usize"
            | "size_t"
            | "uintptr_t"
            | "i16"
            | "int16_t"
            | "i32"
            | "int32_t"
            | "wchar_t"
            | "i64"
            | "int64_t"
            | "isize"
            | "ssize_t"
            | "intptr_t"
            | "ptrdiff_t"
            | "off_t"
    )
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(u64),
    Id(String),
    Op(&'static str),
    /// Text in double quotes, its escapes worked out (`\n`, `\t`, `\0`, `\\`, `\"`, `\xNN`).
    Str(Vec<u8>),
}

const OPS: [&str; 27] = [
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "&", "|", "^", "~", "!", "<", ">", "(",
    ")", "[", "]", ",", "?", "=",
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
            // `0x59dUL`, `8ll`: the C suffixes carry no value
            let digits = t.trim_end_matches(['u', 'U', 'l', 'L']);
            match nexo_bin::parse_number(digits) {
                Ok(n) => out.push(Tok::Num(n as u64)),
                Err(_) => return fail(format!("{t:?} is not a number")),
            }
        } else if c == '\'' && i + 2 < b.len() && b[i + 2] == '\'' {
            out.push(Tok::Num(b[i + 1] as u64));
            i += 3;
        } else if c == '"' {
            let (text, next) = string_at(&b, i + 1)?;
            out.push(Tok::Str(text));
            i = next;
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

/// The bytes of a string whose text starts at `from` (after its `"`), and the index after its closing `"`.
fn string_at(b: &[char], from: usize) -> Result<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut i = from;
    while i < b.len() {
        let c = b[i];
        i += 1;
        match c {
            '"' => return Ok((out, i)),
            '\\' => {
                let Some(&e) = b.get(i) else { break };
                i += 1;
                match e {
                    'n' => out.push(b'\n'),
                    't' => out.push(b'\t'),
                    'r' => out.push(b'\r'),
                    '0' => out.push(0),
                    'x' => {
                        let hex: String = b[i..].iter().take(2).take_while(|c| c.is_ascii_hexdigit()).collect();
                        let Ok(v) = u8::from_str_radix(&hex, 16) else {
                            return fail("`\\x` in a string needs one or two hex digits");
                        };
                        out.push(v);
                        i += hex.len();
                    }
                    other => {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                    }
                }
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    fail("a string has no closing `\"`")
}

/// The expression calls a function of the program (`f(1)`, `(*p)(2)`, `0x401000(3)`) or has text in quotes, so
/// working it out runs code of the program. False when it cannot be read at all.
pub fn has_call(src: &str) -> bool {
    let Ok(t) = lex(src) else { return false };
    if t.iter().any(|x| matches!(x, Tok::Str(_))) {
        return true;
    }
    t.windows(2).enumerate().any(|(i, w)| {
        if w[1] != Tok::Op("(") {
            return false;
        }
        match &w[0] {
            Tok::Id(n) => n != "sizeof",
            Tok::Num(_) | Tok::Op("]") => true,
            // `(char*)(p)` is a cast; `(f)(1)` and `(*p)(2)` are calls
            Tok::Op(")") => !is_cast_before(&t, i),
            _ => false,
        }
    })
}

/// The `)` at `close` ends a C type in parentheses: what is between it and its `(` is type words and `*` only.
fn is_cast_before(t: &[Tok], close: usize) -> bool {
    let mut depth = 0usize;
    let mut j = close;
    loop {
        match &t[j] {
            Tok::Op(")") => depth += 1,
            Tok::Op("(") => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        if j == 0 {
            return false;
        }
        j -= 1;
    }
    let inner = &t[j + 1..close];
    inner.iter().any(|x| matches!(x, Tok::Id(n) if is_type_word(n)))
        && inner.iter().all(|x| matches!(x, Tok::Op("*")) || matches!(x, Tok::Id(n) if is_type_word(n)))
}

struct P<'a> {
    t: Vec<Tok>,
    i: usize,
    env: &'a mut dyn Env,
    /// Bytes of a pointer in the program (see [`Env::ptr_bytes`]).
    ptr: u8,
}

pub fn eval(src: &str, env: &mut dyn Env) -> Result<Val> {
    let t = lex(src)?;
    if t.is_empty() {
        return fail("the expression is empty");
    }
    let ptr = env.ptr_bytes();
    let mut p = P { t, i: 0, env, ptr };
    // `$v = expr` keeps a convenience variable
    if let (Some(Tok::Id(n)), Some(Tok::Op("="))) = (p.t.first().cloned(), p.t.get(1)) {
        if let Some(name) = n.strip_prefix('$').filter(|x| !x.is_empty()) {
            if p.env.reg(name).is_some() || p.env.missing(name).is_some() {
                return fail(format!("{n} is a register: `setreg {name} <value>` changes it"));
            }
            p.i = 2;
            let v = p.ternary()?;
            if p.i < p.t.len() {
                return fail(format!("unexpected {:?} after the expression", p.t[p.i]));
            }
            if !p.env.set_var(name, v) {
                return fail("this place keeps no convenience variables");
            }
            return Ok(v);
        }
    }
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

/// A place in the program's memory: where it starts and how many bytes the expression's type has there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    pub addr: u64,
    pub size: usize,
}

/// The place an expression names, without reading it: `*E` (the pointer's value, the size of what it points at)
/// and `E[i]`. `None` for other forms, which the caller reads as plain addresses or as variables. An operator
/// after the place (`*p + 1`) is no place and says so.
pub fn place(src: &str, env: &mut dyn Env) -> Result<Option<Place>> {
    let t = lex(src)?;
    if t.is_empty() {
        return fail("the expression is empty");
    }
    let ptr = env.ptr_bytes();
    let not_one =
        || fail(format!("{:?} is not one place in memory: the result of an operator has no address", src.trim()));
    // the `[` that matches the `]` ending the expression
    let mut open = None;
    if matches!(t.last(), Some(Tok::Op("]"))) {
        let mut depth = 0usize;
        for (k, tok) in t.iter().enumerate().rev() {
            match tok {
                Tok::Op("]") => depth += 1,
                Tok::Op("[") => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        open = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(k) = open.filter(|k| *k > 0) {
        let mut b = P { t: t[..k].to_vec(), i: 0, env: &mut *env, ptr };
        let base = b.ternary()?;
        if b.i < b.t.len() {
            return not_one();
        }
        let mut q = P { t: t[k + 1..t.len() - 1].to_vec(), i: 0, env: &mut *env, ptr };
        let idx = q.ternary()?;
        if q.i < q.t.len() {
            return not_one();
        }
        let (step, ty) = if base.ty.is_ptr() {
            (base.ty.step(ptr), base.ty.pointee().with_plain(base.ty, ptr))
        } else {
            (u64::from(ptr), word_ty(ptr))
        };
        return Ok(Some(Place { addr: base.v.wrapping_add(idx.v.wrapping_mul(step)), size: ty.target_bytes_of(ptr) }));
    }
    if matches!(t.first(), Some(Tok::Op("*"))) {
        let mut p = P { t, i: 1, env, ptr };
        let v = p.unary()?;
        if p.i < p.t.len() {
            return not_one();
        }
        let ty = v.ty.pointee().with_plain(v.ty, ptr);
        return Ok(Some(Place { addr: v.v, size: ty.target_bytes_of(ptr) }));
    }
    Ok(None)
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
                    l = apply(op, l, r, self.ptr)?;
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
                    l = apply(op, l, r, self.ptr)?;
                    continue 'outer;
                }
            }
            return Ok(l);
        }
    }

    /// A C type at `from` followed by `)`: the type and the index after the `)`.
    fn type_at(&self, from: usize) -> Option<(Ty, usize)> {
        let mut words: Vec<&str> = Vec::new();
        let mut i = from;
        while let Some(Tok::Id(n)) = self.t.get(i) {
            if !is_type_word(n) {
                break;
            }
            words.push(n);
            i += 1;
        }
        if words.is_empty() {
            return None;
        }
        let mut stars = 0u8;
        while matches!(self.t.get(i), Some(Tok::Op("*"))) {
            stars = stars.saturating_add(1);
            i += 1;
        }
        if !matches!(self.t.get(i), Some(Tok::Op(")"))) {
            return None;
        }
        let (bytes, signed, ch) = base_type(&words, self.ptr)?;
        Some((Ty { bytes, signed, stars, ch }, i + 1))
    }

    fn unary(&mut self) -> Result<Val> {
        if self.eat("-") {
            let v = self.unary()?;
            return Ok(Val { v: v.v.wrapping_neg(), ty: Ty { signed: true, ..v.ty } });
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Ok(plain((self.unary()?.v == 0) as u64));
        }
        if self.eat("~") {
            let v = self.unary()?;
            return Ok(Val { v: !v.v, ..v });
        }
        if self.eat("&") {
            // `&sym` is the address of a name, which is what a name already is here
            return self.unary();
        }
        if self.eat("*") {
            let p = self.unary()?;
            return self.deref(p.v, p.ty.pointee().with_plain(p.ty, self.ptr));
        }
        if self.peek_op("(") {
            // a cast: `(u32)x`, `(char*)p`, `(void**)p`, `(unsigned long)x`
            if let Some((ty, next)) = self.type_at(self.i + 1) {
                self.i = next;
                let v = self.unary()?;
                // a pointer is cut to the program's pointer size, a number to its type's size
                let n = ty.target_bytes_of(self.ptr) as u32;
                let v = if n >= 8 {
                    v.v
                } else {
                    let cut = v.v & (u64::MAX >> (64 - 8 * n));
                    if ty.signed && ty.stars == 0 {
                        let sh = 64 - 8 * n;
                        (((cut << sh) as i64) >> sh) as u64
                    } else {
                        cut
                    }
                };
                return Ok(Val { v, ty });
            }
        }
        self.postfix()
    }

    fn deref(&mut self, addr: u64, ty: Ty) -> Result<Val> {
        let n = ty.target_bytes_of(self.ptr);
        match self.env.load(addr, n) {
            Some(raw) => {
                let v = if ty.signed && ty.stars == 0 && n < 8 {
                    let sh = 64 - 8 * (n as u32);
                    (((raw << sh) as i64) >> sh) as u64
                } else {
                    raw
                };
                Ok(Val { v, ty })
            }
            None => fail(format!("cannot read {n} bytes at {addr:#x}")),
        }
    }

    fn postfix(&mut self) -> Result<Val> {
        let mut v = self.atom()?;
        loop {
            // `p[i]` reads element i of what a typed pointer points at
            if self.eat("[") {
                let i = self.ternary()?;
                if !self.eat("]") {
                    return fail("`[` needs a `]`");
                }
                let (step, ty) = if v.ty.is_ptr() {
                    (v.ty.step(self.ptr), v.ty.pointee().with_plain(v.ty, self.ptr))
                } else {
                    (u64::from(self.ptr), word_ty(self.ptr))
                };
                v = self.deref(v.v.wrapping_add(i.v.wrapping_mul(step)), ty)?;
            } else if self.eat("(") {
                // `f(a, b)` runs the function at that address in the program
                let args = self.call_args()?;
                v = plain(self.env.call(v.v, &args)?);
            } else {
                return Ok(v);
            }
        }
    }

    /// The arguments of a call, after its `(`, through its `)`.
    fn call_args(&mut self) -> Result<Vec<Arg>> {
        let mut args = Vec::new();
        if self.eat(")") {
            return Ok(args);
        }
        loop {
            if let Some(Tok::Str(s)) = self.t.get(self.i).cloned() {
                self.i += 1;
                args.push(Arg::Text(s));
            } else {
                args.push(Arg::Num(self.ternary()?.v));
            }
            if self.eat(")") {
                return Ok(args);
            }
            if !self.eat(",") {
                return fail("the arguments of a call are separated by `,` and end with `)`");
            }
        }
    }

    fn atom(&mut self) -> Result<Val> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Num(n)) => {
                self.i += 1;
                Ok(plain(n))
            }
            Some(Tok::Id(n)) if n == "sizeof" && self.peek_at_op(1, "(") => {
                self.i += 2;
                let Some((ty, next)) = self.type_at(self.i) else { return fail("sizeof takes a C type: sizeof(int)") };
                self.i = next;
                Ok(plain(ty.target_bytes_of(self.ptr) as u64))
            }
            Some(Tok::Id(n)) => {
                self.i += 1;
                if let Some(v) = self.env.reg(&n) {
                    return Ok(plain(v));
                }
                if let Some(why) = self.env.missing(&n) {
                    return fail(why);
                }
                if let Some(name) = n.strip_prefix('$') {
                    return match self.env.var(name) {
                        Some(v) => Ok(v),
                        None => fail(format!("{n} is not a register and no `set {n} = ...` came before it")),
                    };
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
            Some(Tok::Str(_)) => fail("text in quotes stands only as an argument of a call: `call f(\"text\")`"),
            Some(t) => fail(format!("unexpected {t:?} in the expression")),
            None => fail("the expression ends too soon"),
        }
    }

    fn peek_at_op(&self, ahead: usize, o: &str) -> bool {
        matches!(self.t.get(self.i + ahead), Some(Tok::Op(x)) if *x == o)
    }
}

impl Ty {
    /// The type of `*p` when `p` has type `outer`: through a plain number, a plain word of `ptr` bytes is read.
    fn with_plain(self, outer: Ty, ptr: u8) -> Ty {
        if outer.stars == 0 {
            word_ty(ptr)
        } else {
            self
        }
    }

    /// Bytes read for a value of this type, as the type of a loaded value: a pointer is `ptr` bytes, a plain value
    /// its size.
    fn target_bytes_of(self, ptr: u8) -> usize {
        if self.stars > 0 {
            ptr as usize
        } else {
            self.bytes as usize
        }
    }
}

fn apply(op: &str, l: Val, r: Val, ptr: u8) -> Result<Val> {
    let signed = l.ty.signed || r.ty.signed;
    let (mut a, mut b) = (l.v, r.v);
    // pointer arithmetic moves in whole elements, as in C: `(u32*)p + 2` is p + 8
    let (lp, rp) = (l.ty.is_ptr(), r.ty.is_ptr());
    let mut ty = Ty { signed, ..U64 };
    match op {
        "+" if lp && !rp => {
            b = b.wrapping_mul(l.ty.step(ptr));
            ty = l.ty;
        }
        "+" if rp && !lp => {
            a = a.wrapping_mul(r.ty.step(ptr));
            ty = r.ty;
        }
        "-" if lp && !rp => {
            b = b.wrapping_mul(l.ty.step(ptr));
            ty = l.ty;
        }
        "-" if lp && rp => {
            let d = (a as i64).wrapping_sub(b as i64) / l.ty.step(ptr).max(1) as i64;
            return Ok(Val { v: d as u64, ty: Ty { signed: true, ..U64 } });
        }
        _ => {}
    }
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
    Ok(Val { v, ty })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct T {
        vars: HashMap<String, Val>,
        calls: Vec<(u64, Vec<Arg>)>,
    }
    impl Env for T {
        fn reg(&mut self, n: &str) -> Option<u64> {
            match n.trim_start_matches('$') {
                "rax" => Some(7),
                "rdi" => Some(0x1000),
                "rbp" => Some(0x1100),
                _ => None,
            }
        }
        fn sym(&mut self, n: &str) -> Option<u64> {
            match n {
                "main" => Some(0x4000),
                "_ZN6LCRYPT7encryptEv.cold" => Some(0x4100),
                _ => None,
            }
        }
        fn load(&mut self, a: u64, size: usize) -> Option<u64> {
            let mask = u64::MAX >> (64 - 8 * size as u32);
            if (0x1000..0x1100).contains(&a) {
                Some((0xffff_ff00u64 + (a & 0xff)) & mask)
            } else if (0x2000..0x2100).contains(&a) {
                // a table of pointers: the pointer at 0x2000+k points at 0x1000+k
                Some((0x1000 + (a - 0x2000)) & mask)
            } else {
                None
            }
        }
        fn var(&mut self, n: &str) -> Option<Val> {
            self.vars.get(n).copied()
        }
        fn set_var(&mut self, n: &str, v: Val) -> bool {
            self.vars.insert(n.to_string(), v);
            true
        }
        fn call(&mut self, target: u64, args: &[Arg]) -> Result<u64> {
            self.calls.push((target, args.to_vec()));
            Ok(target + args.len() as u64)
        }
    }

    fn v(s: &str, e: &mut T) -> u64 {
        value(s, e).unwrap_or_else(|x| panic!("{s}: {x}"))
    }

    #[test]
    fn operators_registers_names_and_memory() {
        let mut e = T::default();
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

    #[test]
    fn a_deref_or_an_index_names_a_place_with_the_size_of_its_type() {
        let mut e = T::default();
        let p = |s: &str, e: &mut T| place(s, e).unwrap_or_else(|x| panic!("{s}: {x}"));
        assert_eq!(p("*(u32*)(rdi+8)", &mut e), Some(Place { addr: 0x1008, size: 4 }));
        assert_eq!(p("*(u8*)(rdi+8)", &mut e), Some(Place { addr: 0x1008, size: 1 }));
        assert_eq!(p("*(u64*)rdi", &mut e), Some(Place { addr: 0x1000, size: 8 }));
        // a plain number points at a word
        assert_eq!(p("*0x2000", &mut e), Some(Place { addr: 0x2000, size: 8 }));
        // what a `u32**` points at is a pointer
        assert_eq!(p("*(u32**)0x2004", &mut e), Some(Place { addr: 0x2004, size: 8 }));
        assert_eq!(p("((u32*)rdi)[2]", &mut e), Some(Place { addr: 0x1008, size: 4 }));
        assert_eq!(p("((u16*)rdi)[rax - 4]", &mut e), Some(Place { addr: 0x1006, size: 2 }));
        // other forms are not places here, and an operator's result has no address
        assert_eq!(p("rdi+8", &mut e), None);
        assert!(place("*(u32*)rdi + 1", &mut e).unwrap_err().to_string().contains("not one place"));
        assert!(place("((u32*)rdi)[1] + 1", &mut e).is_ok_and(|p| p.is_none()));
        assert!(place("nope[1]", &mut e).is_err());
    }

    #[test]
    fn gdb_registers_and_void_pointers_read_like_gdb() {
        let mut e = T::default();
        assert_eq!(v("$rbp-0x100", &mut e), 0x1000);
        assert_eq!(v("(void*)($rbp-0x100)", &mut e), 0x1000);
        // a void* moves by bytes
        assert_eq!(v("(void*)($rbp-0x100) + 8", &mut e), 0x1008);
        // the table of pointers: *(void**)(0x2000+8) is 0x1008
        assert_eq!(v("*(void**)0x2008", &mut e), 0x1008);
        assert_eq!(v("*(u8*)*(void**)0x2008", &mut e), 8);
        assert_eq!(v("**(u8**)0x2008", &mut e), 8);
        assert_eq!(v("*(unsigned char*)*(void**)(0x2000 + 8)", &mut e), 8);
        assert_eq!(v("(char*)0x10 + 3", &mut e), 0x13);
        assert_eq!(v("$rax + 1", &mut e), 8);
    }

    #[test]
    fn convenience_variables_keep_values_and_their_type() {
        let mut e = T::default();
        assert_eq!(v("$v = (void*)0x2000", &mut e), 0x2000);
        assert_eq!(v("$v + 8", &mut e), 0x2008);
        // `printf "%p", *(void**)($v+8)` shape
        assert_eq!(v("*(void**)($v+8)", &mut e), 0x1008);
        assert_eq!(v("$k = *(void**)($v + 0x10)", &mut e), 0x1010);
        assert_eq!(v("*(u8*)($k + 3)", &mut e), 0x13);
        assert_eq!(v("$n = 5", &mut e) + v("$n", &mut e), 10);
        let unset = value("$nothing + 1", &mut e).unwrap_err().to_string();
        assert!(unset.contains("$nothing") && unset.contains("set"), "{unset}");
        let reg = value("$rax = 3", &mut e).unwrap_err().to_string();
        assert!(reg.contains("register") && reg.contains("setreg"), "{reg}");
    }

    #[test]
    fn casts_scale_pointers_and_cut_integers() {
        let mut e = T::default();
        assert_eq!(v("(unsigned char)0x1ff", &mut e), 0xff);
        assert_eq!(v("(int)0xffffffff < 0", &mut e), 1);
        assert_eq!(v("(unsigned int)0xffffffff > 0", &mut e), 1);
        assert_eq!(v("(unsigned long long)5 + 1", &mut e), 6);
        assert_eq!(v("(u32*)0x10 + 2", &mut e), 0x18);
        assert_eq!(v("(void**)0x10 + 2", &mut e), 0x20);
        assert_eq!(v("(u32*)0x18 - (u32*)0x10", &mut e), 2);
        assert_eq!(v("(u16*)0x20 - 3", &mut e), 0x1a);
        assert_eq!(v("sizeof(long) + sizeof(unsigned short)", &mut e), 10);
        assert_eq!(v("sizeof(char*)", &mut e), 8);
        assert_eq!(v("0x10UL + 1ull", &mut e), 0x11);
        // a parenthesis around a register or a name is not a cast
        assert_eq!(v("(rax) + (main)", &mut e), 0x4007);
        assert_eq!(v("*(const char*)0x1010", &mut e), 0x10);
    }

    #[test]
    fn mangled_names_with_an_offset_are_one_name() {
        let mut e = T::default();
        assert_eq!(v("_ZN6LCRYPT7encryptEv.cold+0x59d", &mut e), 0x4100 + 0x59d);
        assert_eq!(v("(_ZN6LCRYPT7encryptEv.cold + 0x59d)", &mut e), 0x4100 + 0x59d);
    }

    #[test]
    fn a_call_passes_numbers_and_text_and_gives_back_its_result() {
        let mut e = T::default();
        assert_eq!(v("main(1, rax + 1, \"a\\n\\x41\\0\")", &mut e), 0x4003);
        assert_eq!(e.calls, [(0x4000, vec![Arg::Num(1), Arg::Num(8), Arg::Text(b"a\nA\0".to_vec())])]);
        // the result takes part in the expression, a call through a pointer and an empty list included
        assert_eq!(v("main() + 1", &mut e), 0x4001);
        assert_eq!(v("(*(u64*)0x2008)(2)", &mut e), 0x1008 + 1);
        assert!(value("\"text\"", &mut e).unwrap_err().to_string().contains("argument of a call"));
        assert!(value("main(1 2)", &mut e).is_err());
        assert!(value("main(\"open", &mut e).unwrap_err().to_string().contains("closing"));
    }

    #[test]
    fn has_call_tells_a_call_from_a_cast_or_sizeof() {
        for c in ["f(1)", "main(\"x\")", "(*p)(2)", "0x401000(3)", "(f)(1)", "t[1](2)", "x + g()", "\"s\""] {
            assert!(has_call(c), "{c}");
        }
        for c in ["(char*)(p)", "(unsigned long)(rax)", "sizeof(int)", "*(u32*)(rdi+8)", "rax + 8", "(rax) + (main)"] {
            assert!(!has_call(c), "{c}");
        }
    }

    #[test]
    fn a_call_elsewhere_than_call_and_print_says_where_it_runs() {
        struct NoCalls;
        impl Env for NoCalls {
            fn reg(&mut self, _: &str) -> Option<u64> {
                None
            }
            fn sym(&mut self, _: &str) -> Option<u64> {
                Some(0x10)
            }
            fn load(&mut self, _: u64, _: usize) -> Option<u64> {
                None
            }
        }
        let e = value("f(1)", &mut NoCalls).unwrap_err().to_string();
        assert!(e.contains("`call <expr>` and `print <expr>`"), "{e}");
    }

    /// A 32-bit program stopped at `main`: its stack holds the return address, then `argv`, then other words.
    struct Main32;
    impl Env for Main32 {
        fn reg(&mut self, n: &str) -> Option<u64> {
            (n.trim_start_matches('$') == "esp").then_some(0x8000)
        }
        fn sym(&mut self, _: &str) -> Option<u64> {
            None
        }
        fn load(&mut self, a: u64, size: usize) -> Option<u64> {
            let words: [(u64, u32); 6] = [
                (0x8000, 0x1111_1111),
                (0x8004, 0x8010),
                (0x8008, 0xdead_beef),
                (0x8010, 0x8020),
                (0x8014, 0x8024),
                (0x8020, u32::from_le_bytes(*b"ab\0\0")),
            ];
            let mut mem = [0u8; 0x28];
            for (at, w) in words {
                let o = (at - 0x8000) as usize;
                mem[o..o + 4].copy_from_slice(&w.to_le_bytes());
            }
            let o = usize::try_from(a.checked_sub(0x8000)?).ok()?;
            let mut b = [0u8; 8];
            b[..size].copy_from_slice(mem.get(o..o + size)?);
            Some(u64::from_le_bytes(b))
        }
        fn ptr_bytes(&mut self) -> u8 {
            4
        }
    }

    #[test]
    fn in_a_32_bit_program_pointers_and_long_are_4_bytes() {
        let v = |s: &str| value(s, &mut Main32).unwrap_or_else(|x| panic!("{s}: {x}"));
        // `argv` alone, not `argv` with the word after it in the high half
        assert_eq!(v("*(char**)($esp+4)"), 0x8010);
        assert_eq!(v("**(char***)($esp+4)"), 0x8020);
        assert_eq!(v("*(u8*)**(char***)($esp+4)"), u64::from(b'a'));
        // pointer arithmetic over pointers moves 4 bytes a step
        assert_eq!(v("((char**)0x8010)[1]"), 0x8024);
        assert_eq!(v("*((char**)0x8010 + 1)"), 0x8024);
        assert_eq!(v("(void**)0x10 + 2"), 0x18);
        assert_eq!(v("(char**)0x8014 - (char**)0x8010"), 1);
        // a word read through a plain number is a stack slot of the program
        assert_eq!(v("*$esp"), 0x1111_1111);
        assert_eq!(v("$esp[1]"), 0x8010);
        assert_eq!(v("sizeof(char*) + sizeof(long) + sizeof(size_t)"), 12);
        assert_eq!(v("sizeof(long long) + sizeof(u64)"), 16);
        assert_eq!(v("(long)0xffffffff < 0"), 1);
        assert_eq!(v("(char*)-1"), 0xffff_ffff);
    }
}
