//! `diff`: two builds of one program, compared function by function. Both files are only read.
//!
//! Functions are paired by the name the file gives them. Unnamed ones (a stripped file) are paired by
//! their code, by their place in the call graph (what a pair calls and who calls it pair up too), then
//! by size and callees. Each instruction is hashed with every address replaced by what it points at:
//! the instruction it lands on inside the function, an import, a named function, a string, a data
//! symbol or a section. So code that only moved counts as the same, and a call that now goes to another
//! function counts as a change.

mod one;

use std::collections::HashMap;

use iced_x86::{Instruction, Mnemonic, OpKind, Register};
use serde_json::{json, Value};

use crate::disasm::walk;
use crate::funcs::Func;
use crate::ops::command;
use crate::page::{cut, shell_word, Output, Row};
use crate::{Error, Image, Result};

/// Rounds of pairing by code and calls; each round goes one call further from the pairs already made.
const MAX_ROUNDS: usize = 64;
/// The most edits the instruction diff of one function looks for before it stops aligning.
const MAX_EDITS: usize = 1000;
/// The most instructions (both sides, past the common start and end) the alignment takes on.
const MAX_ALIGN: usize = 20_000;
/// Bytes of one function that are compared; the rest of a larger one is not.
const MAX_FUNC: usize = 1 << 22;
/// Same instructions shown around each change in the instruction diff.
const CONTEXT: usize = 3;
/// Instructions a function needs before an added one is said to have the code of a removed one.
const TWIN_INSNS: usize = 3;

struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn byte(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
    fn push(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.byte(b);
        }
    }
    fn bytes(&mut self, s: &[u8]) {
        self.push(s.len() as u64);
        for &b in s {
            self.byte(b);
        }
    }
    fn text(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
}

fn tagged(tag: u64, s: &str) -> u64 {
    let mut h = Fnv::new();
    h.push(tag);
    h.text(s);
    h.0
}

/// A data symbol and the offset into it.
fn label(name: &str, off: u64) -> u64 {
    let mut h = Fnv::new();
    h.push(16);
    h.text(name);
    h.push(off);
    h.0
}

/// Each data reference of one side shares a name with the one in the same place on the other.
fn same_data(x: &[Vec<u64>], y: &[Vec<u64>]) -> bool {
    x.len() == y.len() && x.iter().zip(y).all(|(u, v)| u.iter().any(|h| v.binary_search(h).is_ok()))
}

/// One instruction of a function.
struct Item {
    ins: Instruction,
    ip: u64,
    len: u64,
    /// The first byte, for bytes that do not decode.
    byte: u8,
    bad: bool,
    /// Every byte is zero (the filler after the last instruction).
    zero: bool,
}

/// A function's instructions without padding: no-ops and int3 are left out, and so are the zero bytes
/// after the last instruction.
struct Body {
    items: Vec<Item>,
    /// Where every instruction starts, padding included.
    starts: Vec<u64>,
    /// Bytes from the start to the end of the last instruction kept.
    size: u64,
    /// The function is larger than [`MAX_FUNC`] and only its start was read.
    big: bool,
}

impl Body {
    fn new() -> Body {
        Body { items: Vec::new(), starts: Vec::new(), size: 0, big: false }
    }

    fn read(img: &Image, f: &Func) -> Body {
        let mut b = Body::new();
        b.fill(img, f);
        b
    }

    fn fill(&mut self, img: &Image, f: &Func) {
        self.items.clear();
        self.starts.clear();
        let want = usize::try_from(f.size()).unwrap_or(usize::MAX);
        self.big = want > MAX_FUNC;
        let bytes = img.read_at(f.start, want.min(MAX_FUNC)).unwrap_or(&[]);
        let (items, starts) = (&mut self.items, &mut self.starts);
        let mut off = 0u64;
        walk(bytes, f.start, |ins, raw| {
            let ip = f.start + off;
            off += raw.len() as u64;
            starts.push(ip);
            let bad = ins.is_invalid();
            if !bad && matches!(ins.mnemonic(), Mnemonic::Nop | Mnemonic::Int3) {
                return;
            }
            let zero = raw.iter().all(|&b| b == 0);
            let byte = raw.first().copied().unwrap_or(0);
            items.push(Item { ins: *ins, ip, len: raw.len() as u64, byte, bad, zero });
        });
        while self.items.last().is_some_and(|i| i.zero) {
            self.items.pop();
        }
        self.size = self.items.last().map_or(0, |i| i.ip + i.len - f.start);
    }

