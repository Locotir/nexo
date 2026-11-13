//! Variables from DWARF (`.debug_info`): the parameters and locals of the function at a stop, globals, and their
//! types, so `locals`, `print p->tag` and `print rows[2][1]` answer in the program's own words. Only the program
//! itself is read (not its libraries), and only plain (uncompressed) ELF sections, like `lines.rs`.

mod load;
mod path;
mod render;
mod scope;
mod session;
mod types;

use std::collections::HashMap;

use gimli::{AttributeValue, EndianSlice, EvaluationResult, LittleEndian, Location, Value};
use nexo_bin::Image;
use serde_json::json;

use crate::engine::{rows, Session};
use nexo_bin::Output;

use crate::{fail, Result};

/// Biggest `.debug_info` read: past it no variables are built.
const MAX_BYTES: usize = 64 << 20;

/// Longest value text: past it the rest is cut with `...`.
const MAX_TEXT: usize = 400;
/// How deep a type may nest before it is taken to loop back on itself.
const MAX_TYPE_DEPTH: usize = 32;

/// Array elements shown before `...`.
const SHOWN: usize = 12;

type Slice<'a> = EndianSlice<'a, LittleEndian>;

/// Where a variable lives: one expression, a list valid by address range (optimised code), or nowhere.
enum Loc {
    Expr(Vec<u8>),
    List(Vec<(u64, u64, Vec<u8>)>),
    None,
}

struct Var {
    name: String,
    param: bool,
    loc: Loc,
    ty: Option<u64>,
    /// The ranges of the lexical block it was declared in; `None` for the whole function.
    scope: Option<Vec<(u64, u64)>>,
}

struct Func {
    name: String,
    ranges: Vec<(u64, u64)>,
    frame_base: Option<Vec<u8>>,
    enc: gimli::Encoding,
    vars: Vec<Var>,
}

enum Ty {
    Base {
        name: String,
        size: u64,
        enc: u8,
    },
    Ptr {
        to: Option<u64>,
    },
    Array {
        elem: Option<u64>,
        dims: Vec<Option<u64>>,
    },
    Record {
        name: String,
        size: u64,
        members: Vec<(String, u64, Option<u64>)>,
        union: bool,
    },
    /// A typedef (with its name) or a `const`/`volatile` (name empty).
    Alias {
        name: String,
        to: Option<u64>,
    },
    Enum {
        name: String,
        size: u64,
        vals: Vec<(String, i64)>,
    },
    Func,
}

/// The variables and types of a program. Types are keyed by their `.debug_info` offset.
pub struct Vars {
    funcs: Vec<Func>,
    globals: Vec<(Var, gimli::Encoding)>,
    types: HashMap<u64, Ty>,
}

/// A type as used in a value: the type, and how many array dimensions were already indexed.
#[derive(Clone, Copy)]
struct View(Option<u64>, usize);

/// A value: where its bytes are, and its type.
struct Val {
    at: At,
    ty: View,
}

enum At {
    Mem(u64),
    Bytes(Vec<u8>),
}

/// What a location needs from the stopped program.
pub struct Regs<'a> {
    pub gp: [u64; 16],
    pub rip: u64,
    pub cfa: Option<u64>,
    pub bias: u64,
    pub rd: &'a mut dyn FnMut(u64, usize) -> Option<Vec<u8>>,
}

enum Ctx {
    Func(usize),
    Block(Vec<(u64, u64)>),
    Type(u64),
    Skip,
    Other,
}

#[cfg(test)]
mod tests {
    use super::render::{le, quote};
    use super::scope::eval;
    use super::*;

    #[test]
    fn little_endian_numbers_and_c_strings_read_as_a_person_writes_them() {
        assert_eq!(le(&[0xff, 0xff, 0xff, 0xff], true), -1);
        assert_eq!(le(&[0xff, 0xff, 0xff, 0xff], false), 0xffff_ffff);
        assert_eq!(quote(b"ab\0cd", true), "\"ab\"");
        assert_eq!(quote(b"a\nb", true), "\"a\\nb\" (no NUL in its 3 bytes)");
    }

    fn lone(types: Vec<(u64, Ty)>) -> Vars {
        Vars { funcs: Vec::new(), globals: Vec::new(), types: types.into_iter().collect() }
    }

    #[test]
    fn types_that_contain_themselves_end_in_a_name_and_a_size_not_a_stack_overflow() {
        // a pointer to itself, an array of itself, an alias loop, and a huge array of huge arrays
        let v = lone(vec![
            (1, Ty::Ptr { to: Some(1) }),
            (2, Ty::Array { elem: Some(2), dims: vec![Some(4)] }),
            (3, Ty::Alias { name: String::new(), to: Some(3) }),
            (4, Ty::Array { elem: Some(5), dims: vec![Some(u64::MAX), Some(u64::MAX)] }),
            (5, Ty::Array { elem: Some(5), dims: vec![Some(u64::MAX)] }),
            (6, Ty::Ptr { to: Some(6) }),
        ]);
        for t in 1..=6 {
            let _ = v.name(View(Some(t), 0));
            let _ = v.size(View(Some(t), 0));
        }
        assert!(v.name(View(Some(1), 0)).contains("..."));
    }

    #[test]
    fn random_location_expressions_never_panic_or_allocate_without_bound() {
        let enc = gimli::Encoding { address_size: 8, format: gimli::Format::Dwarf32, version: 4 };
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..4000 {
            let n = (next() % 24) as usize;
            let mut e: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            // lean on the opcodes that matter: piece (0x93), bit_piece (0x9d), deref (0x06), fbreg (0x91), addr (0x03)
            for byte in e.iter_mut() {
                if next() % 3 == 0 {
                    *byte = [0x93, 0x9d, 0x06, 0x91, 0x03, 0x23, 0x9f, 0x70, 0x9c][(next() % 9) as usize];
                }
            }
            let mut rd = |_: u64, n: usize| Some(vec![1u8; n.min(64)]);
            let mut regs = Regs { gp: [0x1000; 16], rip: 0x40_0000, cfa: Some(0x7fff_0000), bias: 0, rd: &mut rd };
            let t0 = std::time::Instant::now();
            let _ = eval(&e, enc, Some(&[0x9c]), &mut regs);
            assert!(t0.elapsed().as_secs() < 2, "{e:02x?}");
        }
    }
}
