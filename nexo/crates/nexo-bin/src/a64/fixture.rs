//! A tiny AArch64 ELF built byte by byte for tests, from instruction words written by hand. The same
//! six functions as `crate::fixtures::elf64_xref`: A (main) calls B and C; B takes the string's
//! address (`adrp` + `add`) and returns 1337; C returns 0; D calls nothing and is called by nobody;
//! E writes the global and calls B; F reads the global and loads the string.
//!
//! `eh_frame` lists every function in `.eh_frame`; without it only A and B are symbols, so D and E
//! are found by their prologue and F (no prologue, nothing calls it) is not found at all.

use crate::fixtures::{IMM, STRING, TABLE};

pub const BASE: u64 = 0x40_0000;

pub struct Arm {
    pub bytes: Vec<u8>,
    pub a: u64,
    pub b: u64,
    pub c: u64,
    pub d: u64,
    pub e: u64,
    pub f: u64,
    pub text_end: u64,
    pub string: u64,
    #[allow(dead_code)]
    pub table: u64,
    pub global: u64,
    /// Two 8-byte pointers, to B and to C.
    pub ptrs: u64,
}

pub const NOP: u32 = 0xd503_201f;
pub const RET: u32 = 0xd65f_03c0;
pub const STP_FRAME: u32 = 0xa9bf_7bfd; // stp x29, x30, [sp, #-16]!
pub const MOV_FP: u32 = 0x9100_03fd; // mov x29, sp
pub const LDP_FRAME: u32 = 0xa8c1_7bfd; // ldp x29, x30, [sp], #16

pub fn bl(from: u64, to: u64) -> u32 {
    0x9400_0000 | ((((to as i64 - from as i64) >> 2) as u32) & 0x03ff_ffff)
}

pub fn adrp(rd: u32, from: u64, to: u64) -> u32 {
    let pages = ((to >> 12) as i64 - (from >> 12) as i64) as u32;
    0x9000_0000 | ((pages & 3) << 29) | (((pages >> 2) & 0x7_ffff) << 5) | rd
}

pub fn add_imm(rd: u32, rn: u32, imm: u32) -> u32 {
    0x9100_0000 | (imm << 10) | (rn << 5) | rd
}

pub fn mov_w(rd: u32, imm: u32) -> u32 {
    0x5280_0000 | (imm << 5) | rd
}

pub fn ldr_w(rt: u32, rn: u32, off: u32) -> u32 {
    0xb940_0000 | ((off / 4) << 10) | (rn << 5) | rt
}

pub fn str_w(rt: u32, rn: u32, off: u32) -> u32 {
    0xb900_0000 | ((off / 4) << 10) | (rn << 5) | rt
}

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn put(v: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    if v.len() < at {
        v.resize(at, 0);
    }
    v.truncate(at);
    v.extend_from_slice(bytes);
}