    /// The instruction a target inside the function lands on: the next one kept when it lands on
    /// padding (`items.len()` past the last one); `None` when it lands inside an instruction.
    fn at(&self, v: u64) -> Option<usize> {
        self.starts.binary_search(&v).ok()?;
        Some(self.items.partition_point(|i| i.ip < v))
    }
}

/// What one function's code points at, collected while it is hashed.
#[derive(Default)]
struct Refs {
    /// Functions (indices), in instruction order.
    funcs: Vec<usize>,
    /// Imports and named functions it points at (hashes of their names).
    named: Vec<u64>,
    /// Data symbols, in instruction order: every name the address has (hashes, sorted), since several
    /// symbols can sit at one address and which of them is listed first moves with the layout.
    data: Vec<Vec<u64>>,
}

/// One function, summed up.
struct Sum {
    hash: u64,
    size: u64,
    insns: usize,
    refs: Vec<usize>,
    /// [`Refs::named`], sorted, no repeats.
    named: Vec<u64>,
    data: Vec<Vec<u64>>,
    bad: usize,
    big: bool,
}

/// One build, read for the diff.
struct Side<'a> {
    img: &'a Image,
    funcs: &'a [Func],
    /// The name the file gives each function: `None` for `sub_` and for names that come from signatures.
    names: Vec<Option<&'a str>>,
    /// Addresses written in the code as plain numbers point into the file (it is not position
    /// independent), so they are read as places too.
    fixed: bool,
    /// The names of the symbols at each address (hashes).
    labels: HashMap<u64, Vec<u64>>,
    sums: Vec<Sum>,
    /// The functions that point at each one.
    callers: Vec<Vec<usize>>,
}

impl<'a> Side<'a> {
    fn new(img: &'a Image) -> Side<'a> {
        let funcs = img.funcs();
        let names =
            funcs.iter().map(|f| (f.sig.is_none() && !f.name.starts_with("sub_")).then_some(f.name.as_str())).collect();
        let low = img.sections.iter().filter(|s| s.alloc && s.size > 0).map(|s| s.addr).min();
        let mut s = Side {
            img,
            funcs,
            names,
            fixed: low.is_some_and(|a| a >= 0x10000),
            labels: HashMap::new(),
            sums: Vec::with_capacity(funcs.len()),
            callers: vec![Vec::new(); funcs.len()],
        };
        for sym in &img.symbols {
            s.labels.entry(sym.addr).or_default().push(label(&sym.name, 0));
        }
        let mut body = Body::new();
        for f in funcs {
            body.fill(img, f);
            let mut refs = Refs::default();
            let mut h = Fnv::new();
            for it in &body.items {
                let (k, local) = s.key(f, &body, it, &mut refs);
                h.push(k);
                h.push(local.map_or(u64::MAX, |l| l as u64));
            }
            refs.named.sort_unstable();
            refs.named.dedup();
            let bad = body.items.iter().filter(|i| i.bad).count();
            let (size, insns, big) = (body.size, body.items.len(), body.big);
            s.sums.push(Sum { hash: h.0, size, insns, refs: refs.funcs, named: refs.named, data: refs.data, bad, big });
        }
        for (i, sum) in s.sums.iter().enumerate() {
            for &r in &sum.refs {
                s.callers[r].push(i);
            }
        }
        for c in &mut s.callers {
            c.sort_unstable();
            c.dedup();
        }
        s
    }

    fn name(&self, i: usize) -> &str {
        self.img.func_name(&self.funcs[i])
    }

    /// In a section or just past the end of one (where markers such as `_end` sit).
    fn inside(&self, v: u64) -> bool {
        self.img.section_at(v).is_some()
            || self.img.sections.iter().any(|s| s.alloc && s.size > 0 && s.addr + s.size == v)
    }

    /// The key of one instruction (equal in both builds when the instruction does the same) and the
    /// instruction a branch inside the function goes to.
    fn key(&self, f: &Func, body: &Body, it: &Item, refs: &mut Refs) -> (u64, Option<usize>) {
        let mut t = Fnv::new();
        if it.bad {
            t.push(0xbad);
            t.push(u64::from(it.byte));
            return (t.0, None);
        }
        let ins = &it.ins;
        let mut local = None;
        // a short and a near jump are one instruction written two ways
        if ins.op_count() > 0 && ins.op0_kind() == OpKind::NearBranch64 {
            t.push(1);
            t.push(ins.mnemonic() as u64);
        } else {
            t.push(2);
            t.push(ins.code() as u64);
        }
        t.push(
            u64::from(ins.has_rep_prefix())
                | u64::from(ins.has_repne_prefix()) << 1
                | u64::from(ins.has_lock_prefix()) << 2,
        );
        for i in 0..ins.op_count() {
            let k = ins.op_kind(i);
            t.push(k as u64);
            let at = match k {
                OpKind::Register => {
                    t.push(ins.op_register(i) as u64);
                    None
                }
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    Some((ins.near_branch_target(), 0))
                }
                OpKind::Memory => {
                    let seg = ins.segment_prefix();
                    t.push(ins.memory_base() as u64);
                    t.push(ins.memory_index() as u64);
                    t.push(u64::from(ins.memory_index_scale()));
                    t.push(seg as u64);
                    t.push(ins.memory_size() as u64);
                    let size = ins.memory_size().size();
                    if ins.is_ip_rel_memory_operand() {
                        Some((ins.ip_rel_memory_address(), size))
                    } else {
                        let d = ins.memory_displacement64();
                        let tls = matches!(seg, Register::FS | Register::GS);
                        if self.fixed && !tls && self.inside(d) {
                            Some((d, size))
                        } else {
                            t.push(d);
                            None
                        }
                    }
                }
                OpKind::Immediate32 | OpKind::Immediate64 | OpKind::Immediate32to64 => {
                    let v = ins.immediate(i);
                    if self.fixed && self.inside(v) {
                        Some((v, 0))
                    } else {
                        t.push(v);
                        None
                    }
                }
                OpKind::Immediate8
                | OpKind::Immediate8_2nd
                | OpKind::Immediate16
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32
                | OpKind::Immediate8to64 => {
                    t.push(ins.immediate(i));
                    None
                }
                _ => None,
            };
            if let Some((v, size)) = at {
                if let Some(l) = self.place(f, body, v, size, &mut t, refs) {
                    local = Some(l);
                }
            }
        }
        (t.0, local)
    }

