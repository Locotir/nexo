//! Calls into libraries (`ltrace`) and declared hooks on functions (`hook`): a breakpoint at the
//! entry reads the arguments with their types, a breakpoint at the return address reads the result.
//! A call is written down when it returns; one that never returned says so.

use std::collections::BTreeMap;

use nexo_bin::{Output, Place, Proto};
use serde_json::json;

use crate::callargs::{conversions, first_stack_arg, format_kind, int_regs, Abi, Class, Conv, Loc, Sizes};
use crate::cmds::Args;
use crate::engine::{rows, Session, Why};
use crate::platform::{Os, Tid};
use crate::regs::Regs;
use crate::trace::Kind;
use crate::vector::{f64_to_f80, f80_to_f64, float_text, parse_float, Vector};
use crate::{fail, usage, Result};

/// Calls in flight at once (deeper recursion is not followed to its return).
const PENDING_MAX: usize = 4096;
/// Arguments read off a format string at most; a longer list ends in `...`.
const VARARGS_MAX: usize = 24;
/// Bytes of a format string read at most.
const FORMAT_MAX: usize = 1024;

/// A library function as the program reaches it.
#[derive(Debug, Clone)]
pub struct Site {
    pub name: String,
    /// `None`: a stub entered by a call. `Some(len)`: a `call [slot]` of this length.
    pub call_len: Option<u8>,
}

/// `ltrace` is on: the sites it watches (memory address) and the names it keeps.
#[derive(Debug, Clone, Default)]
pub struct Lib {
    pub sites: BTreeMap<u64, Site>,
    pub names: Vec<String>,
}

/// A declared hook.
#[derive(Debug, Clone)]
pub struct Hook {
    pub id: u32,
    /// Memory address of the entry.
    pub addr: u64,
    pub name: String,
    pub proto: Option<Proto>,
    /// The hook sits on a `call [slot]` and not on the function.
    pub call_len: Option<u8>,
    pub bt: usize,
    pub stop: bool,
    /// (argument position, value) set at the entry.
    pub set: Vec<(usize, u64)>,
    /// The value the function returns instead of its own.
    pub ret: Option<u64>,
    pub hits: u64,
    pub returns: u64,
}

/// A call whose entry was seen, waiting for its return.
#[derive(Debug, Clone)]
pub struct Pending {
    pub tid: Tid,
    pub ret_addr: u64,
    /// The stack pointer right after the return.
    pub sp_after: u64,
    pub kind: Kind,
    pub hook: Option<u32>,
    pub name: String,
    pub text: String,
    pub ret_ty: Option<String>,
}

fn is_float(ty: &str) -> bool {
    let t = ty.trim();
    !t.contains('*') && (t.contains("double") || t.contains("float"))
}

/// The kinds of floating point value. A value of each is handed around as bits: a `float`'s 32, the others as a
/// double's 64 (an x87 `long double` is read as the nearest double).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fp {
    Single,
    Double,
    Extended,
}

fn fp(ty: &str) -> Option<Fp> {
    if !is_float(ty) {
        None
    } else if ty.contains("long") {
        Some(Fp::Extended)
    } else if ty.contains("double") {
        Some(Fp::Double)
    } else {
        Some(Fp::Single)
    }
}

/// How an argument of type `ty` travels in a 64-bit call. Win64 has no x87 `long double`: MSVC's is a double.
fn class_of(ty: &str, os: Os) -> Class {
    match fp(ty) {
        None => Class::Int,
        Some(Fp::Extended) if os != Os::Windows => Class::X87,
        Some(_) => Class::Float,
    }
}

/// A float written by a person as the bits a value of type `ty` is handed around as.
fn float_bits(ty: &str, s: &str) -> Option<u64> {
    let x = parse_float(s)?;
    Some(if fp(ty) == Some(Fp::Single) { u64::from((x as f32).to_bits()) } else { x.to_bits() })
}

/// Bits as `float_bits` gives them, as the nearest double.
fn bits_f64(ty: &str, v: u64) -> f64 {
    if fp(ty) == Some(Fp::Single) {
        f64::from(f32::from_bits(v as u32))
    } else {
        f64::from_bits(v)
    }
}

/// The value in `b` as `typed` takes it: up to 8 little-endian bytes, or a 10-byte x87 value as a double's bits.
fn value_of(b: &[u8]) -> u64 {
    if let Ok(x) = <[u8; 10]>::try_from(&b[..b.len().min(10)]) {
        return f80_to_f64(x).to_bits();
    }
    let mut w = [0u8; 8];
    let n = b.len().min(8);
    w[..n].copy_from_slice(&b[..n]);
    u64::from_le_bytes(w)
}

/// The low `bytes` of `v` as a signed or an unsigned number.
fn int_text(v: u64, bytes: u32, unsigned: bool) -> String {
    let mask = if bytes >= 8 { u64::MAX } else { (1u64 << (bytes * 8)) - 1 };
    let x = v & mask;
    if unsigned {
        x.to_string()
    } else {
        let shift = 64 - bytes.min(8) * 8;
        (((x << shift) as i64) >> shift).to_string()
    }
}

