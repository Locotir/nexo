//! Tiny ELF64 and PE32+ files built byte by byte, for tests. No binary file is committed.
//!
//! Both hold the same three functions, A (the entry) calls B and C; B loads [`STRING`] by a
//! RIP-relative `lea` and returns 1337; C returns 0 (in the PE it first calls through the import
//! slot of `ExitProcess`). A data section holds [`TABLE`] and, in the ELF, the string too.
//! `hidden` adds a fourth function D that nothing calls: only the exception data (PE `.pdata`,
//! ELF `.eh_frame`) lists it.

/// The text the fixtures hold.
pub const STRING: &str = "hello nexo bin";
/// The table of u32 in the data section.
pub const TABLE: [u32; 4] = [10, 20, 30, 0x1234_5678];
/// The number B returns (`mov eax, 0x539`).
pub const IMM: u32 = 1337;

/// A built file and the addresses worth asserting on.
pub struct Fixture {
    pub bytes: Vec<u8>,
    pub a: u64,
    pub b: u64,
    pub c: u64,
    /// Only with `hidden`.
    pub d: Option<u64>,
    /// One past the last byte of .text.
    pub text_end: u64,
    pub string: u64,
    pub table: u64,
    pub image_base: u64,
}

#[derive(Default)]
struct Buf(Vec<u8>);

impl Buf {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    fn pad_to(&mut self, len: usize) -> &mut Self {
        assert!(self.0.len() <= len, "layout overlaps: {} > {len}", self.0.len());
        self.0.resize(len, 0);
        self
    }
}

fn rel32(from_next: u64, to: u64) -> u32 {
    (to as i64 - from_next as i64) as i32 as u32
}

/// Machine code for A, B, C (and D) placed at `base`. `iat` is the address of the import slot C calls
/// through, when there is one.
fn code(base: u64, string: u64, iat: Option<u64>, hidden: bool) -> (Vec<u8>, u64, u64, Option<u64>) {
    let (a, b, c) = (base, base + 16, base + 32);
    let mut t = Buf::default();
    // A: push rbp ; mov rbp, rsp ; call B ; call C ; pop rbp ; ret   (16 bytes)
    t.u8(0x55).bytes(&[0x48, 0x89, 0xe5]);
    t.u8(0xe8).u32(rel32(a + 4 + 5, b));
    t.u8(0xe8).u32(rel32(a + 9 + 5, c));
    t.u8(0x5d).u8(0xc3);
    // B: lea rdi, [rip+string] ; mov eax, 1337 ; ret ; 3 bytes of int3 padding   (16 bytes)
    t.bytes(&[0x48, 0x8d, 0x3d]).u32(rel32(b + 7, string));
    t.u8(0xb8).u32(IMM);
    t.u8(0xc3).bytes(&[0xcc, 0xcc, 0xcc]);
    // C: [call qword ptr [rip+iat] ;] xor eax, eax ; ret
    let mut at = c;
    if let Some(slot) = iat {
        t.bytes(&[0xff, 0x15]).u32(rel32(c + 6, slot));
        at += 6;
    }
    t.bytes(&[0x31, 0xc0, 0xc3]);
    at += 3;
    // D: nop ; ret, called by nobody
    let d = hidden.then(|| {
        t.bytes(&[0x90, 0xc3]);
        at
    });
    (t.0, a, c, d)
}