    /// Hash what the address `v` is, never the number itself when it is a known place: the instruction
    /// it lands on inside this function (returned), an import, a function, a string, a data symbol or
    /// a section. `size` is how many bytes a memory operand reads there (0 when unknown).
    fn place(&self, f: &Func, body: &Body, v: u64, size: usize, t: &mut Fnv, refs: &mut Refs) -> Option<usize> {
        let img = self.img;
        if v >= f.start && v < f.end {
            return match body.at(v) {
                Some(i) => {
                    t.push(10);
                    Some(i)
                }
                None => {
                    t.push(11);
                    t.push(v - f.start);
                    None
                }
            };
        }
        if let Some(&i) = img.slots.get(&v) {
            let name = &img.imports[i].name;
            t.push(12);
            t.text(name);
            refs.named.push(tagged(12, name));
            return None;
        }
        let n = self.funcs.partition_point(|x| x.start <= v);
        if let Some(g) = n.checked_sub(1).filter(|&g| v < self.funcs[g].end) {
            refs.funcs.push(g);
            let off = v - self.funcs[g].start;
            match self.names[g] {
                Some(name) => {
                    t.push(13);
                    t.text(name);
                    t.push(off);
                    if off == 0 {
                        refs.named.push(tagged(13, name));
                    }
                }
                None => {
                    t.push(14);
                    t.push(off);
                }
            }
            return None;
        }
        if let Some((s, off)) = img.string_at(v) {
            t.push(15);
            t.push(u64::from(s.wide));
            t.text(&s.text);
            t.push(off);
            return None;
        }
        // a data symbol: the names are compared as sets (see `same_data`), not hashed
        let mut names = self.labels.get(&v).cloned().unwrap_or_default();
        if let Some((name, off)) = img.symbol_covering(v) {
            names.push(label(name, off));
        }
        if !names.is_empty() {
            names.sort_unstable();
            names.dedup();
            t.push(16);
            refs.data.push(names);
            return None;
        }
        match img.section_at(v) {
            // a constant read from a read-only section is part of what the code does; its address is not
            Some(sec) if !sec.write => {
                t.push(17);
                t.text(&sec.name);
                if (1..=16).contains(&size) {
                    if let Some(b) = img.read_at(v, size).filter(|b| b.len() == size) {
                        let word = <[u8; 8]>::try_from(b).ok().map(u64::from_le_bytes);
                        if self.fixed && word.is_some_and(|w| img.section_at(w).is_some()) {
                            t.push(18);
                        } else {
                            t.bytes(b);
                        }
                    }
                }
            }
            // which global an unnamed writable address is moves with the layout (.data and .bss trade
            // places, a marker sits just past the end), so only "writable data" is kept
            Some(_) => t.push(22),
            None if img.sections.iter().any(|s| s.alloc && s.write && s.addr + s.size == v) => t.push(22),
            None => {
                t.push(19);
                t.push(v);
            }
        }
        None
    }
}

