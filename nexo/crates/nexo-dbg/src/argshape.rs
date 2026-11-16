//! What each argument and the result of a call are as the call passes them. A prototype writes its types as the
//! source did (`real`, `point_t`, `struct pair`); where a value goes depends on what the name stands for: a typedef
//! of `double` travels in an xmm register, a struct by value in the registers of its eightbytes or in memory, an
//! `__int128` in two integer registers. The file's debug info says what each name is.

use nexo_bin::{Image, Proto, Ty};

use crate::callargs::SysV;
use crate::platform::Os;
use crate::vector::{f80_to_f64, float_text};

/// How deep typedefs that end in another typedef's name are followed.
const MAX_TYPEDEFS: usize = 4;
/// Elements of an array member shown before `...`.
const SHOWN_ELEMS: usize = 8;

/// How a value travels in a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// An integer, a pointer or a float: the C type that says which, typedefs resolved (`real` is `double`).
    Scalar(String),
    /// A struct, a union, an `__int128` or a complex number, passed by value.
    Agg(Agg),
    /// A struct or union the file does not describe (as written): its size, and so where it and every later
    /// argument are, is not known.
    Unknown(String),
}

/// A value of more than one part passed by value.
#[derive(Debug, Clone, PartialEq)]
pub struct Agg {
    /// The type as the prototype writes it.
    pub text: String,
    pub size: u64,
    pub align: u64,
    pub sysv: SysV,
    pub parts: Parts,
}

/// What an aggregate is made of, to show its value.
#[derive(Debug, Clone, PartialEq)]
pub enum Parts {
    Fields(Ty),
    Int128 {
        signed: bool,
    },
    /// The real and the imaginary part, each a float of this many bytes (10: an x87 `long double` in 16).
    Complex(usize),
}

/// A prototype with each argument and the result as the call passes them.
#[derive(Debug, Clone)]
pub struct Sig {
    pub proto: Proto,
    pub args: Vec<Shape>,
    pub ret: Shape,
}

impl Sig {
    /// `img`: the file whose debug info and named structs say what the prototype's type names are.
    pub fn new(img: Option<&Image>, proto: Proto) -> Sig {
        let args = proto.args.iter().map(|(t, _)| shape(img, t)).collect();
        let ret = shape(img, &proto.ret);
        Sig { proto, args, ret }
    }

    /// The declared type of argument `n`, as written.
    pub fn arg_text(&self, n: usize) -> Option<&str> {
        self.proto.args.get(n).map(|(t, _)| t.as_str())
    }
}

impl Shape {
    /// The C type of a scalar, `None` for anything else.
    pub fn scalar(&self) -> Option<&str> {
        match self {
            Shape::Scalar(t) => Some(t),
            _ => None,
        }
    }

    /// Whether the function returns this through memory: the caller passes where as a hidden first argument
    /// (rdi, rcx, or the first stack slot of a 32-bit call) and gets the same address back in rax. `None`: not
    /// known (a struct the file does not describe).
    pub fn hidden_return(&self, os: Os, x32: bool) -> Option<bool> {
        let a = match self {
            Shape::Scalar(_) => return Some(false),
            Shape::Unknown(_) => return None,
            Shape::Agg(a) => a,
        };
        let small = matches!(a.size, 1 | 2 | 4 | 8);
        Some(match (os, x32) {
            // MinGW returns an __int128 in xmm0
            (Os::Windows, false) => !small && !matches!(a.parts, Parts::Int128 { .. }),
            (Os::Windows, true) => !small,
            // the i386 System V ABI returns every struct and union through memory; a complex float comes in edx:eax
            (_, true) => a.parts != Parts::Complex(4),
            (_, false) => a.sysv == SysV::Memory,
        })
    }
}

/// What the type written `ty` is as a call passes it, from `img`'s typedefs and structs when there is one.
pub fn shape(img: Option<&Image>, ty: &str) -> Shape {
    shape_in(img, ty, 0)
}

fn shape_in(img: Option<&Image>, ty: &str, depth: usize) -> Shape {
    let text = ty.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.contains('*') || text.contains('[') {
        return Shape::Scalar(text);
    }
    let bare = text.split(' ').filter(|w| !matches!(*w, "const" | "volatile")).collect::<Vec<_>>().join(" ");
    if let Some(to) = img.filter(|_| depth < MAX_TYPEDEFS).and_then(|i| nexo_bin::typedef_of(i, &bare)) {
        return match shape_in(img, &to, depth + 1) {
            Shape::Agg(a) => Shape::Agg(Agg { text, ..a }),
            Shape::Unknown(_) => Shape::Unknown(text),
            s => s,
        };
    }
    if let Some(a) = special(&bare) {
        return Shape::Agg(Agg { text, ..a });
    }
    let named = bare.starts_with("struct ") || bare.starts_with("union ") || bare.starts_with("class ");
    if bare.contains("float") || bare.contains("double") || bare.starts_with("enum ") {
        return Shape::Scalar(bare);
    }
    if let Some(img) = img {
        if let Ok(v) = nexo_bin::resolve_type(img, &bare) {
            if let Ty::Struct(l) = v.ty() {
                let t = v.ty().clone();
                let size = l.size;
                return Shape::Agg(Agg {
                    text,
                    size,
                    align: t.align(),
                    sysv: sysv_of(&t, size),
                    parts: Parts::Fields(t),
                });
            }
            return Shape::Scalar(bare);
        }
    }
    if named {
        return Shape::Unknown(text);
    }
    Shape::Scalar(bare)
}

