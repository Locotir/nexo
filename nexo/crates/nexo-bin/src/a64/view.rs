//! Walking AArch64 code and describing what an instruction points at.

use super::scan::{Tracker, Via};
use super::{decode, Flow, Ins};
use crate::disasm::describe;
use crate::Image;

/// Decode `bytes` (which start at `ip`) one word at a time and call `f` with each instruction, the
/// address it reaches through a register (see [`Tracker`]) and the four bytes it took. A tail shorter
/// than a word is left out.
pub(crate) fn walk(bytes: &[u8], ip: u64, mut f: impl FnMut(&Ins, Option<Via>, &[u8])) {
    let mut tr = Tracker::new();
    for (k, w) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let ins = decode(u32::from_le_bytes(*w), ip.wrapping_add(4 * k as u64));
        let via = tr.step(&ins);
        f(&ins, via, &w[..]);
        if ins.ends_block() {
            tr.clear();
        }
    }
}

/// The comment for an instruction line, if it points at something.
pub(crate) fn comment(img: &Image, ins: &Ins, via: Option<Via>) -> Option<String> {
    if let Some(why) = ins.undecoded {
        return Some(format!("not decoded: {}", why.word()));
    }
    match via {
        Some(Via::Addr(a)) => return describe(img, a, false),
        Some(Via::Read(a) | Via::Write(a)) => return describe(img, a, true),
        None => {}
    }
    match ins.flow {
        Flow::Call | Flow::Jump | Flow::Cond => ins.target.and_then(|t| describe(img, t, false)),
        _ => None,
    }
}