/// `name=value` for a hook, `value` for the library trace.
fn label(named: bool, name: &str, shown: String) -> String {
    if named && !name.is_empty() {
        format!("{name}={shown}")
    } else {
        shown
    }
}

/// Where one 64-bit call's arguments are read from: its registers, its vector registers (read the first time an
/// argument needs them) and its first stack argument.
struct Entry<'a> {
    tid: Tid,
    r: &'a Regs,
    stack: u64,
    vec: Option<Option<Vector>>,
}

/// Where argument `n` of a 64-bit call is, with its class and its declared type (`None` past the prototype's
/// arguments or with none, where it is taken as an integer of the `...`).
fn arg_place(proto: Option<&Proto>, n: usize, os: Os) -> (Class, Loc, Option<&str>) {
    let mut abi = Abi::new(os);
    let mut at = (Class::Int, Loc::Gp(0), None);
    for i in 0..=n {
        let ty = proto.and_then(|p| p.args.get(i)).map(|(t, _)| t.as_str());
        let class = ty.map_or(Class::Int, |t| class_of(t, os));
        at = (class, abi.next(class, ty.is_none()), ty);
    }
    at
}

fn is_ptr(ty: &str) -> bool {
    ty.contains('*')
}

fn is_text(ty: &str) -> bool {
    let t = ty.replace(' ', "");
    t == "constchar*" || t == "charconst*"
}

/// Integer types by how many bytes they have and whether they carry a sign; `x32`: in a 32-bit program, where
/// `long`, `size_t` and `intptr_t` have 4.
fn int_kind(ty: &str, x32: bool) -> (u32, bool) {
    let t = ty.trim();
    let unsigned = t.contains("unsigned") || t.starts_with("size_t") || t.starts_with("uint") || t == "bool";
    let wide = t.contains("long long") || t.contains("64");
    let bytes = if wide || !x32 && (t.contains("long") || t.contains("size_t") || t == "intptr_t") {
        8
    } else if t.contains("short") || t.contains("16") {
        2
    } else if t == "char" || t.contains("int8") || t == "bool" || t.ends_with(" char") {
        1
    } else {
        4
    };
    (bytes, unsigned)
}

/// An 8-byte integer, which a 32-bit program returns in edx:eax.
fn wide32(ty: Option<&str>) -> bool {
    ty.is_some_and(|t| !is_ptr(t) && !is_float(t) && int_kind(t, true).0 == 8)
}

/// The bytes a 32-bit program's caller pushes for an argument of type `ty`: a whole number of 4-byte slots.
fn pushed_size(ty: &str) -> u64 {
    let t = ty.trim();
    let n = if is_ptr(t) {
        4
    } else if is_float(t) {
        if t.contains("long") {
            12
        } else if t.contains("double") {
            8
        } else {
            4
        }
    } else {
        u64::from(int_kind(t, true).0)
    };
    n.max(4)
}

impl Session {
    /// A value of type `ty` as a person reads it. Text is read from memory now; a float comes as its bits.
    fn typed(&mut self, ty: &str, v: u64) -> String {
        if is_float(ty) {
            return float_text(bits_f64(ty, v));
        }
        if is_text(ty) {
            if v == 0 {
                return "NULL".into();
            }
            return self.cstr(v);
        }
        if is_ptr(ty) {
            return if v == 0 { "NULL".into() } else { format!("{v:#x}") };
        }
        let (bytes, unsigned) = int_kind(ty, self.bits() == 32);
        int_text(v, bytes, unsigned)
    }

    /// A value a format string's conversion took, as `printf` would show it.
    fn conv_text(&mut self, c: Conv, v: u64) -> String {
        match c {
            Conv::Int { bytes, hex: true, .. } => {
                format!("{:#x}", v & if bytes >= 8 { u64::MAX } else { (1u64 << (bytes * 8)) - 1 })
            }
            Conv::Int { bytes, signed, .. } => int_text(v, bytes, !signed),
            Conv::Char if (v as u8).is_ascii_graphic() || v as u8 == b' ' => format!("'{}'", v as u8 as char),
            Conv::Char => int_text(v, 4, false),
            Conv::Text => self.typed("const char *", v),
            Conv::Ptr if v == 0 => "NULL".into(),
            Conv::Ptr => format!("{v:#x}"),
            Conv::Double | Conv::LongDouble => float_text(f64::from_bits(v)),
        }
    }

    /// The vector registers of `tid`, `None` where the target gives none (a core file, a remote stub).
    pub(crate) fn vector(&mut self, tid: Tid) -> Option<Vector> {
        let os = self.t.os();
        Vector::new(os, self.t.vector_state(tid)?)
    }

    /// `n` bytes of the argument at `loc`: of an integer register (at most 8), the low ones of an xmm register, or
    /// of the stack.
    fn arg_bytes(&mut self, e: &mut Entry, loc: Loc, n: usize) -> Option<Vec<u8>> {
        match loc {
            Loc::Gp(i) => Some(e.r.get(int_regs(self.t.os()).get(i)?)?.to_le_bytes()[..n.min(8)].to_vec()),
            Loc::Xmm(i) => {
                if e.vec.is_none() {
                    e.vec = Some(self.vector(e.tid));
                }
                Some(e.vec.as_ref()?.as_ref()?.xmm(i)[..n.min(16)].to_vec())
            }
            Loc::Stack(off) => {
                let b = self.read_vec(e.stack + off, n);
                (b.len() == n).then_some(b)
            }
        }
    }

