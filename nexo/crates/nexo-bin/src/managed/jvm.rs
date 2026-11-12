//! JVM `.class` files: the constant pool, fields, methods with descriptors, and bytecode with
//! resolved names. Parsing is strict about bounds; anything it cannot decode is said with its offset.

use super::{Code, FieldDef, Insn, MethodDef, TypeDef};
use std::collections::HashSet;

const NAMES: &str = "nop aconst_null iconst_m1 iconst_0 iconst_1 iconst_2 iconst_3 iconst_4 iconst_5 lconst_0 lconst_1 \
fconst_0 fconst_1 fconst_2 dconst_0 dconst_1 bipush sipush ldc ldc_w ldc2_w iload lload fload dload aload \
iload_0 iload_1 iload_2 iload_3 lload_0 lload_1 lload_2 lload_3 fload_0 fload_1 fload_2 fload_3 dload_0 dload_1 dload_2 dload_3 \
aload_0 aload_1 aload_2 aload_3 iaload laload faload daload aaload baload caload saload istore lstore fstore dstore astore \
istore_0 istore_1 istore_2 istore_3 lstore_0 lstore_1 lstore_2 lstore_3 fstore_0 fstore_1 fstore_2 fstore_3 \
dstore_0 dstore_1 dstore_2 dstore_3 astore_0 astore_1 astore_2 astore_3 iastore lastore fastore dastore aastore bastore castore sastore \
pop pop2 dup dup_x1 dup_x2 dup2 dup2_x1 dup2_x2 swap iadd ladd fadd dadd isub lsub fsub dsub imul lmul fmul dmul \
idiv ldiv fdiv ddiv irem lrem frem drem ineg lneg fneg dneg ishl lshl ishr lshr iushr lushr iand land ior lor ixor lxor iinc \
i2l i2f i2d l2i l2f l2d f2i f2l f2d d2i d2l d2f i2b i2c i2s lcmp fcmpl fcmpg dcmpl dcmpg \
ifeq ifne iflt ifge ifgt ifle if_icmpeq if_icmpne if_icmplt if_icmpge if_icmpgt if_icmple if_acmpeq if_acmpne goto jsr ret \
tableswitch lookupswitch ireturn lreturn freturn dreturn areturn return getstatic putstatic getfield putfield \
invokevirtual invokespecial invokestatic invokeinterface invokedynamic new newarray anewarray arraylength athrow \
checkcast instanceof monitorenter monitorexit wide multianewarray ifnull ifnonnull goto_w jsr_w";