/// A minimal ELF64 x86-64 executable: .text, .eh_frame, .data, .symtab (main, helper), .strtab,
/// .shstrtab; three program headers (code, data, GNU_STACK).
pub fn elf64(hidden: bool) -> Fixture {
    const BASE: u64 = 0x40_0000;
    let (text_off, eh_off, data_off, sym_off) = (0x100usize, 0x180usize, 0x200usize, 0x240usize);
    let data_addr = BASE + data_off as u64;
    let string = data_addr + 16;
    let (text, a, c, d) = code(BASE + text_off as u64, string, None, hidden);
    let b = a + 16;
    let text_end = a + text.len() as u64;

    // .eh_frame: one CIE (version 1, "zR", FDE pointers are pc-relative 4-byte signed) and one FDE per function
    let funcs: Vec<(u64, u64)> = {
        let mut v = vec![(a, 16u64), (b, 16), (c, 3)];
        if let Some(d) = d {
            v.push((d, 2));
        }
        v
    };
    let mut eh = Buf::default();
    eh.u32(16).u32(0).u8(1).bytes(b"zR\0").u8(1).u8(0x78).u8(16).u8(1).u8(0x1b).bytes(&[0, 0, 0]);
    for (start, size) in &funcs {
        let field = eh.0.len() as u64 + 8; // the pc_begin field: after length and CIE pointer
        let cie_ptr = eh.0.len() as u32 + 4;
        eh.u32(16).u32(cie_ptr);
        eh.u32(rel32(BASE + eh_off as u64 + field, *start));
        eh.u32(*size as u32).u8(0).bytes(&[0, 0, 0]);
    }
    eh.u32(0);

    // .data: the table, then the string
    let mut data = Buf::default();
    for v in TABLE {
        data.u32(v);
    }
    data.bytes(STRING.as_bytes()).u8(0);

    // .symtab: null, main, helper
    let mut sym = Buf::default();
    sym.pad_to(24);
    for (name, value, size) in [(1u32, a, 16u64), (6, b, 13)] {
        sym.u32(name).u8(0x12).u8(0).u16(1).u64(value).u64(size);
    }
    let strtab = b"\0main\0helper\0";
    let shstr = b"\0.text\0.eh_frame\0.data\0.symtab\0.strtab\0.shstrtab\0";
    let (strtab_off, shstr_off, sh_off) = (sym_off + sym.0.len(), 0x2a0usize, 0x300usize);
    assert!(strtab_off + strtab.len() <= shstr_off);

    let mut f = Buf::default();
    // ELF header
    f.bytes(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]).pad_to(16);
    f.u16(2).u16(62).u32(1); // ET_EXEC, x86-64, version
    f.u64(a).u64(64).u64(sh_off as u64); // entry, phoff, shoff
    f.u32(0).u16(64).u16(56).u16(3).u16(64).u16(7).u16(6); // flags, ehsize, phentsize, phnum, shentsize, shnum, shstrndx
                                                           // program headers: code (R+X), data (R+W), GNU_STACK (R+W: not executable)
    let code_len = (eh_off + eh.0.len()) as u64;
    for (ty, flags, off, len) in
        [(1u32, 5u32, 0u64, code_len), (1, 6, data_off as u64, data.0.len() as u64), (0x6474_e551, 6, 0, 0)]
    {
        let addr = if ty == 0x6474_e551 { 0 } else { BASE + off };
        f.u32(ty).u32(flags).u64(off).u64(addr).u64(addr).u64(len).u64(len).u64(1);
    }
    f.pad_to(text_off).bytes(&text);
    f.pad_to(eh_off).bytes(&eh.0);
    f.pad_to(data_off).bytes(&data.0);
    f.pad_to(sym_off).bytes(&sym.0);
    f.pad_to(strtab_off).bytes(strtab);
    f.pad_to(shstr_off).bytes(shstr);
    f.pad_to(sh_off);
    // section headers: name, type, flags, addr, offset, size, link, info, align, entsize
    let mut sh =
        |name: u32, ty: u32, flags: u64, addr: u64, off: usize, size: usize, link: u32, info: u32, entsize: u64| {
            f.u32(name)
                .u32(ty)
                .u64(flags)
                .u64(addr)
                .u64(off as u64)
                .u64(size as u64)
                .u32(link)
                .u32(info)
                .u64(1)
                .u64(entsize);
        };
    sh(0, 0, 0, 0, 0, 0, 0, 0, 0);
    sh(1, 1, 6, a, text_off, text.len(), 0, 0, 0);
    sh(7, 1, 2, BASE + eh_off as u64, eh_off, eh.0.len(), 0, 0, 0);
    sh(17, 1, 3, data_addr, data_off, data.0.len(), 0, 0, 0);
    sh(23, 2, 0, 0, sym_off, sym.0.len(), 5, 1, 24);
    sh(31, 3, 0, 0, strtab_off, strtab.len(), 0, 0, 0);
    sh(39, 3, 0, 0, shstr_off, shstr.len(), 0, 0, 0);
    Fixture { bytes: f.0, a, b, c, d, text_end, string, table: data_addr, image_base: BASE }
}