    /// The argument of type `ty` (taken as an integer when `None`) at `loc`, as `typed` takes it.
    fn arg_value(&mut self, e: &mut Entry, ty: Option<&str>, class: Class, loc: Loc) -> Option<u64> {
        let n = match (class, ty.and_then(fp)) {
            (Class::X87, _) => 10,
            (_, Some(Fp::Single)) => 4,
            _ => 8,
        };
        Some(value_of(&self.arg_bytes(e, loc, n)?))
    }

    /// What an argument that could not be read says instead of its value.
    fn unread(e: &Entry, loc: Loc) -> String {
        match loc {
            Loc::Stack(off) => format!("(its stack slot {:#x} cannot be read)", e.stack + off),
            Loc::Xmm(i) => format!("(in xmm{i}; this target gives no vector registers)"),
            Loc::Gp(_) => "?".into(),
        }
    }

    /// The `...` of a call to `name`, read off its format string `fmt` (the last `const char *` argument) when it is a
    /// printf or scanf one; `next` reads each value in turn (`None`: it could not be read). Else `...`.
    fn varargs(
        &mut self,
        name: &str,
        fmt: Option<u64>,
        sz: Sizes,
        next: &mut dyn FnMut(&mut Self, Conv) -> Option<u64>,
    ) -> Vec<String> {
        let (Some(scan), Some(f)) = (format_kind(name), fmt.filter(|&f| f != 0)) else { return vec!["...".into()] };
        let mut text = self.read_vec(f, FORMAT_MAX);
        let ended = text.iter().position(|&b| b == 0).map(|i| text.truncate(i)).is_some();
        let (convs, known) = conversions(&text, scan, sz);
        let mut out = Vec::new();
        for &c in convs.iter().take(VARARGS_MAX) {
            out.push(match next(self, c) {
                Some(v) => self.conv_text(c, v),
                None => "?".into(),
            });
        }
        if !ended || !known || convs.len() > VARARGS_MAX {
            out.push("...".into());
        }
        out
    }

    /// `name(arg, ...)` from the registers and the stack at the entry. `named`: `x=12` instead of `12`. `call_len`:
    /// where the thread is, as a hook's site says (`None`: at the function's entry).
    fn call_args(
        &mut self,
        tid: Tid,
        name: &str,
        proto: Option<&Proto>,
        r: &Regs,
        named: bool,
        call_len: Option<u8>,
    ) -> (String, Option<String>) {
        if r.x32 {
            return self.pushed_args(name, proto, r, named, call_len);
        }
        let os = self.t.os();
        let regs = int_regs(os);
        let reg = |i: usize| regs.get(i).and_then(|n| r.get(n));
        if let Some(p) = proto {
            let mut e = Entry { tid, r, stack: first_stack_arg(os, r.rsp(), call_len.is_some()), vec: None };
            let mut abi = Abi::new(os);
            let (mut parts, mut fmt) = (Vec::new(), None);
            for (ty, an) in &p.args {
                let class = class_of(ty, os);
                let loc = abi.next(class, false);
                let v = self.arg_value(&mut e, Some(ty), class, loc);
                if is_text(ty) {
                    fmt = v;
                }
                let shown = match v {
                    Some(v) => self.typed(ty, v),
                    None => Self::unread(&e, loc),
                };
                parts.push(label(named, an, shown));
            }
            if p.variadic {
                let sz = Sizes { long: if os == Os::Windows { 4 } else { 8 }, ptr: 8 };
                let mut next = |s: &mut Self, c: Conv| {
                    let class = match c {
                        Conv::Double => Class::Float,
                        Conv::LongDouble => class_of("long double", os),
                        _ => Class::Int,
                    };
                    let loc = abi.next(class, true);
                    s.arg_value(&mut e, None, class, loc)
                };
                parts.extend(self.varargs(name, fmt, sz, &mut next));
            }
            return (format!("{name}({})", parts.join(", ")), Some(p.ret.clone()));
        }
        if let Some((n, _)) = nexo_bin::arg_count(name) {
            let v: Vec<String> = (0..n).map(|i| reg(i).map(|x| format!("{x:#x}")).unwrap_or("?".into())).collect();
            return (format!("{name}({})", v.join(", ")), None);
        }
        let v: Vec<String> = (0..3).map(|i| format!("{}={:#x}", regs[i], reg(i).unwrap_or(0))).collect();
        (format!("{name}({}; no prototype)", v.join(", ")), None)
    }

    /// Where a 32-bit program's first argument is (cdecl: the caller pushed them, the first one lowest): past the
    /// return address at the function's entry, on top of the stack on the call instruction.
    fn first_pushed(r: &Regs, call_len: Option<u8>) -> u64 {
        r.rsp() + if call_len.is_none() { 4 } else { 0 }
    }