/// How a pair was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum By {
    Name,
    Code,
    Graph,
    Shape,
    Callees,
    Order,
    Copies,
}

impl By {
    const ALL: [By; 7] = [By::Name, By::Code, By::Graph, By::Shape, By::Callees, By::Order, By::Copies];

    fn word(self) -> &'static str {
        match self {
            By::Name => "name",
            By::Code => "code",
            By::Graph => "call graph",
            By::Shape => "size and callees",
            By::Callees => "callees only",
            By::Order => "name and address order",
            By::Copies => "code and address order",
        }
    }
}

/// Which function of A goes with which of B.
struct Pairs {
    ab: Vec<Option<usize>>,
    ba: Vec<Option<usize>>,
    by: Vec<Option<By>>,
    /// Pairing was still finding pairs when the rounds ran out.
    capped: bool,
}

impl Pairs {
    fn join(&mut self, i: usize, j: usize, by: By) {
        self.ab[i] = Some(j);
        self.ba[j] = Some(i);
        self.by[i] = Some(by);
    }
}

/// Two functions may pair unless the files give them different names.
fn may_pair(a: &Side, b: &Side, i: usize, j: usize) -> bool {
    match (a.names[i], b.names[j]) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

fn pair_up(a: &Side, b: &Side) -> Pairs {
    let mut p = Pairs {
        ab: vec![None; a.funcs.len()],
        ba: vec![None; b.funcs.len()],
        by: vec![None; a.funcs.len()],
        capped: false,
    };
    let mut groups: HashMap<&str, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for (i, n) in a.names.iter().enumerate() {
        if let Some(n) = n {
            groups.entry(n).or_default().0.push(i);
        }
    }
    for (j, n) in b.names.iter().enumerate() {
        if let Some(n) = n {
            groups.entry(n).or_default().1.push(j);
        }
    }
    let mut repeated = Vec::new();
    for (xs, ys) in groups.into_values() {
        if xs.is_empty() || ys.is_empty() {
            continue;
        }
        if let ([x], [y]) = (xs.as_slice(), ys.as_slice()) {
            p.join(*x, *y, By::Name);
            continue;
        }
        // one name more than once (static functions of several files): a copy whose code is unique pairs now
        for &x in &xs {
            let h = a.sums[x].hash;
            let mut same = ys.iter().filter(|&&y| b.sums[y].hash == h);
            if let (Some(&y), None) = (same.next(), same.next()) {
                if xs.iter().filter(|&&x2| a.sums[x2].hash == h).count() == 1 {
                    p.join(x, y, By::Name);
                }
            }
        }
        repeated.push((xs, ys));
    }
    let mut progress = false;
    for _ in 0..MAX_ROUNDS {
        progress = by_key(&mut p, a, b, By::Code, |s, _, i, _| Some(s.sums[i].hash)) > 0
            || by_graph(&mut p, a, b) > 0
            || by_key(&mut p, a, b, By::Shape, |s, on_a, i, p| shape(s, on_a, i, p, true)) > 0
            || by_key(&mut p, a, b, By::Callees, |s, on_a, i, p| shape(s, on_a, i, p, false)) > 0;
        if !progress {
            break;
        }
    }
    p.capped = progress;
    for (xs, ys) in repeated {
        let xs: Vec<usize> = xs.into_iter().filter(|&x| p.ab[x].is_none()).collect();
        let ys: Vec<usize> = ys.into_iter().filter(|&y| p.ba[y].is_none()).collect();
        for (x, y) in xs.into_iter().zip(ys) {
            p.join(x, y, By::Order);
        }
    }
    // copies of one piece of code (such as two `endbr64; ret` stubs), as many on each side: which is
    // which cannot be told and does not change what they do, so they pair in address order
    let mut copies: HashMap<u64, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for i in (0..a.funcs.len()).filter(|&i| p.ab[i].is_none()) {
        copies.entry(a.sums[i].hash).or_default().0.push(i);
    }
    for j in (0..b.funcs.len()).filter(|&j| p.ba[j].is_none()) {
        copies.entry(b.sums[j].hash).or_default().1.push(j);
    }
    for (xs, ys) in copies.into_values() {
        let fits = |(&x, &y): (&usize, &usize)| may_pair(a, b, x, y) && same_data(&a.sums[x].data, &b.sums[y].data);
        if xs.len() == ys.len() && xs.iter().zip(&ys).all(fits) {
            for (x, y) in xs.into_iter().zip(ys) {
                p.join(x, y, By::Copies);
            }
        }
    }
    p
}

/// Pair the functions whose key is unique among the ones not paired yet, on both sides.
fn by_key(p: &mut Pairs, a: &Side, b: &Side, by: By, key: impl Fn(&Side, bool, usize, &Pairs) -> Option<u64>) -> usize {
    let count = |s: &Side, on_a: bool, open: &dyn Fn(usize) -> bool| {
        let mut m: HashMap<u64, (usize, usize)> = HashMap::new();
        for i in (0..s.funcs.len()).filter(|&i| open(i)) {
            if let Some(k) = key(s, on_a, i, p) {
                m.entry(k).or_insert((0, i)).0 += 1;
            }
        }
        m
    };
    let ka = count(a, true, &|i| p.ab[i].is_none());
    let kb = count(b, false, &|j| p.ba[j].is_none());
    let found: Vec<(usize, usize)> = ka
        .iter()
        .filter(|(_, &(n, _))| n == 1)
        .filter_map(|(k, &(_, i))| match kb.get(k) {
            Some(&(1, j)) if may_pair(a, b, i, j) => Some((i, j)),
            _ => None,
        })
        .collect();
    for &(i, j) in &found {
        p.join(i, j, by);
    }
    found.len()
}

/// What a function points at, as one number: the imports and named functions, the unnamed ones by
/// the pair they are in; with the size when `sized`. `None` when too little of it is known.
fn shape(s: &Side, on_a: bool, i: usize, p: &Pairs, sized: bool) -> Option<u64> {
    let mut ids = s.sums[i].named.clone();
    let mut known = ids.len();
    for &r in &s.sums[i].refs {
        if s.names[r].is_some() {
            continue;
        }
        let paired = if on_a { p.ab[r] } else { p.ba[r].map(|_| r) };
        match paired {
            Some(j) => {
                known += 1;
                ids.push(tagged(20, &j.to_string()));
            }
            None => ids.push(21),
        }
    }
    if known < if sized { 1 } else { 2 } {
        return None;
    }
    ids.sort_unstable();
    ids.dedup();
    let mut h = Fnv::new();
    for id in ids {
        h.push(id);
    }
    if sized {
        h.push(s.sums[i].size);
    }
    Some(h.0)
}

/// Pair what a pair points at and who points at it: the same code points at its functions in the same
/// order; otherwise only one function left on each side pairs. A function proposed twice is left alone.
fn by_graph(p: &mut Pairs, a: &Side, b: &Side) -> usize {
    let mut wa: HashMap<usize, Option<usize>> = HashMap::new();
    let mut wb: HashMap<usize, Option<usize>> = HashMap::new();
    let mut propose = |i: usize, j: usize| {
        if !may_pair(a, b, i, j) {
            return;
        }
        let e = wa.entry(i).or_insert(Some(j));
        if *e != Some(j) {
            *e = None;
        }
        let e = wb.entry(j).or_insert(Some(i));
        if *e != Some(i) {
            *e = None;
        }
    };
    for i in 0..a.funcs.len() {
        let Some(j) = p.ab[i] else { continue };
        let (ra, rb) = (&a.sums[i].refs, &b.sums[j].refs);
        if a.sums[i].hash == b.sums[j].hash && ra.len() == rb.len() {
            for (&x, &y) in ra.iter().zip(rb) {
                if p.ab[x].is_none() && p.ba[y].is_none() {
                    propose(x, y);
                }
            }
        } else if let Some((x, y)) = single(ra, rb, p) {
            propose(x, y);
        }
        if let Some((x, y)) = single(&a.callers[i], &b.callers[j], p) {
            propose(x, y);
        }
    }
    let mut found: Vec<(usize, usize)> = wa
        .iter()
        .filter_map(|(&i, &j)| {
            let j = j?;
            (wb.get(&j) == Some(&Some(i))).then_some((i, j))
        })
        .collect();
    found.sort_unstable();
    for &(i, j) in &found {
        p.join(i, j, By::Graph);
    }
    found.len()
}

/// The one function on each side not paired yet, when each side has exactly one.
fn single(xs: &[usize], ys: &[usize], p: &Pairs) -> Option<(usize, usize)> {
    fn one(mut v: impl Iterator<Item = usize>) -> Option<usize> {
        let first = v.next()?;
        v.all(|z| z == first).then_some(first)
    }
    let x = one(xs.iter().copied().filter(|&x| p.ab[x].is_none()))?;
    let y = one(ys.iter().copied().filter(|&y| p.ba[y].is_none()))?;
    Some((x, y))
}

/// The function `x` of A and `y` of B are the same callee: paired, both unpaired with the same code,
/// or the same name with the same code (a repeated name paired the other way round).
fn same_target(p: &Pairs, a: &Side, b: &Side, x: usize, y: usize) -> bool {
    let code = a.sums[x].hash == b.sums[y].hash;
    match (p.ab[x], p.ba[y]) {
        (Some(y2), _) if y2 == y => true,
        (None, None) => code,
        _ => code && a.names[x].is_some() && a.names[x] == b.names[y],
    }
}

/// Every function and data symbol the pair points at is the same one on both sides.
fn same_targets(p: &Pairs, a: &Side, b: &Side, i: usize, j: usize) -> bool {
    let (ra, rb) = (&a.sums[i].refs, &b.sums[j].refs);
    ra.len() == rb.len()
        && ra.iter().zip(rb).all(|(&x, &y)| same_target(p, a, b, x, y))
        && same_data(&a.sums[i].data, &b.sums[j].data)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Status {
    Changed,
    Added,
    Removed,
    Same,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Changed => "changed",
            Status::Added => "added",
            Status::Removed => "removed",
            Status::Same => "same",
        }
    }
}