/// `__int128` and the complex numbers, which C names without a struct.
fn special(t: &str) -> Option<Agg> {
    let words: Vec<&str> = t.split(' ').collect();
    let has = |w: &str| words.contains(&w);
    let agg = |size, align, sysv, parts| Some(Agg { text: String::new(), size, align, sysv, parts });
    if has("__int128") || has("__int128_t") || has("__uint128_t") || has("i128") || has("u128") {
        let signed = !(has("unsigned") || has("__uint128_t") || has("u128"));
        return agg(16, 16, SysV::Regs(false, Some(false)), Parts::Int128 { signed });
    }
    if !(has("_Complex") || has("complex")) {
        return None;
    }
    if has("float") {
        agg(8, 4, SysV::Regs(true, None), Parts::Complex(4))
    } else if has("long") {
        agg(32, 16, SysV::X87, Parts::Complex(10))
    } else {
        agg(16, 8, SysV::Regs(true, Some(true)), Parts::Complex(8))
    }
}

/// How a scalar part of an aggregate travels.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Leaf {
    Int,
    Sse,
    X87,
}

/// Each scalar part of a value of type `t` at `off`: (offset, bytes, how it travels). `false`: a part is not at an
/// offset its alignment allows (a packed struct), which System V passes in memory.
fn leaves(t: &Ty, off: u64, out: &mut Vec<(u64, u64, Leaf)>) -> bool {
    let size = t.size().unwrap_or(0);
    match t {
        Ty::Struct(l) => l.fields.iter().all(|m| leaves(&m.ty, off + m.off, out)),
        Ty::Array { elem, n } => {
            let w = elem.size().unwrap_or(0);
            // a struct of more than 16 bytes never gets here: an array in one has at most 16 elements
            (0..n.unwrap_or(0).min(16)).all(|i| leaves(elem, off + i * w, out))
        }
        _ if !off.is_multiple_of(t.align().max(1)) => false,
        Ty::Float { .. } => {
            out.push((off, size, Leaf::Sse));
            true
        }
        // a member the type view cannot read: what the file names it says how it travels
        Ty::Raw { text, .. } => {
            let leaf = if text.contains("long") && text.contains("double") {
                Leaf::X87
            } else if text.contains("Complex") || text.contains("float") || text.contains("double") {
                Leaf::Sse
            } else {
                Leaf::Int
            };
            out.push((off, size, leaf));
            true
        }
        _ => {
            out.push((off, size, Leaf::Int));
            true
        }
    }
}

/// How System V passes a struct of `size` bytes: more than 16 or a member out of its alignment in memory, one with a
/// `long double` in memory too (in st0 as a result), else each eightbyte in an xmm register when only floats are in
/// it and in an integer register when anything else is.
fn sysv_of(t: &Ty, size: u64) -> SysV {
    let mut parts = Vec::new();
    if size == 0 || size > 16 || !leaves(t, 0, &mut parts) {
        return SysV::Memory;
    }
    if parts.iter().any(|p| p.2 == Leaf::X87) {
        return SysV::X87;
    }
    let sse = |e: u64| !parts.iter().any(|&(o, n, l)| l == Leaf::Int && o < (e + 1) * 8 && o + n.max(1) > e * 8);
    SysV::Regs(sse(0), (size > 8).then(|| sse(1)))
}

/// The value of an aggregate from its bytes: `{x=1, y=2.5}`, an `__int128` as a number, a complex number as
/// `re+imi`.
pub fn agg_text(a: &Agg, b: &[u8]) -> String {
    match &a.parts {
        Parts::Int128 { signed } => {
            let mut x = [0u8; 16];
            let n = b.len().min(16);
            x[..n].copy_from_slice(&b[..n]);
            let v = u128::from_le_bytes(x);
            if *signed {
                (v as i128).to_string()
            } else {
                v.to_string()
            }
        }
        Parts::Complex(w) => {
            let step = if *w == 10 { 16 } else { *w };
            let part = |at: usize| float_of(b.get(at..at + w).unwrap_or(&[]));
            let (re, im) = (part(0), part(step));
            format!("{}{}{}i", float_text(re), if im.is_sign_negative() { "" } else { "+" }, float_text(im))
        }
        Parts::Fields(t) => ty_text(t, b),
    }
}

/// A float from its 4, 8 or 10 (x87) bytes.
fn float_of(b: &[u8]) -> f64 {
    match b.len() {
        4 => f64::from(f32::from_le_bytes(b.try_into().unwrap_or_default())),
        8 => f64::from_le_bytes(b.try_into().unwrap_or_default()),
        10 => f80_to_f64(b.try_into().unwrap_or_default()),
        _ => f64::NAN,
    }
}