pub fn elf(eh_frame: bool) -> Arm {
    let (text_off, eh_off, data_off, sym_off) = (0x100usize, 0x200usize, 0x1000usize, 0x1100usize);
    let a = BASE + text_off as u64;
    let (b, c, d, e, f) = (a + 0x20, a + 0x40, a + 0x50, a + 0x60, a + 0x80);
    let data_addr = BASE + data_off as u64;
    let (table, string, global, ptrs) = (data_addr, data_addr + 16, data_addr + 32, data_addr + 40);
    let lo = |x: u64| (x & 0xfff) as u32;

    let mut text = Vec::new();
    put(&mut text, 0, &words(&[STP_FRAME, MOV_FP, bl(a + 8, b), bl(a + 12, c), LDP_FRAME, RET]));
    put(
        &mut text,
        0x20,
        &words(&[adrp(0, b, string), add_imm(0, 0, lo(string)), mov_w(0, IMM), RET, NOP, NOP, NOP, NOP]),
    );
    put(&mut text, 0x40, &words(&[mov_w(0, 0), RET, NOP, NOP]));
    // D: a frame, nothing else
    put(&mut text, 0x50, &words(&[0xa9be_7bfd, 0xa8c2_7bfd, RET, NOP])); // stp x29,x30,[sp,#-32]!; ldp ..,#32
    put(
        &mut text,
        0x60,
        &words(&[
            STP_FRAME,
            adrp(8, e + 4, global),
            mov_w(9, 7),
            str_w(9, 8, lo(global)),
            bl(e + 16, b),
            LDP_FRAME,
            RET,
            NOP,
        ]),
    );
    put(
        &mut text,
        0x80,
        &words(&[adrp(8, f, global), ldr_w(0, 8, lo(global)), adrp(1, f + 8, string), add_imm(1, 1, lo(string)), RET]),
    );
    let text_end = a + text.len() as u64;

    // .eh_frame: one CIE (zR, pc-relative sdata4) and one FDE per function
    let mut eh: Vec<u8> = Vec::new();
    let funcs = [(a, 24u64), (b, 32), (c, 16), (d, 16), (e, 32), (f, 20)];
    let u32le = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    u32le(&mut eh, 16);
    u32le(&mut eh, 0);
    eh.extend_from_slice(&[1, b'z', b'R', 0, 1, 0x78, 30, 1, 0x1b, 0, 0, 0]);
    for (start, size) in funcs {
        let field = eh.len() as u64 + 8;
        let cie_ptr = eh.len() as u32 + 4;
        u32le(&mut eh, 16);
        u32le(&mut eh, cie_ptr);
        u32le(&mut eh, (start as i64 - (BASE + eh_off as u64 + field) as i64) as i32 as u32);
        u32le(&mut eh, size as u32);
        eh.extend_from_slice(&[0, 0, 0, 0]);
    }
    u32le(&mut eh, 0);

    let mut data = Vec::new();
    for v in TABLE {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(STRING.as_bytes());
    put(&mut data, 32, &0u32.to_le_bytes());
    put(&mut data, 40, &b.to_le_bytes());
    data.extend_from_slice(&c.to_le_bytes());

    // .symtab: null, main, helper
    let mut sym = vec![0u8; 24];
    for (name, value, size) in [(1u32, a, 24u64), (6, b, 32)] {
        sym.extend_from_slice(&name.to_le_bytes());
        sym.extend_from_slice(&[0x12, 0, 1, 0]);
        sym.extend_from_slice(&value.to_le_bytes());
        sym.extend_from_slice(&size.to_le_bytes());
    }
    let strtab = b"\0main\0helper\0";
    let shstr = b"\0.text\0.eh_frame\0.data\0.symtab\0.strtab\0.shstrtab\0";
    let strtab_off = sym_off + sym.len();
    let shstr_off = strtab_off + strtab.len();
    let sh_off = (shstr_off + shstr.len() + 7) & !7;

    let mut out = Vec::new();
    let p16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let p32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    let p64 = |v: &mut Vec<u8>, x: u64| v.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    p16(&mut out, 2);
    p16(&mut out, 183); // EM_AARCH64
    p32(&mut out, 1);
    p64(&mut out, a);
    p64(&mut out, 64);
    p64(&mut out, sh_off as u64);
    p32(&mut out, 0);
    for x in [64u16, 56, 3, 64, 7, 6] {
        p16(&mut out, x);
    }
    let code_len = if eh_frame { eh_off + eh.len() } else { text_off + text.len() } as u64;
    for (ty, flags, off, len) in
        [(1u32, 5u32, 0u64, code_len), (1, 6, data_off as u64, data.len() as u64), (0x6474_e551, 6, 0, 0)]
    {
        let addr = if ty == 0x6474_e551 { 0 } else { BASE + off };
        p32(&mut out, ty);
        p32(&mut out, flags);
        for x in [off, addr, addr, len, len, 1] {
            p64(&mut out, x);
        }
    }
    put(&mut out, text_off, &text);
    if eh_frame {
        put(&mut out, eh_off, &eh);
    }
    put(&mut out, data_off, &data);
    put(&mut out, sym_off, &sym);
    put(&mut out, strtab_off, strtab);
    put(&mut out, shstr_off, shstr);
    out.resize(sh_off, 0);
    let eh_len = if eh_frame { eh.len() } else { 0 };
    for (name, ty, flags, addr, off, size, link, info, entsize) in [
        (0u32, 0u32, 0u64, 0u64, 0usize, 0usize, 0u32, 0u32, 0u64),
        (1, 1, 6, a, text_off, text.len(), 0, 0, 0),
        (7, 1, 2, BASE + eh_off as u64, eh_off, eh_len, 0, 0, 0),
        (17, 1, 3, data_addr, data_off, data.len(), 0, 0, 0),
        (23, 2, 0, 0, sym_off, sym.len(), 5, 1, 24),
        (31, 3, 0, 0, strtab_off, strtab.len(), 0, 0, 0),
        (39, 3, 0, 0, shstr_off, shstr.len(), 0, 0, 0),
    ] {
        p32(&mut out, name);
        p32(&mut out, ty);
        p64(&mut out, flags);
        p64(&mut out, addr);
        p64(&mut out, off as u64);
        p64(&mut out, size as u64);
        p32(&mut out, link);
        p32(&mut out, info);
        p64(&mut out, 1);
        p64(&mut out, entsize);
    }
    Arm { bytes: out, a, b, c, d, e, f, text_end, string, table, global, ptrs }
}