/// A minimal PE32+ x86-64 executable: .text, .rdata (the string and an unwind record), .data,
/// .idata (KERNEL32!ExitProcess) and .pdata.
pub fn pe64(hidden: bool) -> Fixture {
    const BASE: u64 = 0x1_4000_0000;
    let (text_rva, rdata_rva, data_rva, idata_rva, pdata_rva) = (0x1000u64, 0x2000u64, 0x3000u64, 0x4000u64, 0x5000u64);
    let string = BASE + rdata_rva;
    let iat = BASE + idata_rva + 0x50;
    let (text, a, c, d) = code(BASE + text_rva, string, Some(iat), hidden);
    let b = a + 16;
    let text_end = a + text.len() as u64;

    let mut rdata = Buf::default();
    rdata.bytes(STRING.as_bytes()).u8(0).pad_to(0x10);
    rdata.bytes(&[0x01, 0, 0, 0]); // unwind info: version 1, no flags, no codes
    let unwind_rva = (rdata_rva + 0x10) as u32;

    let mut data = Buf::default();
    for v in TABLE {
        data.u32(v);
    }

    let mut idata = Buf::default();
    // descriptor: lookup table, time stamp, forwarder, name, address table; then the null descriptor
    idata.u32((idata_rva + 0x40) as u32).u32(0).u32(0).u32((idata_rva + 0x60) as u32).u32((idata_rva + 0x50) as u32);
    idata.pad_to(0x28);
    idata.pad_to(0x40).u64(idata_rva + 0x70).u64(0); // lookup table
    idata.pad_to(0x50).u64(idata_rva + 0x70).u64(0); // address table (the loader overwrites it)
    idata.pad_to(0x60).bytes(b"KERNEL32.dll\0");
    idata.pad_to(0x70).u16(0).bytes(b"ExitProcess\0");

    let mut pdata = Buf::default();
    let mut ents = vec![(text_rva, 16u32), (text_rva + 16, 16), (text_rva + 32, 9)];
    if hidden {
        ents.push((text_rva + 41, 2));
    }
    for (begin, len) in ents {
        pdata.u32(begin as u32).u32(begin as u32 + len).u32(unwind_rva);
    }

    // sections: name, rva, bytes, characteristics
    let secs: [(&[u8; 8], u64, &Vec<u8>, u32); 5] = [
        (b".text\0\0\0", text_rva, &text, 0x6000_0020),
        (b".rdata\0\0", rdata_rva, &rdata.0, 0x4000_0040),
        (b".data\0\0\0", data_rva, &data.0, 0xc000_0040),
        (b".idata\0\0", idata_rva, &idata.0, 0xc000_0040),
        (b".pdata\0\0", pdata_rva, &pdata.0, 0x4000_0040),
    ];
    let headers_len = 0x400usize;
    let raw_len = |n: usize| n.div_ceil(0x200) * 0x200;

    let mut f = Buf::default();
    // DOS header: "MZ" and the offset of the PE header at 0x3c
    f.bytes(b"MZ").pad_to(0x3c).u32(0x40);
    f.pad_to(0x40).bytes(b"PE\0\0");
    // COFF header: x86-64, 5 sections, optional header 0xf0 bytes, executable + large-address-aware
    f.u16(0x8664).u16(5).u32(0).u32(0).u32(0).u16(0xf0).u16(0x0022);
    // optional header (PE32+)
    f.u16(0x20b).u8(2).u8(0).u32(0x200).u32(0x600).u32(0);
    f.u32(text_rva as u32).u32(text_rva as u32); // entry point, base of code
    f.u64(BASE).u32(0x1000).u32(0x200); // image base, section alignment, file alignment
    f.u16(6).u16(0).u16(0).u16(0).u16(6).u16(0).u32(0); // OS, image, subsystem versions, win32 version
    f.u32(0x6000).u32(headers_len as u32).u32(0); // size of image, size of headers, checksum
    f.u16(3).u16(0x8160); // console, dynamic base + high entropy + NX compatible + terminal server aware
    f.u64(0x10_0000).u64(0x1000).u64(0x10_0000).u64(0x1000).u32(0).u32(16);
    for i in 0..16 {
        match i {
            1 => f.u32(idata_rva as u32).u32(40),
            3 => f.u32(pdata_rva as u32).u32(pdata.0.len() as u32),
            _ => f.u32(0).u32(0),
        };
    }
    // section table
    let mut raw_at = headers_len;
    for (name, rva, bytes, chars) in &secs {
        f.bytes(*name).u32(bytes.len() as u32).u32(*rva as u32).u32(raw_len(bytes.len()) as u32).u32(raw_at as u32);
        f.u32(0).u32(0).u16(0).u16(0).u32(*chars);
        raw_at += raw_len(bytes.len());
    }
    let mut at = headers_len;
    for (_, _, bytes, _) in &secs {
        f.pad_to(at).bytes(bytes);
        at += raw_len(bytes.len());
        f.pad_to(at);
    }
    Fixture { bytes: f.0, a, b, c, d, text_end, string, table: BASE + data_rva, image_base: BASE }
}