/// One row of the list.
struct Entry {
    status: Status,
    a: Option<usize>,
    b: Option<usize>,
    by: Option<By>,
    /// Same code, but a call or pointer now goes to another function or global.
    elsewhere: bool,
    /// An added (removed) function has the code of this removed (added) one.
    twin: Option<usize>,
}

fn status_of(p: &Pairs, a: &Side, b: &Side, i: usize, j: usize) -> (Status, bool) {
    if a.sums[i].hash != b.sums[j].hash {
        (Status::Changed, false)
    } else if !same_targets(p, a, b, i, j) {
        (Status::Changed, true)
    } else {
        (Status::Same, false)
    }
}

fn entries(p: &Pairs, a: &Side, b: &Side) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut gone: HashMap<u64, usize> = HashMap::new();
    let mut new: HashMap<u64, usize> = HashMap::new();
    for i in (0..a.funcs.len()).filter(|&i| p.ab[i].is_none() && a.sums[i].insns >= TWIN_INSNS) {
        gone.entry(a.sums[i].hash).or_insert(i);
    }
    for j in (0..b.funcs.len()).filter(|&j| p.ba[j].is_none() && b.sums[j].insns >= TWIN_INSNS) {
        new.entry(b.sums[j].hash).or_insert(j);
    }
    for i in 0..a.funcs.len() {
        let e = match p.ab[i] {
            Some(j) => {
                let (status, elsewhere) = status_of(p, a, b, i, j);
                Entry { status, a: Some(i), b: Some(j), by: p.by[i], elsewhere, twin: None }
            }
            None => {
                let twin = new.get(&a.sums[i].hash).copied().filter(|&j| same_data(&a.sums[i].data, &b.sums[j].data));
                Entry { status: Status::Removed, a: Some(i), b: None, by: None, elsewhere: false, twin }
            }
        };
        out.push(e);
    }
    for j in (0..b.funcs.len()).filter(|&j| p.ba[j].is_none()) {
        let twin = gone.get(&b.sums[j].hash).copied().filter(|&i| same_data(&a.sums[i].data, &b.sums[j].data));
        out.push(Entry { status: Status::Added, a: None, b: Some(j), by: None, elsewhere: false, twin });
    }
    let addr = |e: &Entry| match (e.a, e.b) {
        (Some(i), _) => a.funcs[i].start,
        (None, Some(j)) => b.funcs[j].start,
        _ => 0,
    };
    out.sort_by_key(|e| (e.status, addr(e)));
    out
}