    /// `n` bytes (at most 8) of the program's memory at `at`, as a little-endian number.
    fn word_at(&mut self, at: u64, n: usize) -> Option<u64> {
        let mut b = [0u8; 8];
        let n = n.min(8);
        (self.t.read(at, &mut b[..n]) == n).then(|| u64::from_le_bytes(b))
    }

    /// The pushed argument of type `ty` at `at`, as `typed` takes it: the bytes of its slot, an x87 `long double`
    /// read from the first 10 of its 12.
    fn pushed_value(&mut self, at: u64, ty: &str) -> Option<u64> {
        let n = if fp(ty) == Some(Fp::Extended) { 10 } else { pushed_size(ty).min(8) as usize };
        let b = self.read_vec(at, n);
        (b.len() == n).then(|| value_of(&b))
    }

    /// `call_args` for a 32-bit thread: each argument read from the stack slot its caller pushed.
    fn pushed_args(
        &mut self,
        name: &str,
        proto: Option<&Proto>,
        r: &Regs,
        named: bool,
        call_len: Option<u8>,
    ) -> (String, Option<String>) {
        let first = Self::first_pushed(r, call_len);
        if let Some(p) = proto {
            let (mut parts, mut at, mut fmt) = (Vec::new(), first, None);
            for (ty, an) in &p.args {
                let v = self.pushed_value(at, ty);
                if is_text(ty) {
                    fmt = v;
                }
                let shown = match v {
                    Some(v) => self.typed(ty, v),
                    None => format!("(its stack slot {at:#x} cannot be read)"),
                };
                at += pushed_size(ty);
                parts.push(label(named, an, shown));
            }
            if p.variadic {
                // the `...` follow in slots of their own size: a double 8 bytes, a long double 12, a long long 8
                let mut next = |s: &mut Self, c: Conv| {
                    let ty = match c {
                        Conv::Double => "double",
                        Conv::LongDouble => "long double",
                        Conv::Int { bytes: 8, .. } => "long long",
                        _ => "int",
                    };
                    let v = s.pushed_value(at, ty);
                    at += pushed_size(ty);
                    v
                };
                parts.extend(self.varargs(name, fmt, Sizes { long: 4, ptr: 4 }, &mut next));
            }
            return (format!("{name}({})", parts.join(", ")), Some(p.ret.clone()));
        }
        let word = |s: &mut Self, i: u64| s.word_at(first + 4 * i, 4).map_or("?".into(), |x| format!("{x:#x}"));
        if let Some((n, _)) = nexo_bin::arg_count(name) {
            let v: Vec<String> = (0..n as u64).map(|i| word(self, i)).collect();
            return (format!("{name}({})", v.join(", ")), None);
        }
        let off = first - r.rsp();
        let v: Vec<String> = (0..3u64).map(|i| format!("[esp+{}]={}", off + 4 * i, word(self, i))).collect();
        (format!("{name}({}; no prototype)", v.join(", ")), None)
    }

