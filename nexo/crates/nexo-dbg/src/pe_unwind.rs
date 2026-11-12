//! Unwinding one frame of a Windows x86-64 program from the tables the linker wrote: `.pdata` (one
//! RUNTIME_FUNCTION per function) and the UNWIND_INFO each one points to. A function with no entry
//! is a leaf: its return address is on top of the stack.

use nexo_bin::Image;

/// Windows register number (RAX RCX RDX RBX RSP RBP RSI RDI R8..R15) to the index in `Regs::gp`.
const WIN2GP: [usize; 16] = [0, 2, 3, 1, 7, 6, 4, 5, 8, 9, 10, 11, 12, 13, 14, 15];
const SP: usize = 7;

/// What `unwind` gives back: the caller's pc, its registers, and the stack pointer after the return.
pub type Step = (u64, [u64; 16], Option<u64>);

/// `len` bytes of the image at this relative address, when the file holds them.
fn at_rva(img: &Image, rva: u64, len: usize) -> Option<&[u8]> {
    let a = img.image_base.checked_add(rva)?;
    let s = img.section_at(a)?;
    let bytes = img.section_bytes(s);
    let off = (a - s.addr) as usize;
    bytes.get(off..off.checked_add(len)?)
}

fn u16_at(img: &Image, rva: u64) -> Option<u16> {
    at_rva(img, rva, 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(img: &Image, rva: u64) -> Option<u32> {
    at_rva(img, rva, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The RUNTIME_FUNCTION that covers `rva`: (begin, end, unwind info), by binary search on `.pdata`.
fn function_at(img: &Image, rva: u64) -> Option<(u32, u32, u32)> {
    let s = img.sections.iter().find(|s| s.name == ".pdata")?;
    let table = img.section_bytes(s);
    let n = table.len() / 12;
    let entry = |i: usize| {
        let e = &table[i * 12..i * 12 + 12];
        let w = |k: usize| u32::from_le_bytes([e[k], e[k + 1], e[k + 2], e[k + 3]]);
        (w(0), w(4), w(8))
    };
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let (b, e, u) = entry(mid);
        if rva < b as u64 {
            hi = mid;
        } else if rva >= e as u64 {
            lo = mid + 1;
        } else {
            return Some((b, e, u));
        }
    }
    None
}

/// The frame above the one at `pc` (a return address is looked up one byte earlier by the caller).
/// `None`: this file cannot say; `Some(None)` is never returned (no frame is known to be the last).
pub fn unwind(img: &Image, bias: u64, pc: u64, gp: &[u64; 16], rd: &mut dyn FnMut(u64) -> Option<u64>) -> Option<Step> {
    let rva = pc.wrapping_sub(bias).wrapping_sub(img.image_base);
    let mut n = *gp;
    match function_at(img, rva) {
        None => {
            // a leaf: nothing was pushed
            let ra = rd(n[SP])?;
            n[SP] += 8;
            Some((ra, n, Some(n[SP])))
        }
        Some((begin, _, info)) => {
            let into_prolog = rva - begin as u64;
            apply(img, info as u64, Some(into_prolog), &mut n, rd, 0)?;
            let ra = rd(n[SP])?;
            n[SP] += 8;
            Some((ra, n, Some(n[SP])))
        }
    }
}

/// Undo the prolog described by one UNWIND_INFO (and the ones it chains to) on `n`.
/// `prolog`: how far into the function the pc is, so only the steps already run are undone.
fn apply(
    img: &Image,
    info: u64,
    prolog: Option<u64>,
    n: &mut [u64; 16],
    rd: &mut dyn FnMut(u64) -> Option<u64>,
    depth: u32,
) -> Option<()> {
    if depth > 8 {
        return None;
    }
    let head = at_rva(img, info, 4)?;
    let (flags, count, frame) = (head[0] >> 3, head[2] as u64, head[3]);
    let code = |i: u64| -> Option<(u8, u8, u8)> {
        let b = at_rva(img, info + 4 + 2 * i, 2)?;
        Some((b[0], b[1] & 0xf, b[1] >> 4))
    };
    let slot16 = |i: u64| u16_at(img, info + 4 + 2 * i);
    let mut i = 0;
    while i < count {
        let (off, op, opinfo) = code(i)?;
        let mut used = 1;
        let run = prolog.is_none_or(|p| off as u64 <= p);
        match op {
            // PUSH_NONVOL
            0 => {
                if run {
                    n[WIN2GP[opinfo as usize]] = rd(n[SP])?;
                    n[SP] += 8;
                }
            }
            // ALLOC_LARGE
            1 => {
                used = if opinfo == 0 { 2 } else { 3 };
                if run {
                    let size = if opinfo == 0 {
                        slot16(i + 1)? as u64 * 8
                    } else {
                        slot16(i + 1)? as u64 | (slot16(i + 2)? as u64) << 16
                    };
                    n[SP] = n[SP].wrapping_add(size);
                }
            }
            // ALLOC_SMALL
            2 => {
                if run {
                    n[SP] += opinfo as u64 * 8 + 8;
                }
            }
            // SET_FPREG
            3 => {
                if run {
                    let reg = (frame & 0xf) as usize;
                    n[SP] = n[WIN2GP[reg]].wrapping_sub((frame >> 4) as u64 * 16);
                }
            }
            // SAVE_NONVOL, SAVE_NONVOL_FAR
            4 | 5 => {
                used = if op == 4 { 2 } else { 3 };
                if run {
                    let off = if op == 4 {
                        slot16(i + 1)? as u64 * 8
                    } else {
                        slot16(i + 1)? as u64 | (slot16(i + 2)? as u64) << 16
                    };
                    n[WIN2GP[opinfo as usize]] = rd(n[SP].wrapping_add(off))?;
                }
            }
            // SAVE_XMM128 (version 1 slot counts); an xmm register is not a thing this debugger unwinds
            8 => used = 2,
            9 => used = 3,
            // PUSH_MACHFRAME: an interrupt or exception frame, 5 words (rip cs eflags rsp ss), one more with an error code
            10 => {
                if run {
                    let at = n[SP] + if opinfo == 1 { 8 } else { 0 };
                    n[SP] = rd(at + 24)?;
                    return Some(());
                }
            }
            // epilog and spare entries of version 2
            6 => used = 2,
            7 => used = 3,
            _ => return None,
        }
        i += used;
    }
    if flags & 4 != 0 {
        // chained: the parent function's entry follows the codes, which are padded to an even count
        let at = info + 4 + 2 * ((count + 1) & !1);
        let parent_info = u32_at(img, at + 8)? as u64;
        apply(img, parent_info, None, n, rd, depth + 1)?;
    }
    Some(())
}