/// Compare two builds of one program: with no `target`, the functions that changed, were added or
/// removed (`all` adds the same ones); with one, its instructions side by side. `target` is a name, an
/// address in `a`, or `b:` and an address (or name) in `b`.
pub fn diff_window(
    a: &Image,
    b: &Image,
    target: Option<&str>,
    grep: Option<&str>,
    all: bool,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    a.require_x86("diff")?;
    b.require_x86("diff")?;
    if a.format != b.format {
        return Err(Error::Bad(format!(
            "{} is {} and {} is {}: diff compares two builds of one program for the same format",
            a.path.display(),
            a.format.name(),
            b.path.display(),
            b.format.name()
        )));
    }
    if target.is_some() && grep.is_some() {
        return Err(Error::Usage("--grep filters the list of functions; leave it out when naming one function".into()));
    }
    let re = grep.map(regex::Regex::new).transpose().map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let (sa, sb) = (Side::new(a), Side::new(b));
    let p = pair_up(&sa, &sb);
    let base = command(a, "diff", &shell_word(&b.path.display().to_string()));
    match target {
        Some(t) => one::show(&sa, &sb, &p, t, all, &base, from, budget),
        None => list(&sa, &sb, &p, re.as_ref(), grep, all, &base, from, budget),
    }
}