    /// What a function returned, as `typed` takes it: rax (eax, or edx:eax for an 8-byte value, in a 32-bit thread);
    /// a float from xmm0, or from st0 for an x87 `long double` and in a 32-bit thread. `Err`: why it cannot be read.
    fn returned(&mut self, tid: Tid, ty: Option<&str>, r: &Regs) -> std::result::Result<u64, &'static str> {
        let Some(f) = ty.and_then(fp) else {
            if !r.x32 {
                return Ok(r.gp[0]);
            }
            let low = r.gp[0] & 0xffff_ffff;
            return Ok(if wide32(ty) { low | (r.gp[3] & 0xffff_ffff) << 32 } else { low });
        };
        let v = self.vector(tid).ok_or("this target gives no vector registers")?;
        if r.x32 || f == Fp::Extended && self.t.os() != Os::Windows {
            let x = v.st(0).ok_or("the x87 stack is empty")?;
            return Ok(if f == Fp::Single { u64::from((x as f32).to_bits()) } else { x.to_bits() });
        }
        let n = if f == Fp::Single { 4 } else { 8 };
        Ok(value_of(&v.xmm(0)[..n]))
    }

    /// Make the function return `v` (as `returned` gives it): in rax, edx:eax, xmm0 or st0.
    fn set_returned(&mut self, tid: Tid, ty: Option<&str>, r: &mut Regs, v: u64) -> Result<()> {
        let Some(f) = ty.and_then(fp) else {
            if r.x32 {
                // eax, and edx for the high half of an 8-byte value
                r.gp[0] = v & 0xffff_ffff;
                if wide32(ty) {
                    r.gp[3] = v >> 32;
                }
            } else {
                r.gp[0] = v;
            }
            return self.t.set_regs(tid, r);
        };
        let Some(mut vec) = self.vector(tid) else { return fail("this target gives no vector registers") };
        if r.x32 || f == Fp::Extended && self.t.os() != Os::Windows {
            if !vec.set_st0(bits_f64(ty.unwrap_or("double"), v)) {
                return fail("the x87 stack is empty: there is no st0 to change");
            }
        } else {
            let mut x = vec.xmm(0);
            let n = if f == Fp::Single { 4 } else { 8 };
            x[..n].copy_from_slice(&v.to_le_bytes()[..n]);
            vec.set_xmm(0, x);
        }
        self.t.set_vector_state(tid, vec.bytes())
    }

    fn ret_text(&mut self, ty: Option<&str>, rax: u64) -> String {
        match ty.map(str::trim) {
            Some("void") => "(void)".into(),
            Some(t) if is_ptr(t) && t.replace(' ', "").ends_with("char*") && rax != 0 => {
                format!("{rax:#x} {}", self.cstr(rax))
            }
            Some(t) => self.typed(t, rax),
            None => format!("{rax:#x}"),
        }
    }

    /// Set argument `n` of a call stopped at its entry to `v` (as `typed` takes it): its register, its xmm register
    /// or its stack slot. The text says what was done.
    fn set_arg(
        &mut self,
        tid: Tid,
        proto: Option<&Proto>,
        r: &mut Regs,
        call_len: Option<u8>,
        n: usize,
        v: u64,
    ) -> String {
        let ty = proto.and_then(|p| p.args.get(n)).map(|(t, _)| t.as_str());
        let shown = match ty {
            Some(t) if is_float(t) => float_text(bits_f64(t, v)),
            _ => format!("{v:#x}"),
        };
        let wrote = if r.x32 {
            let at = Self::first_pushed(r, call_len)
                + match proto {
                    Some(p) => {
                        p.args.iter().take(n).map(|(t, _)| pushed_size(t)).sum::<u64>()
                            + 4 * n.saturating_sub(p.args.len()) as u64
                    }
                    None => 4 * n as u64,
                };
            let b = match ty.and_then(fp) {
                Some(Fp::Extended) => f64_to_f80(f64::from_bits(v)).to_vec(),
                _ => v.to_le_bytes()[..ty.map_or(4, |t| pushed_size(t).min(8) as usize)].to_vec(),
            };
            self.t.write(at, &b).map_err(|_| format!("its stack slot {at:#x} cannot be written"))
        } else {
            let os = self.t.os();
            let (class, loc, ty) = arg_place(proto, n, os);
            let size = if ty.and_then(fp) == Some(Fp::Single) { 4 } else { 8 };
            match loc {
                Loc::Gp(i) => {
                    let name = int_regs(os)[i];
                    r.set(name, v);
                    Ok(())
                }
                Loc::Xmm(i) => match self.vector(tid) {
                    Some(mut vec) => {
                        let mut x = vec.xmm(i);
                        x[..size].copy_from_slice(&v.to_le_bytes()[..size]);
                        vec.set_xmm(i, x);
                        self.t.set_vector_state(tid, vec.bytes()).map_err(|e| e.to_string())
                    }
                    None => Err(format!("it is in xmm{i} and this target gives no vector registers")),
                },
                Loc::Stack(off) => {
                    let at = first_stack_arg(os, r.rsp(), call_len.is_some()) + off;
                    let b = if class == Class::X87 {
                        f64_to_f80(f64::from_bits(v)).to_vec()
                    } else {
                        v.to_le_bytes()[..size].to_vec()
                    };
                    self.t.write(at, &b).map_err(|_| format!("its stack slot {at:#x} cannot be written"))
                }
            }
        };
        match wrote {
            Ok(()) => format!("arg{n}={shown}"),
            Err(why) => format!("arg{n} not set: {why}"),
        }
    }

    /// Entry and return addresses the trace and the hooks need in memory now.
    pub(crate) fn hook_addrs(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.tr.pending.iter().map(|p| p.ret_addr).collect();
        if let Some(l) = &self.tr.lib {
            v.extend(l.sites.keys());
        }
        v.extend(self.tr.hooks.iter().map(|h| h.addr));
        v.extend(self.cover_addrs());
        v
    }

    /// A thread is at an address the trace or a hook watches. `Some`: a hook stops the program.
    pub(crate) fn hook_hit(&mut self, tid: Tid, addr: u64) -> Result<Option<(Tid, Why)>> {
        let mut verdict = None;
        self.cover_hit(addr);
        // a return first: the same address may also be an entry
        if self.tr.pending.iter().any(|p| p.tid == tid && p.ret_addr == addr) {
            let mut r = self.t.regs(tid)?;
            let sp = r.rsp();
            if let Some(i) =
                self.tr.pending.iter().rposition(|p| p.tid == tid && p.ret_addr == addr && p.sp_after == sp)
            {
                let p = self.tr.pending.remove(i);
                let ty = p.ret_ty.as_deref();
                let mut ret = match self.returned(tid, ty, &r) {
                    Ok(got) => self.ret_text(ty, got),
                    Err(why) => format!("? ({why})"),
                };
                if let Some(h) = p.hook.and_then(|id| self.tr.hooks.iter_mut().find(|h| h.id == id)) {
                    h.returns += 1;
                    if let Some(v) = h.ret {
                        match self.set_returned(tid, ty, &mut r, v) {
                            Ok(()) => {
                                let now = self.ret_text(ty, v);
                                ret = format!("{ret}, changed to {now}");
                            }
                            Err(e) => ret = format!("{ret}, not changed: {e}"),
                        }
                    }
                }
                self.tr.push(p.kind, &p.name, format!("{} = {ret}", p.text));
            }
        }
        let site = self
            .tr
            .lib
            .as_ref()
            .and_then(|l| l.sites.get(&addr).filter(|s| l.names.is_empty() || l.names.contains(&s.name)).cloned());
        if let Some(s) = site {
            let r = self.t.regs(tid)?;
            let proto = nexo_bin::libc_proto(&s.name);
            let (text, ret_ty) = self.call_args(tid, &s.name, proto.as_ref(), &r, false, s.call_len);
            self.pend(tid, &r, s.call_len, Kind::Lib, None, &s.name, text, ret_ty)?;
        }
        let ids: Vec<u32> = self.tr.hooks.iter().filter(|h| h.addr == addr).map(|h| h.id).collect();
        for id in ids {
            let Some(i) = self.tr.hooks.iter().position(|h| h.id == id) else { continue };
            let h = self.tr.hooks[i].clone();
            self.tr.hooks[i].hits += 1;
            let mut r = self.t.regs(tid)?;
            let (mut text, ret_ty) = self.call_args(tid, &h.name, h.proto.as_ref(), &r, true, h.call_len);
            text = format!("#{id} {text}");
            if !h.set.is_empty() {
                let done: Vec<String> =
                    h.set.iter().map(|&(n, v)| self.set_arg(tid, h.proto.as_ref(), &mut r, h.call_len, n, v)).collect();
                if !r.x32 {
                    self.t.set_regs(tid, &r)?;
                }
                text.push_str(&format!(" [changed {}]", done.join(" ")));
            }
            if h.bt > 0 {
                let frames = {
                    let Session { t, mods, .. } = self;
                    let mut rd = |a: u64| {
                        let mut b = [0u8; 8];
                        (t.read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
                    };
                    mods.backtrace(&r, &mut rd, h.bt + 1)
                };
                let names: Vec<String> = frames
                    .iter()
                    .skip(1)
                    .map(|f| self.mods.label(f.pc.wrapping_sub(1), false).unwrap_or_else(|| format!("{:#x}", f.pc)))
                    .collect();
                if !names.is_empty() {
                    text.push_str(&format!(" from {}", names.join(" <- ")));
                }
            }
            text.push_str(&format!(" [hit {}]", h.hits + 1));
            self.pend(tid, &r, h.call_len, Kind::Hook, Some(id), &h.name, text.clone(), ret_ty)?;
            if h.stop && verdict.is_none() {
                verdict = Some((tid, Why::Hook(id, text)));
            }
        }
        Ok(verdict)
    }

    #[allow(clippy::too_many_arguments)]
    fn pend(
        &mut self,
        tid: Tid,
        r: &Regs,
        call_len: Option<u8>,
        kind: Kind,
        hook: Option<u32>,
        name: &str,
        text: String,
        ret_ty: Option<String>,
    ) -> Result<()> {
        let (ret_addr, sp_after) = match call_len {
            None => {
                // a 32-bit program's return address has 4 bytes
                let n = if r.x32 { 4 } else { 8 };
                let Some(back) = self.word_at(r.rsp(), n) else {
                    self.tr.push(kind, name, format!("{text} = ? (return address unreadable)"));
                    return Ok(());
                };
                (back, r.rsp() + n as u64)
            }
            Some(len) => (r.rip + len as u64, r.rsp()),
        };
        if self.tr.pending.len() >= PENDING_MAX {
            self.tr.push(kind, name, format!("{text} = ? (too many calls in flight to follow this one)"));
            return Ok(());
        }
        self.tr.pending.push(Pending { tid, ret_addr, sp_after, kind, hook, name: name.into(), text, ret_ty });
        Ok(())
    }

    /// The program ended: calls still in flight never returned.
    pub(crate) fn hooks_end(&mut self) {
        for p in std::mem::take(&mut self.tr.pending) {
            self.tr.push(p.kind, &p.name, format!("{} = ? (its return was not seen)", p.text));
        }
    }

    /// The import calls of the program, by memory address.
    fn lib_sites(&self) -> BTreeMap<u64, Site> {
        let bias = self.mods.main_bias();
        nexo_bin::import_sites(self.mods.main_image())
            .into_iter()
            .map(|s| (s.addr.wrapping_add(bias), Site { name: s.name, call_len: s.call_len }))
            .collect()
    }

    /// `ltrace [names]` | `ltrace off` | `ltrace show [names]`.
    pub(crate) fn cmd_ltrace(&mut self, a: &Args) -> Result<Output> {
        match a.pos.first().map(String::as_str) {
            Some("off") => {
                self.tr.lib = None;
                self.tr.pending.retain(|p| p.kind != Kind::Lib);
                Ok(rows("ltrace", vec!["library call trace off (`ltrace show` keeps the lines)".into()], json!({})))
            }
            Some("show") => {
                let names: Vec<String> = a.pos[1..].iter().flat_map(|w| w.split(',')).map(str::to_string).collect();
                Ok(self.show(
                    Kind::Lib,
                    &|n: &str| names.is_empty() || names.iter().any(|x| x == n),
                    "`ltrace` turns it on",
                ))
            }
            _ => {
                self.need_stopped()?;
                let names: Vec<String> = a.pos.iter().flat_map(|w| w.split(',')).map(str::to_string).collect();
                let sites = self.lib_sites();
                if sites.is_empty() {
                    return fail("this program calls no library function through an import (a static program?): `hook <function>` follows one");
                }
                let known: Vec<&str> = sites.values().map(|s| s.name.as_str()).collect();
                if let Some(bad) = names.iter().find(|n| !known.contains(&n.as_str())) {
                    let mut k = known.clone();
                    k.sort();
                    k.dedup();
                    return usage(format!("the program imports no {bad:?}; it imports: {}", k.join(" ")));
                }
                let n = sites.values().filter(|s| names.is_empty() || names.contains(&s.name)).count();
                self.tr.lib = Some(crate::hooks::Lib { sites, names: names.clone() });
                let what = if names.is_empty() { "every imported function".to_string() } else { names.join(",") };
                Ok(rows(
                    "ltrace",
                    vec![format!(
                        "tracing library calls: {what} ({n} call sites); `cont`, then `ltrace show`; a call is written when it returns"
                    )],
                    json!({"sites": n}),
                ))
            }
        }
    }

    /// `hook <place> [bt N] [stop] [set argN V] [ret V]` | `hook del <id>|all` | `hook show [name]` | `hooks`.
    pub(crate) fn cmd_hook(&mut self, words: &[String]) -> Result<Output> {
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        match w.first().copied() {
            None | Some("list") => Ok(self.hook_list()),
            Some("show") => {
                let names: Vec<String> = w[1..].iter().map(|s| s.to_string()).collect();
                Ok(self.show(
                    Kind::Hook,
                    &|n: &str| names.is_empty() || names.iter().any(|x| x == n),
                    "`hook <function>` sets one",
                ))
            }
            Some("del") | Some("delete") => {
                let before = self.tr.hooks.len();
                match w.get(1).copied() {
                    Some("all") => self.tr.hooks.clear(),
                    Some(x) => {
                        let id: u32 = x
                            .trim_start_matches('#')
                            .parse()
                            .map_err(|_| crate::Error::Usage("hook del <id>|all".into()))?;
                        self.tr.hooks.retain(|h| h.id != id);
                    }
                    None => return usage("hook del <id>|all"),
                }
                let gone = before - self.tr.hooks.len();
                if gone == 0 {
                    return fail("no such hook: `hooks` lists them");
                }
                self.tr.pending.retain(|p| p.hook.is_none_or(|id| self.tr.hooks.iter().any(|h| h.id == id)));
                Ok(rows("hook", vec![format!("{gone} hook(s) deleted")], json!({})))
            }
            Some(place) => self.hook_add(place, &w[1..]),
        }
    }

    fn hook_add(&mut self, place: &str, rest: &[&str]) -> Result<Output> {
        let (mut bt, mut stop, mut set_words, mut ret_word) = (0usize, false, Vec::new(), None);
        let num = |s: Option<&&str>, what: &str| -> Result<u64> {
            let s = s.ok_or_else(|| crate::Error::Usage(format!("{what} needs a value")))?;
            if let Some(n) = s.strip_prefix('-') {
                let v = nexo_bin::parse_addr(n).map_err(|_| crate::Error::Usage(format!("{s:?} is not a number")))?;
                return Ok((v as i64).wrapping_neg() as u64);
            }
            nexo_bin::parse_addr(s).map_err(|_| crate::Error::Usage(format!("{s:?} is not a number")))
        };
        let mut i = 0;
        while i < rest.len() {
            match rest[i] {
                "bt" => {
                    bt = num(rest.get(i + 1), "bt")?.min(16) as usize;
                    i += 1;
                }
                "stop" => stop = true,
                "set" => {
                    let which = rest.get(i + 1).copied().unwrap_or("");
                    let n: usize =
                        which.strip_prefix("arg").and_then(|d| d.parse().ok()).filter(|&n| n < 64).ok_or_else(
                            || crate::Error::Usage(format!("set arg<N> <value>: {which:?} is not arg0, arg1, ...")),
                        )?;
                    let v = rest.get(i + 2).ok_or_else(|| crate::Error::Usage("set needs a value".into()))?;
                    set_words.push((n, *v));
                    i += 2;
                }
                "ret" => {
                    ret_word = Some(*rest.get(i + 1).ok_or_else(|| crate::Error::Usage("ret needs a value".into()))?);
                    i += 1;
                }
                w => {
                    return usage(format!(
                        "unexpected {w:?}: after the place come `bt N`, `stop`, `set argN V`, `ret V`"
                    ))
                }
            }
            i += 1;
        }
        let (addr, call_len, name, file_addr) = self.hook_place(place)?;
        let proto = file_addr
            .and_then(|fa| nexo_bin::proto_at(self.mods.main_image(), fa))
            .or_else(|| nexo_bin::libc_proto(&name));
        // a value for a float argument or result is a float (`2.5`), any other a number
        let value = |ty: Option<&str>, w: &str, what: &str| -> Result<u64> {
            match ty {
                Some(t) if is_float(t) => float_bits(t, w).ok_or_else(|| {
                    crate::Error::Usage(format!("{what} {w:?}: the {t} there takes a float such as 2.5"))
                }),
                _ => num(Some(&w), what),
            }
        };
        let arg_ty = |n: usize| proto.as_ref().and_then(|p| p.args.get(n)).map(|(t, _)| t.as_str());
        let set = set_words.iter().map(|&(n, w)| Ok((n, value(arg_ty(n), w, "set")?))).collect::<Result<Vec<_>>>()?;
        let ret = ret_word.map(|w| value(proto.as_ref().map(|p| p.ret.as_str()), w, "ret")).transpose()?;
        let id = self.new_id();
        let typed = match &proto {
            Some(p) => format!(
                "{} {}({})",
                p.ret,
                p.name,
                p.args.iter().map(|(t, n)| format!("{t} {n}")).collect::<Vec<_>>().join(", ")
            ),
            None => "no prototype: raw registers (`nexo bin proto` gives one)".into(),
        };
        let mut l = vec![format!("hook #{id} at {addr:#x} {name}: {typed}")];
        if !set.is_empty() || ret.is_some() {
            l.push("this hook changes the program: arguments are set at the entry, the result at the return".into());
        }
        l.push("`cont`, then `hook show`; `hooks` lists the counters".into());
        self.tr.hooks.push(Hook { id, addr, name, proto, call_len, bt, stop, set, ret, hits: 0, returns: 0 });
        Ok(rows("hook", l, json!({"id": id, "addr": addr})))
    }

    /// Where a hook goes: (memory address, call length when on a `call [slot]`, name, file address in the program).
    fn hook_place(&mut self, place: &str) -> Result<(u64, Option<u8>, String, Option<u64>)> {
        let bias = self.mods.main_bias();
        match self.resolve_place(place) {
            Ok(Place::Code(x)) => {
                let fa = x.wrapping_sub(bias);
                let in_main = self.mods.main_image().section_at(fa).is_some();
                let name = self
                    .mods
                    .label(x, false)
                    .map(|l| {
                        let base = l.split('+').next().unwrap_or(&l);
                        // a stub is the import itself: `atoi@plt` takes atoi's prototype
                        base.strip_suffix("@plt").unwrap_or(base).to_string()
                    })
                    .unwrap_or_else(|| place.to_string());
                Ok((x, None, name, in_main.then_some(fa)))
            }
            Ok(Place::ImportSlot(slot)) => {
                // the import's own name: the stubs and the library know it by that, not by `puts@got`
                let bare = self.mods.import_of_slot(slot).unwrap_or_else(|| place.trim_start_matches('!').into());
                let bare = bare.as_str();
                // the program's way into the import: its stub, or each `call [slot]`
                let s = self.lib_sites().into_iter().find(|(_, s)| s.name == bare);
                match s {
                    Some((a, s)) if s.call_len.is_none() => Ok((a, None, bare.to_string(), None)),
                    _ => match self.resolve_place(&format!("!{bare}")) {
                        Ok(Place::Code(x)) => Ok((x, None, bare.to_string(), None)),
                        _ => fail(format!("{bare} is an import with no stub and its library is not loaded yet: `cont` past the entry first")),
                    },
                }
            }
            Ok(Place::Data(x)) => fail(format!("{place} ({x:#x}) is data, not code")),
            Err(e) => Err(e),
        }
    }

    fn hook_list(&mut self) -> Output {
        let mut l = Vec::new();
        for h in &self.tr.hooks {
            let mut what = Vec::new();
            if h.bt > 0 {
                what.push(format!("bt {}", h.bt));
            }
            if h.stop {
                what.push("stop".into());
            }
            // a float as it was written, any other value in hex
            let shown = |ty: Option<&str>, v: u64| match ty {
                Some(t) if is_float(t) => float_text(bits_f64(t, v)),
                _ => format!("{v:#x}"),
            };
            for &(n, v) in &h.set {
                let ty = h.proto.as_ref().and_then(|p| p.args.get(n)).map(|(t, _)| t.as_str());
                what.push(format!("set arg{n} {}", shown(ty, v)));
            }
            if let Some(v) = h.ret {
                what.push(format!("ret {}", shown(h.proto.as_ref().map(|p| p.ret.as_str()), v)));
            }
            l.push(format!(
                "hook #{} {:#x} {} entries {} returns {}{}",
                h.id,
                h.addr,
                h.name,
                h.hits,
                h.returns,
                if what.is_empty() { String::new() } else { format!(" [{}]", what.join(", ")) }
            ));
        }
        if l.is_empty() {
            l.push("no hooks: `hook <function> [bt N] [stop] [set argN V] [ret V]`".into());
        }
        rows("hooks", l, json!({}))
    }
}