fn ty_text(t: &Ty, b: &[u8]) -> String {
    let w = t.size().unwrap_or(0) as usize;
    let Some(b) = b.get(..w) else { return "?".into() };
    let mut x = [0u8; 8];
    x[..w.min(8)].copy_from_slice(&b[..w.min(8)]);
    let v = u64::from_le_bytes(x);
    match t {
        Ty::Struct(l) => {
            let f: Vec<String> = l
                .fields
                .iter()
                .map(|m| {
                    format!("{}={}", m.name, b.get(m.off as usize..).map_or("?".into(), |rest| ty_text(&m.ty, rest)))
                })
                .collect();
            format!("{{{}}}", f.join(", "))
        }
        Ty::Array { elem, n } => {
            let ew = elem.size().unwrap_or(1).max(1) as usize;
            let n = n.unwrap_or(0) as usize;
            let mut f: Vec<String> =
                (0..n.min(SHOWN_ELEMS)).map(|i| b.get(i * ew..).map_or("?".into(), |r| ty_text(elem, r))).collect();
            if n > SHOWN_ELEMS {
                f.push("...".into());
            }
            format!("[{}]", f.join(", "))
        }
        Ty::Int { w, signed: true } if *w < 8 => (((v << (64 - 8 * w)) as i64) >> (64 - 8 * w)).to_string(),
        Ty::Int { signed: true, .. } => (v as i64).to_string(),
        Ty::Int { .. } => v.to_string(),
        Ty::Char if (v as u8).is_ascii_graphic() || v == u64::from(b' ') => format!("'{}'", v as u8 as char),
        Ty::Char => v.to_string(),
        Ty::Bool => (v != 0).to_string(),
        Ty::Float { .. } => float_text(float_of(b)),
        Ty::Ptr { .. } if v == 0 => "NULL".into(),
        Ty::Ptr { .. } => format!("{v:#x}"),
        Ty::Raw { text, .. } if text.contains("long") && text.contains("double") && w >= 10 => {
            float_text(float_of(&b[..10]))
        }
        _ if w <= 8 => format!("{v:#x}"),
        _ => format!("<{}>", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int128_and_complex_numbers_are_aggregates_of_their_own_size() {
        let Shape::Agg(a) = shape(None, "unsigned __int128") else { panic!() };
        assert_eq!((a.size, a.align, a.sysv), (16, 16, SysV::Regs(false, Some(false))));
        assert_eq!(agg_text(&a, &[0xff; 16]), u128::MAX.to_string());
        let Shape::Agg(c) = shape(None, "double _Complex") else { panic!() };
        assert_eq!(c.sysv, SysV::Regs(true, Some(true)));
        let mut b = 1.5f64.to_le_bytes().to_vec();
        b.extend((-2.0f64).to_le_bytes());
        assert_eq!(agg_text(&c, &b), "1.5-2.0i");
        assert_eq!(shape(None, "const double"), Shape::Scalar("double".into()));
        assert_eq!(shape(None, "struct pair"), Shape::Unknown("struct pair".into()));
        assert_eq!(shape(None, "struct pair *"), Shape::Scalar("struct pair *".into()));
        // a hidden pointer only for what does not come back in registers
        assert_eq!(Shape::Agg(a.clone()).hidden_return(Os::Linux, false), Some(false));
        assert_eq!(Shape::Agg(a).hidden_return(Os::Linux, true), Some(true));
        assert_eq!(Shape::Unknown("struct pair".into()).hidden_return(Os::Linux, false), None);
    }

    #[test]
    fn system_v_puts_an_eightbyte_of_floats_in_xmm_and_one_with_any_integer_in_a_general_register() {
        let f = |off, ty| nexo_bin::Member { off, name: String::new(), ty };
        let st = |size, fields| Ty::Struct(Box::new(nexo_bin::Layout { name: "s".into(), size, fields, named: true }));
        let d = Ty::Float { w: 8 };
        let i = Ty::Int { w: 4, signed: true };
        let fl = Ty::Float { w: 4 };
        assert_eq!(sysv_of(&st(16, vec![f(0, d.clone()), f(8, i.clone())]), 16), SysV::Regs(true, Some(false)));
        assert_eq!(sysv_of(&st(8, vec![f(0, fl.clone()), f(4, i.clone())]), 8), SysV::Regs(false, None));
        assert_eq!(sysv_of(&st(8, vec![f(0, fl.clone()), f(4, fl)]), 8), SysV::Regs(true, None));
        assert_eq!(sysv_of(&st(24, vec![f(0, d.clone()), f(8, d.clone()), f(16, d)]), 24), SysV::Memory);
        let ld = Ty::Raw { text: "longdouble".into(), size: 16 };
        assert_eq!(sysv_of(&st(16, vec![f(0, ld)]), 16), SysV::X87);
        // packed: an int at offset 1 is not where its alignment puts it
        assert_eq!(sysv_of(&st(5, vec![f(0, Ty::Char), f(1, i)]), 5), SysV::Memory);
    }
}