fn text_row(t: String) -> Row {
    Row { key: 0, text: t, json: None }
}

#[allow(clippy::too_many_arguments)]
fn list(
    sa: &Side,
    sb: &Side,
    p: &Pairs,
    re: Option<&regex::Regex>,
    grep: Option<&str>,
    all: bool,
    base: &str,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let found = entries(p, sa, sb);
    let count = |s: Status| found.iter().filter(|e| e.status == s).count();
    let (changed, added, removed, same) =
        (count(Status::Changed), count(Status::Added), count(Status::Removed), count(Status::Same));
    let hit = |s: &Side, i: Option<usize>| {
        i.is_some_and(|i| {
            let f = &s.funcs[i];
            re.is_none_or(|r| {
                r.is_match(s.name(i))
                    || r.is_match(&f.name)
                    || s.img.raw_name_at(f.start).is_some_and(|n| r.is_match(n))
            })
        })
    };
    let shown: Vec<&Entry> =
        found.iter().filter(|e| all || e.status != Status::Same).filter(|e| hit(sa, e.a) || hit(sb, e.b)).collect();
    let path = |s: &Side| shell_word(&s.img.path.display().to_string());
    let mut by: Vec<(By, usize)> =
        By::ALL.iter().map(|&w| (w, p.by.iter().filter(|x| **x == Some(w)).count())).filter(|&(_, n)| n > 0).collect();
    by.sort_by_key(|&(w, n)| (std::cmp::Reverse(n), w as u8));
    let mut head = vec![format!(
        "diff {} -> {}: {} functions -> {}; {changed} changed, {added} added, {removed} removed, {same} same",
        path(sa),
        path(sb),
        sa.funcs.len(),
        sb.funcs.len()
    )];
    head.push(if by.is_empty() {
        "matched: none".to_string()
    } else {
        let parts: Vec<String> = by.iter().map(|(w, n)| format!("{n} by {}", w.word())).collect();
        format!("matched: {}", parts.join(", "))
    });
    if sa.img.sha256 == sb.img.sha256 {
        head.push("the two files are the same bytes".into());
    }
    if by.iter().any(|&(w, _)| w == By::Callees) {
        head.push("note: a pair matched by callees only is a guess; check it with its instruction diff".into());
    }
    if p.capped {
        head.push(format!(
            "note: pairing stopped after {MAX_ROUNDS} rounds; some added and removed functions may be pairs not found"
        ));
    }
    if let Some(e) = found.iter().find(|e| e.status == Status::Changed) {
        let i = e.a.unwrap_or(0);
        let t = match sa.names[i] {
            Some(n) if sa.funcs.iter().filter(|f| f.name == n).count() == 1 => shell_word(n),
            _ => format!("{:#x}", sa.funcs[i].start),
        };
        head.push(format!("next: {base} {t}  (its instructions side by side)"));
    }
    enum L<'e> {
        Text(String),
        Fn(&'e Entry),
    }
    let mut items: Vec<L> = head.into_iter().map(L::Text).collect();
    items.push(L::Text(format!("{:<8} {:<10}  {:<10}  {:<18}  name", "status", "a", "b", "size a -> b")));
    if shown.is_empty() {
        items.push(L::Text(match grep {
            Some(g) => format!("no function matches {g:?}"),
            None if changed + added + removed == 0 => "no function differs".into(),
            None => "nothing to list".into(),
        }));
    }
    items.extend(shown.iter().map(|e| L::Fn(e)));
    if !all && same > 0 {
        items.push(L::Text(format!("the {same} same functions are left out (--all lists them)")));
    }
    items.push(L::Text(
        "same = the same instructions once addresses are set aside; data bytes (initial values of globals) are not compared"
            .into(),
    ));
    let (rows, tail) = cut(&items, from, budget, |it| match it {
        L::Text(s) => text_row(s.clone()),
        L::Fn(e) => entry_row(sa, sb, e),
    });
    let mut rest = String::new();
    if let Some(g) = grep {
        rest.push_str(&format!(" --grep {}", shell_word(g)));
    }
    if all {
        rest.push_str(" --all");
    }
    let matched_by: serde_json::Map<String, Value> = by.iter().map(|(w, n)| (w.word().to_string(), json!(n))).collect();
    let meta = json!({
        "a": sa.img.path.display().to_string(), "b": sb.img.path.display().to_string(),
        "functions_a": sa.funcs.len(), "functions_b": sb.funcs.len(),
        "changed": changed, "added": added, "removed": removed, "same": same,
        "matched_by": matched_by, "listed": shown.len(), "grep": grep, "all": all,
    });
    Ok(Output { op: "diff", meta, rows, tail, command: format!("{base}{rest}") })
}