/// One constant-pool entry.
#[derive(Clone, Debug)]
enum Cp {
    None,
    Utf8(String),
    Int(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    Str(u16),
    /// A field, method or interface method reference: class, name-and-type.
    Ref(u16, u16),
    NameType(u16, u16),
    Handle(u8, u16),
    MethodType(u16),
    /// Dynamic constant or invokedynamic: bootstrap index, name-and-type.
    Dyn(u16, u16, bool),
    Other,
}

struct R<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.d.get(self.p..self.p.checked_add(n).ok_or("length overflow")?);
        let s = s.ok_or_else(|| format!("cut short at byte {} (needs {n} more)", self.p))?;
        self.p += n;
        Ok(s)
    }
    fn u1(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u2(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u4(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
}

/// Modified UTF-8 (NUL as C0 80, surrogate pairs as two 3-byte forms).
fn mutf8(b: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            units.push(c as u16);
            i += 1;
        } else if c & 0xE0 == 0xC0 && i + 1 < b.len() {
            units.push(((c as u16 & 0x1F) << 6) | (b[i + 1] as u16 & 0x3F));
            i += 2;
        } else if c & 0xF0 == 0xE0 && i + 2 < b.len() {
            units.push(((c as u16 & 0x0F) << 12) | ((b[i + 1] as u16 & 0x3F) << 6) | (b[i + 2] as u16 & 0x3F));
            i += 3;
        } else {
            units.push(0xFFFD);
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

/// `java/lang/String` to `java.lang.String`.
fn dotted(s: &str) -> String {
    s.replace('/', ".")
}

/// One field descriptor at the start of `s`: the readable type and the bytes it used.
fn desc_one(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let mut dims = 0;
    while b.get(dims) == Some(&b'[') {
        dims += 1;
    }
    let (base, used) = match *b.get(dims)? {
        b'B' => ("byte".to_string(), 1),
        b'C' => ("char".into(), 1),
        b'D' => ("double".into(), 1),
        b'F' => ("float".into(), 1),
        b'I' => ("int".into(), 1),
        b'J' => ("long".into(), 1),
        b'S' => ("short".into(), 1),
        b'Z' => ("boolean".into(), 1),
        b'V' => ("void".into(), 1),
        b'L' => {
            let end = s[dims..].find(';')?;
            let name = dotted(&s[dims + 1..dims + end]);
            (name.strip_prefix("java.lang.").filter(|r| !r.contains('.')).map_or(name.clone(), str::to_string), end + 1)
        }
        _ => return None,
    };
    Some((format!("{base}{}", "[]".repeat(dims)), dims + used))
}

/// `(ILjava/lang/String;)V` to `(int, String) -> void`; unreadable descriptors come back as they are.
pub fn pretty_method(desc: &str) -> String {
    let Some(rest) = desc.strip_prefix('(') else { return desc.to_string() };
    let mut args = Vec::new();
    let mut s = rest;
    while !s.starts_with(')') {
        let Some((t, n)) = desc_one(s) else { return desc.to_string() };
        args.push(t);
        s = &s[n..];
    }
    match desc_one(&s[1..]) {
        Some((r, n)) if n == s.len() - 1 => format!("({}) -> {r}", args.join(", ")),
        _ => desc.to_string(),
    }
}

pub fn pretty_field(desc: &str) -> String {
    match desc_one(desc) {
        Some((t, n)) if n == desc.len() => t,
        _ => desc.to_string(),
    }
}

fn flags(v: u16, table: &[(u16, &str)]) -> String {
    table.iter().filter(|(b, _)| v & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join(" ")
}

const CLASS_FLAGS: &[(u16, &str)] = &[(1, "public"), (0x10, "final"), (0x400, "abstract"), (0x1000, "synthetic")];
const FIELD_FLAGS: &[(u16, &str)] = &[
    (1, "public"),
    (2, "private"),
    (4, "protected"),
    (8, "static"),
    (0x10, "final"),
    (0x40, "volatile"),
    (0x80, "transient"),
    (0x1000, "synthetic"),
];
const METHOD_FLAGS: &[(u16, &str)] = &[
    (1, "public"),
    (2, "private"),
    (4, "protected"),
    (8, "static"),
    (0x10, "final"),
    (0x20, "synchronized"),
    (0x40, "bridge"),
    (0x80, "varargs"),
    (0x100, "native"),
    (0x400, "abstract"),
    (0x1000, "synthetic"),
];

struct Pool(Vec<Cp>);

impl Pool {
    fn utf8(&self, i: u16) -> String {
        match self.0.get(i as usize) {
            Some(Cp::Utf8(s)) => s.clone(),
            _ => format!("#{i}?"),
        }
    }
    fn class(&self, i: u16) -> String {
        match self.0.get(i as usize) {
            Some(Cp::Class(n)) => {
                let n = self.utf8(*n);
                if n.starts_with('[') {
                    pretty_field(&n)
                } else {
                    dotted(&n)
                }
            }
            _ => format!("#{i}?"),
        }
    }
    fn name_type(&self, i: u16) -> (String, String) {
        match self.0.get(i as usize) {
            Some(Cp::NameType(n, t)) => (self.utf8(*n), self.utf8(*t)),
            _ => (format!("#{i}?"), String::new()),
        }
    }
    /// `(owner, name, raw descriptor)` of a member reference.
    fn member(&self, i: u16) -> Option<(String, String, String)> {
        match self.0.get(i as usize) {
            Some(Cp::Ref(c, nt)) => {
                let (n, t) = self.name_type(*nt);
                Some((self.class(*c), n, t))
            }
            _ => None,
        }
    }
    /// `owner.name(raw)` for a method reference, `owner.name` for a field.
    fn member_key(&self, i: u16) -> String {
        match self.member(i) {
            Some((o, n, t)) => format!("{o}.{n}{}", if t.starts_with('(') { t } else { String::new() }),
            None => format!("#{i}?"),
        }
    }
    fn member_text(&self, i: u16) -> String {
        match self.member(i) {
            Some((o, n, t)) if t.starts_with('(') => format!("{o}.{n}{}", pretty_method(&t)),
            Some((o, n, t)) => format!("{o}.{n}: {}", pretty_field(&t)),
            None => format!("#{i}?"),
        }
    }
}

fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars().take(160) {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    if s.chars().count() > 160 {
        o.push_str("...");
    }
    o.push('"');
    o
}

/// What a class file gives back: the type and the strings its code loads.
pub struct Parsed {
    pub ty: TypeDef,
    /// `(origin, text)`: strings loaded by code (origin names the method and offset) and pool strings
    /// no code loads (origin is the type).
    pub strings: Vec<(String, String)>,
    pub major: u16,
}

pub fn parse(data: &[u8]) -> Result<Parsed, String> {
    let mut r = R { d: data, p: 0 };
    if r.u4()? != 0xCAFE_BABE {
        return Err("not a class file (no CAFEBABE magic)".into());
    }
    let _minor = r.u2()?;
    let major = r.u2()?;
    let n = r.u2()? as usize;
    let mut cp = vec![Cp::None; n.max(1)];
    let mut i = 1;
    while i < n {
        let tag = r.u1()?;
        cp[i] = match tag {
            1 => {
                let l = r.u2()? as usize;
                Cp::Utf8(mutf8(r.take(l)?))
            }
            3 => Cp::Int(r.u4()? as i32),
            4 => Cp::Float(f32::from_bits(r.u4()?)),
            5 => Cp::Long(((r.u4()? as u64) << 32 | r.u4()? as u64) as i64),
            6 => Cp::Double(f64::from_bits((r.u4()? as u64) << 32 | r.u4()? as u64)),
            7 => Cp::Class(r.u2()?),
            8 => Cp::Str(r.u2()?),
            9..=11 => Cp::Ref(r.u2()?, r.u2()?),
            12 => Cp::NameType(r.u2()?, r.u2()?),
            15 => Cp::Handle(r.u1()?, r.u2()?),
            16 => Cp::MethodType(r.u2()?),
            17 | 18 => Cp::Dyn(r.u2()?, r.u2()?, tag == 18),
            19 | 20 => {
                r.u2()?;
                Cp::Other
            }
            t => return Err(format!("unknown constant-pool tag {t} at entry {i}")),
        };
        i += if matches!(tag, 5 | 6) { 2 } else { 1 };
    }
    let pool = Pool(cp);
    let access = r.u2()?;
    let this = pool.class(r.u2()?);
    let sup = r.u2()?;
    let extends = (sup != 0).then(|| pool.class(sup));
    let mut implements = Vec::new();
    for _ in 0..r.u2()? {
        implements.push(pool.class(r.u2()?));
    }
    let mut fields = Vec::new();
    for _ in 0..r.u2()? {
        let (acc, name, desc) = (r.u2()?, r.u2()?, r.u2()?);
        let mut constant = None;
        for _ in 0..r.u2()? {
            let (an, al) = (pool.utf8(r.u2()?), r.u4()? as usize);
            let body = r.take(al)?;
            if an == "ConstantValue" && body.len() == 2 {
                constant = Some(const_text(&pool, u16::from_be_bytes([body[0], body[1]])));
            }
        }
        fields.push(FieldDef {
            name: pool.utf8(name),
            ty: pretty_field(&pool.utf8(desc)),
            raw: pool.utf8(desc),
            flags: flags(acc, FIELD_FLAGS),
            constant,
        });
    }
    struct RawMethod {
        acc: u16,
        name: String,
        desc: String,
        code: Option<Vec<u8>>,
    }
    let mut raw_methods = Vec::new();
    for _ in 0..r.u2()? {
        let (acc, name, desc) = (r.u2()?, r.u2()?, r.u2()?);
        let mut code = None;
        for _ in 0..r.u2()? {
            let (an, al) = (pool.utf8(r.u2()?), r.u4()? as usize);
            let body = r.take(al)?;
            if an == "Code" {
                code = Some(body.to_vec());
            }
        }
        raw_methods.push(RawMethod { acc, name: pool.utf8(name), desc: pool.utf8(desc), code });
    }
    let mut bootstrap: Vec<(u16, Vec<u16>)> = Vec::new();
    let mut source_file = None;
    for _ in 0..r.u2()? {
        let (an, al) = (pool.utf8(r.u2()?), r.u4()? as usize);
        let body = r.take(al)?;
        let mut b = R { d: body, p: 0 };
        match an.as_str() {
            "SourceFile" => source_file = b.u2().ok().map(|i| pool.utf8(i)),
            "BootstrapMethods" => {
                if let Ok(c) = b.u2() {
                    for _ in 0..c {
                        let (Ok(m), Ok(na)) = (b.u2(), b.u2()) else { break };
                        let args: Vec<u16> = (0..na).filter_map(|_| b.u2().ok()).collect();
                        bootstrap.push((m, args));
                    }
                }
            }
            _ => {}
        }
    }
    let is_iface = access & 0x200 != 0;
    let kind = if access & 0x2000 != 0 {
        "annotation"
    } else if is_iface {
        "interface"
    } else if access & 0x4000 != 0 {
        "enum"
    } else if access & 0x8000 != 0 {
        "module"
    } else {
        "class"
    };
    let mut strings = Vec::new();
    let mut used: HashSet<u16> = HashSet::new();
    let mut methods = Vec::new();
    for m in raw_methods {
        let key = format!("{this}.{}{}", m.name, m.desc);
        let code = m.code.as_ref().map(|c| decode_code(&pool, c, &bootstrap, &this, &m.name, &mut used, &mut strings));
        methods.push(MethodDef {
            name: m.name.clone(),
            sig: pretty_method(&m.desc),
            raw: m.desc.clone(),
            flags: flags(m.acc, METHOD_FLAGS),
            key,
            code,
        });
    }
    for (i, e) in pool.0.iter().enumerate() {
        if let Cp::Str(u) = e {
            if !used.contains(&(i as u16)) {
                strings.push((this.clone(), pool.utf8(*u)));
            }
        }
    }
    let mut cf = flags(access, CLASS_FLAGS);
    if let Some(s) = &source_file {
        cf = format!("{cf} source={s}").trim().to_string();
    }
    let mut ty = TypeDef {
        name: this,
        kind: kind.into(),
        flags: cf,
        extends,
        implements,
        fields,
        methods,
        origin: String::new(),
    };
    ty.methods.shrink_to_fit();
    Ok(Parsed { ty, strings, major })
}

fn const_text(pool: &Pool, i: u16) -> String {
    match pool.0.get(i as usize) {
        Some(Cp::Int(v)) => v.to_string(),
        Some(Cp::Long(v)) => format!("{v}L"),
        Some(Cp::Float(v)) => format!("{v}f"),
        Some(Cp::Double(v)) => format!("{v}d"),
        Some(Cp::Str(u)) => quote(&pool.utf8(*u)),
        Some(Cp::Class(_)) => format!("{}.class", pool.class(i)),
        Some(Cp::MethodType(d)) => format!("methodtype {}", pretty_method(&pool.utf8(*d))),
        Some(Cp::Handle(k, r)) => format!("handle({k}) {}", pool.member_key(*r)),
        Some(Cp::Dyn(b, nt, _)) => format!("dynamic[{b}] {}", pool.name_type(*nt).0),
        _ => format!("#{i}?"),
    }
}

/// The bootstrap method of an invokedynamic, with its static arguments (a string-concat recipe shows here).
fn indy_text(
    pool: &Pool,
    bsm: &[(u16, Vec<u16>)],
    b: u16,
    nt: u16,
    used: &mut HashSet<u16>,
    strs: &mut Vec<u16>,
) -> String {
    let (name, desc) = pool.name_type(nt);
    let mut s = format!("{name}{}", pretty_method(&desc));
    match bsm.get(b as usize) {
        Some((m, args)) => {
            let h = match pool.0.get(*m as usize) {
                Some(Cp::Handle(_, r)) => pool.member_key(*r),
                _ => format!("#{m}?"),
            };
            let h = h.split('(').next().unwrap_or("").to_string();
            let a: Vec<String> = args
                .iter()
                .map(|&x| {
                    if let Some(Cp::Str(_)) = pool.0.get(x as usize) {
                        used.insert(x);
                        strs.push(x);
                    }
                    const_text(pool, x)
                })
                .collect();
            s.push_str(&format!("  via {h}"));
            if !a.is_empty() {
                s.push_str(&format!(" [{}]", a.join(", ")));
            }
        }
        None => s.push_str(&format!("  via bootstrap #{b} (missing)")),
    }
    s
}

fn decode_code(
    pool: &Pool,
    attr: &[u8],
    bsm: &[(u16, Vec<u16>)],
    this: &str,
    mname: &str,
    used: &mut HashSet<u16>,
    strings: &mut Vec<(String, String)>,
) -> Code {
    let mut c = Code::default();
    let mut r = R { d: attr, p: 0 };
    let head = (|| -> Result<(u16, u16, &[u8]), String> {
        let (ms, ml) = (r.u2()?, r.u2()?);
        let n = r.u4()? as usize;
        Ok((ms, ml, r.take(n)?))
    })();
    let (ms, ml, code) = match head {
        Ok(h) => h,
        Err(e) => {
            c.note = Some(format!("Code attribute damaged: {e}"));
            return c;
        }
    };
    c.size = code.len();
    c.info = format!("stack {ms}, locals {ml}");
    // exception table and line numbers
    let mut lines: Vec<(u16, u16)> = Vec::new();
    let tail = (|| -> Result<(), String> {
        for _ in 0..r.u2()? {
            let (s, e, h, t) = (r.u2()?, r.u2()?, r.u2()?, r.u2()?);
            let ty = if t == 0 { "any".to_string() } else { pool.class(t) };
            c.handlers.push(format!("catch {s:#06x}..{e:#06x} -> {h:#06x} {ty}"));
        }
        for _ in 0..r.u2()? {
            let (an, al) = (pool.utf8(r.u2()?), r.u4()? as usize);
            let body = r.take(al)?;
            if an == "LineNumberTable" {
                let mut b = R { d: body, p: 0 };
                for _ in 0..b.u2()? {
                    lines.push((b.u2()?, b.u2()?));
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = tail {
        c.note = Some(format!("exception or line table damaged: {e}"));
    }
    let names: Vec<&str> = NAMES.split_whitespace().collect();
    let line_at = |off: usize| lines.iter().find(|(pc, _)| *pc as usize == off).map(|(_, l)| *l as u32);
    let mut p = 0usize;
    while p < code.len() {
        let off = p;
        let op = code[p];
        let mut ins = Insn { off: off as u32, line: line_at(off), ..Insn::default() };
        let Some(&mn) = names.get(op as usize) else {
            ins.text = format!("?? unknown opcode {op:#04x}");
            c.insns.push(ins);
            c.note = Some(format!("decoding stopped at {off:#06x}: unknown opcode {op:#04x}"));
            return c;
        };
        let res = decode_one(pool, bsm, code, off, op, &mut ins, used);
        let (len, strs) = match res {
            Ok(v) => v,
            Err(e) => {
                ins.text = format!("?? {mn} {e}");
                c.insns.push(ins);
                c.note = Some(format!("decoding stopped at {off:#06x}: {e}"));
                return c;
            }
        };
        for s in strs {
            strings.push((format!("{this}.{mname} +{off:#06x}"), s));
        }
        p += len;
        c.insns.push(ins);
    }
    c
}

fn be16(c: &[u8], p: usize) -> Result<u16, String> {
    c.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or_else(|| "operand cut short".to_string())
}
fn be32(c: &[u8], p: usize) -> Result<i32, String> {
    c.get(p..p + 4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| "operand cut short".to_string())
}

fn target(code: &[u8], off: usize, rel: i64) -> String {
    let t = off as i64 + rel;
    if t < 0 || t as usize >= code.len() {
        format!("{t:#06x} (outside the method)")
    } else {
        format!("{t:#06x}")
    }
}

/// Fill `ins.text` (and `callee`, `cont`) for the instruction at `off`; returns its length and the
/// strings it loads.
fn decode_one(
    pool: &Pool,
    bsm: &[(u16, Vec<u16>)],
    code: &[u8],
    off: usize,
    op: u8,
    ins: &mut Insn,
    used: &mut HashSet<u16>,
) -> Result<(usize, Vec<String>), String> {
    let mut strs = Vec::new();
    let b1 = |i: usize| code.get(off + i).copied().ok_or_else(|| "operand cut short".to_string());
    let len;
    let mn = NAMES.split_whitespace().nth(op as usize).unwrap_or("?");
    let mut text = mn.to_string();
    match op {
        16 => {
            text += &format!(" {}", b1(1)? as i8);
            len = 2;
        }
        17 => {
            text += &format!(" {}", be16(code, off + 1)? as i16);
            len = 3;
        }
        18..=20 => {
            let (i, l) = if op == 18 { (b1(1)? as u16, 2) } else { (be16(code, off + 1)?, 3) };
            len = l;
            if let Some(Cp::Str(u)) = pool.0.get(i as usize) {
                used.insert(i);
                strs.push(pool.utf8(*u));
            }
            text += &format!(" {}", const_text(pool, i));
        }
        21..=25 | 54..=58 | 169 => {
            text += &format!(" {}", b1(1)?);
            len = 2;
        }
        132 => {
            text += &format!(" {} {}", b1(1)?, b1(2)? as i8);
            len = 3;
        }
        153..=168 | 198 | 199 => {
            text += &format!(" -> {}", target(code, off, be16(code, off + 1)? as i16 as i64));
            len = 3;
        }
        200 | 201 => {
            text += &format!(" -> {}", target(code, off, be32(code, off + 1)? as i64));
            len = 5;
        }
        170 | 171 => {
            let mut p = (off + 4) & !3;
            let def = be32(code, p)?;
            let mut cases: Vec<String> = Vec::new();
            if op == 170 {
                let (lo, hi) = (be32(code, p + 4)?, be32(code, p + 8)?);
                if hi < lo || (hi as i64 - lo as i64) > 100_000 {
                    return Err(format!("switch range {lo}..{hi} is not sane"));
                }
                p += 12;
                for k in lo as i64..=hi as i64 {
                    cases.push(format!("{k} -> {}", target(code, off, be32(code, p)? as i64)));
                    p += 4;
                }
            } else {
                let n = be32(code, p + 4)?;
                if !(0..=100_000).contains(&n) {
                    return Err(format!("switch with {n} cases is not sane"));
                }
                p += 8;
                for _ in 0..n {
                    cases.push(format!("{} -> {}", be32(code, p)?, target(code, off, be32(code, p + 4)? as i64)));
                    p += 8;
                }
            }
            let def_t = target(code, off, def as i64);
            if cases.len() <= 4 {
                text += &format!(" {{{}; default -> {def_t}}}", cases.join(", "));
            } else {
                text += &format!(" {} cases, default -> {def_t}", cases.len());
                ins.cont = cases.into_iter().map(|c| format!("case {c}")).collect();
            }
            len = p - off;
        }
        178..=185 => {
            let i = be16(code, off + 1)?;
            text += &format!(" {}", pool.member_text(i));
            if matches!(pool.0.get(i as usize), Some(Cp::Ref(..))) {
                if op >= 182 {
                    ins.callee = Some(pool.member_key(i));
                }
            } else {
                return Err(format!("operand #{i} is not a member reference"));
            }
            len = if op == 185 { 5 } else { 3 };
        }
        186 => {
            let i = be16(code, off + 1)?;
            match pool.0.get(i as usize) {
                Some(Cp::Dyn(b, nt, true)) => {
                    let mut ss = Vec::new();
                    text += &format!(" {}", indy_text(pool, bsm, *b, *nt, used, &mut ss));
                    for x in ss {
                        strs.push(pool.utf8(match pool.0.get(x as usize) {
                            Some(Cp::Str(u)) => *u,
                            _ => 0,
                        }));
                    }
                }
                _ => return Err(format!("operand #{i} is not an invokedynamic entry")),
            }
            len = 5;
        }
        187 | 189 | 192 | 193 => {
            text += &format!(" {}", pool.class(be16(code, off + 1)?));
            len = 3;
        }
        188 => {
            let t = match b1(1)? {
                4 => "boolean",
                5 => "char",
                6 => "float",
                7 => "double",
                8 => "byte",
                9 => "short",
                10 => "int",
                11 => "long",
                _ => "?",
            };
            text += &format!(" {t}");
            len = 2;
        }
        197 => {
            text += &format!(" {} dims {}", pool.class(be16(code, off + 1)?), b1(3)?);
            len = 4;
        }
        196 => {
            let inner = b1(1)?;
            let n = *NAMES.split_whitespace().collect::<Vec<_>>().get(inner as usize).unwrap_or(&"?");
            if inner == 132 {
                text = format!("wide iinc {} {}", be16(code, off + 2)?, be16(code, off + 4)? as i16);
                len = 6;
            } else if matches!(inner, 21..=25 | 54..=58 | 169) {
                text = format!("wide {n} {}", be16(code, off + 2)?);
                len = 4;
            } else {
                return Err(format!("wide before opcode {inner:#04x}"));
            }
        }
        _ => len = 1,
    }
    if off + len > code.len() {
        return Err("operand cut short".into());
    }
    ins.text = text;
    Ok((len, strs))
}
