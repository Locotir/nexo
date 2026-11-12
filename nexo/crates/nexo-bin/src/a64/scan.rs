//! Following addresses through registers: `adrp x8, page` then `add x8, x8, #lo` or
//! `ldr w0, [x8, #lo]` is how AArch64 code names a global, so the pair is read as one address.

use super::{Flow, Ins};

/// An address an instruction reaches through a register that an earlier `adrp` loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Via {
    /// The address is computed (`add x0, x8, #lo`, `adr`).
    Addr(u64),
    Read(u64),
    Write(u64),
}

/// What each register holds, when it is an address worked out from an `adrp`. Lives for one
/// function; a branch target does not reset it, so a pair split by a branch can be missed or, rarely,
/// carried across one.
pub(crate) struct Tracker {
    known: [Option<u64>; 31],
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker { known: [None; 31] }
    }

    pub fn clear(&mut self) {
        self.known = [None; 31];
    }

    /// Feed the next instruction in order; the address it reaches, if it is one.
    pub fn step(&mut self, ins: &Ins) -> Option<Via> {
        let base = |k: &[Option<u64>; 31], r: u8| k.get(usize::from(r)).copied().flatten();
        let mut via = None;
        let mut value = None;
        if let Some((rd, a)) = ins.adr {
            via = Some(Via::Addr(a));
            value = Some((rd, a));
        } else if let Some((rd, rn, imm)) = ins.addimm {
            if let Some(p) = base(&self.known, rn) {
                let a = p.wrapping_add(imm);
                via = Some(Via::Addr(a));
                value = Some((rd, a));
            }
        } else if let Some(m) = ins.mem {
            if let Some(p) = base(&self.known, m.base) {
                let a = p.wrapping_add(m.off as u64);
                via = Some(if m.store { Via::Write(a) } else { Via::Read(a) });
            }
        } else if let Some(l) = ins.literal {
            via = Some(Via::Read(l));
        }
        for (n, slot) in self.known.iter_mut().enumerate().take(31) {
            if ins.writes >> n & 1 == 1 {
                *slot = None;
            }
        }
        if matches!(ins.flow, Flow::Call | Flow::CallReg) {
            self.known[..19].fill(None);
        }
        if let Some((rd, page)) = ins.adrp {
            if let Some(slot) = self.known.get_mut(usize::from(rd)) {
                *slot = Some(page);
            }
        }
        if let Some((rd, a)) = value {
            if let Some(slot) = self.known.get_mut(usize::from(rd)) {
                *slot = Some(a);
            }
        }
        via
    }
}

use std::collections::BTreeSet;

use super::{view, Opd};
use crate::image::Section;
use crate::refs::{Collector, Kind};
use crate::Image;

/// Where `starts` are kept and how to tell an address is code.
type Starts<'a> = (&'a mut BTreeSet<u64>, &'a dyn Fn(u64) -> bool);

/// `stp xA, xB, [sp, #-N]!`, `sub sp, sp, #N`, `paciasp` or `bti c`: how a function opens.
fn is_prologue(i: &Ins) -> bool {
    let r = i.raw;
    (r & 0xffc0_0000 == 0xa980_0000 && (r >> 5) & 31 == 31)
        || r & 0xff00_03ff == 0xd100_03ff
        || r == 0xd503_233f
        || r == 0xd503_245f
        || r == 0xd503_249f
}

/// One pass over a code section: hand every instruction to the reference collector, and collect
/// function starts: call targets, and a prologue right after a `ret`, a plain `b`, a `br` or padding.
pub(crate) fn discover(img: &Image, sec: &Section, mut c: Option<&mut Collector>, mut starts: Option<Starts>) {
    let bytes = img.section_bytes(sec);
    #[cfg(test)]
    crate::disasm::DECODED.with(|d| d.set(d.get() + bytes.len()));
    let mut open = true;
    view::walk(bytes, sec.addr, |ins, via, _| {
        if let Some(c) = c.as_deref_mut() {
            collect(c, img, ins, via);
        }
        if let Some((set, in_code)) = starts.as_mut() {
            if ins.flow == Flow::Call {
                if let Some(t) = ins.target.filter(|&t| in_code(t)) {
                    set.insert(t);
                }
            }
            if open && ins.is_decoded() && is_prologue(ins) {
                set.insert(ins.pc);
            }
        }
        open = ins.ends_block() || (open && ins.is_padding());
    });
}

/// The import a PLT stub (`adrp x16; ldr x17, [x16, #off]; add ..; br x17`) goes through, as `name@plt`.
pub(crate) fn stub_name(img: &Image, start: u64, end: u64) -> Option<String> {
    let bytes = img.read_at(start, ((end - start) as usize).min(16))?;
    let mut found = None;
    let mut seen = 0;
    view::walk(bytes, start, |_, via, _| {
        seen += 1;
        if let (None, Some(Via::Read(a))) = (&found, via) {
            if seen <= 3 {
                found = img.slots.get(&a).map(|&i| img.imports[i].name.clone());
            }
        }
    });
    found.map(|n| if img.format == crate::image::Format::Elf { format!("{n}@plt") } else { n })
}

/// What one instruction refers to: branch targets, and addresses reached through `adrp` pairs.
pub(crate) fn collect(c: &mut Collector, img: &Image, ins: &Ins, via: Option<Via>) {
    if !ins.is_decoded() {
        return;
    }
    let mapped = |a: u64| img.section_at(a).is_some();
    if let Some((reg, slot, at)) = c.a64_load.take() {
        let by = match (ins.flow, ins.ops[0]) {
            (Flow::CallReg, Opd::Gpr { n, .. }) if n == reg => Some(Kind::Call),
            (Flow::JumpReg, Opd::Gpr { n, .. }) if n == reg => Some(Kind::Jump),
            _ => None,
        };
        match by {
            Some(kind) => {
                c.push(slot, ins.pc, kind);
                return;
            }
            None => {
                c.push(slot, at, Kind::Read);
            }
        }
    }
    match ins.flow {
        Flow::Call | Flow::Jump | Flow::Cond => {
            if let Some(t) = ins.target.filter(|&t| mapped(t)) {
                c.push(t, ins.pc, if ins.flow == Flow::Call { Kind::Call } else { Kind::Jump });
            }
            return;
        }
        Flow::CallReg => {
            c.indirect_calls += 1;
            return;
        }
        Flow::JumpReg => {
            c.indirect_jumps += 1;
            return;
        }
        _ => {}
    }
    match via {
        Some(Via::Addr(a)) if mapped(a) => {
            c.push(a, ins.pc, Kind::Addr);
        }
        Some(Via::Read(a)) if mapped(a) => match ins.ops[0] {
            Opd::Gpr { n, x: true, .. } if img.slots.contains_key(&a) => c.a64_load = Some((n, a, ins.pc)),
            _ => {
                c.push(a, ins.pc, Kind::Read);
            }
        },
        Some(Via::Write(a)) if mapped(a) => {
            c.push(a, ins.pc, Kind::Write);
        }
        _ => {}
    }
}