fn entry_row(sa: &Side, sb: &Side, e: &Entry) -> Row {
    let addr = |s: &Side, i: Option<usize>| i.map_or("-".to_string(), |i| format!("{:#010x}", s.funcs[i].start));
    let size = |s: &Side, i: Option<usize>| i.map_or("-".to_string(), |i| s.sums[i].size.to_string());
    let delta = match (e.a, e.b) {
        (Some(i), Some(j)) => Some(sb.sums[j].size as i64 - sa.sums[i].size as i64),
        _ => None,
    };
    let sizes = match delta {
        Some(d) if d != 0 => format!("{} -> {} ({d:+})", size(sa, e.a), size(sb, e.b)),
        _ => format!("{} -> {}", size(sa, e.a), size(sb, e.b)),
    };
    let (na, nb) = (e.a.map(|i| sa.name(i)), e.b.map(|j| sb.name(j)));
    let name = match (na, nb) {
        (Some(x), Some(y)) if x != y => format!("{x} -> {y}"),
        (Some(x), _) => x.to_string(),
        (None, Some(y)) => y.to_string(),
        (None, None) => String::new(),
    };
    let mut notes: Vec<String> = Vec::new();
    if let Some(by) = e.by.filter(|&w| w != By::Name) {
        notes.push(format!("matched by {}", by.word()));
    }
    if e.elsewhere {
        notes.push("same code, but it now calls or points at another function or global".into());
    }
    if let Some(t) = e.twin {
        let (other, side, at) = match e.status {
            Status::Added => (sa.name(t), "a", sa.funcs[t].start),
            _ => (sb.name(t), "b", sb.funcs[t].start),
        };
        notes.push(format!("same code as {other} ({at:#x}) in {side}"));
    }
    let bad: usize = e.a.map_or(0, |i| sa.sums[i].bad) + e.b.map_or(0, |j| sb.sums[j].bad);
    if bad > 0 {
        notes.push(format!("{bad} bytes do not decode as instructions"));
    }
    if e.a.is_some_and(|i| sa.sums[i].big) || e.b.is_some_and(|j| sb.sums[j].big) {
        notes.push(format!("only the first {} MiB compared", MAX_FUNC >> 20));
    }
    let tail = if notes.is_empty() { String::new() } else { format!("  [{}]", notes.join("; ")) };
    let text =
        format!("{:<8} {:<10}  {:<10}  {sizes:<18}  {name}{tail}", e.status.word(), addr(sa, e.a), addr(sb, e.b));
    let json = json!({
        "status": e.status.word(),
        "a_addr": e.a.map(|i| sa.funcs[i].start), "b_addr": e.b.map(|j| sb.funcs[j].start),
        "a_size": e.a.map(|i| sa.sums[i].size), "b_size": e.b.map(|j| sb.sums[j].size), "delta": delta,
        "name_a": na, "name_b": nb, "by": e.by.map(By::word), "notes": notes,
    });
    Row { key: 0, text, json: Some(json) }
}
