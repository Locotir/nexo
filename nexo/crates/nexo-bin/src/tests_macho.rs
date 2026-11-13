//! Mach-O programs built byte by byte: a thin x86-64 one (classic bind info and an exports trie), a thin
//! arm64 one (chained fixups), stripped variants and a universal file holding both. Each has `main`
//! (calls `puts` through a stub, then `helper`), `helper`, `spare` (nothing calls it: only
//! LC_FUNCTION_STARTS and the symbol table list it), one `__got` slot and one string.

use std::path::PathBuf;

use crate::*;

const BASE: u64 = 0x1_0000_0000;
/// `main`, the first byte of `__TEXT,__text` (file offset 0x400).
pub(crate) const MAIN: u64 = BASE + 0x400;
pub(crate) const HELPER: u64 = MAIN + 0x28;
pub(crate) const SPARE: u64 = MAIN + 0x30;
/// The one stub, in `__TEXT,__stubs`.
pub(crate) const STUB: u64 = MAIN + 0x40;
pub(crate) const STRING_AT: u64 = MAIN + 0x50;
pub(crate) const HELLO: &str = "hello mach-o";
/// The `__DATA_CONST,__got` slot of `puts`.
pub(crate) const GOT: u64 = BASE + 0x1000;
const LINKEDIT: usize = 0x2000;

/// A built file and where its parts are.
pub(crate) struct MachoFx {
    pub(crate) bytes: Vec<u8>,
    /// (file offset, size) of `__LINKEDIT`.
    pub(crate) linkedit: (usize, usize),
    /// File offset of the indirect symbol table.
    pub(crate) indirect: usize,
}

fn put(b: &mut Vec<u8>, at: usize, v: &[u8]) {
    if b.len() < at + v.len() {
        b.resize(at + v.len(), 0);
    }
    b[at..at + v.len()].copy_from_slice(v);
}

fn name16(s: &str) -> [u8; 16] {
    let mut n = [0u8; 16];
    n[..s.len()].copy_from_slice(s.as_bytes());
    n
}

fn pad8(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(8) {
        v.push(0);
    }
}

/// One `section_64`: (section, segment, address, size, file offset, flags, reserved1, reserved2).
type Sect = (&'static str, &'static str, u64, u64, u32, u32, u32, u32);

fn segment(c: &mut Vec<u8>, name: &str, (vmaddr, vmsize): (u64, u64), (off, size): (u64, u64), prot: u32, s: &[Sect]) {
    c.extend(0x19u32.to_le_bytes());
    c.extend((72 + 80 * s.len() as u32).to_le_bytes());
    c.extend(name16(name));
    for v in [vmaddr, vmsize, off, size] {
        c.extend(v.to_le_bytes());
    }
    for v in [prot, prot, s.len() as u32, 0] {
        c.extend(v.to_le_bytes());
    }
    for &(sect, seg, addr, size, off, flags, r1, r2) in s {
        c.extend(name16(sect));
        c.extend(name16(seg));
        c.extend(addr.to_le_bytes());
        c.extend(size.to_le_bytes());
        for v in [off, 2, 0, 0, flags, r1, r2, 0] {
            c.extend(v.to_le_bytes());
        }
    }
}

fn cmd(c: &mut Vec<u8>, id: u32, words: &[u32]) {
    c.extend(id.to_le_bytes());
    c.extend((8 + 4 * words.len() as u32).to_le_bytes());
    for w in words {
        c.extend(w.to_le_bytes());
    }
}

fn x86_code() -> Vec<u8> {
    let rel = |to: u64, next: u64| ((to as i64 - next as i64) as i32).to_le_bytes();
    let mut t = vec![0x55, 0x48, 0x89, 0xe5, 0x48, 0x8d, 0x3d];
    t.extend(rel(STRING_AT, MAIN + 11));
    t.push(0xe8);
    t.extend(rel(STUB, MAIN + 16));
    t.push(0xe8);
    t.extend(rel(HELPER, MAIN + 21));
    t.extend([0x31, 0xc0, 0x5d, 0xc3]);
    t.resize((HELPER - MAIN) as usize, 0xcc);
    t.extend([0xb8, 42, 0, 0, 0, 0xc3, 0xcc, 0xcc]);
    t.extend([0xb8, 7, 0, 0, 0, 0xc3]);
    t
}

fn arm64_code() -> Vec<u8> {
    let bl = |at: u64, to: u64| 0x9400_0000 | ((to.wrapping_sub(at) / 4) as u32 & 0x03ff_ffff);
    let words = [
        0xa9bf_7bfd,                                        // stp x29, x30, [sp, #-16]!
        0x9100_03fd,                                        // mov x29, sp
        0x9000_0000,                                        // adrp x0, the string's page
        0x9100_0000 | (((STRING_AT & 0xfff) as u32) << 10), // add x0, x0, the string's offset
        bl(MAIN + 0x10, STUB),                              // bl puts
        bl(MAIN + 0x14, HELPER),                            // bl helper
        0x5280_0000,                                        // mov w0, #0
        0xa8c1_7bfd,                                        // ldp x29, x30, [sp], #16
        0xd65f_03c0,                                        // ret
        0xd503_201f,                                        // nop
        0x5280_0540,                                        // helper: mov w0, #42
        0xd65f_03c0,                                        // ret
        0x5280_00e0,                                        // spare: mov w0, #7
        0xd65f_03c0,                                        // ret
    ];
    words.iter().flat_map(|w: &u32| w.to_le_bytes()).collect()
}

fn uleb(v: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            v.push(b);
            return;
        }
        v.push(b | 0x80);
    }
}

/// A thin program: x86-64 with bind opcodes and an exports trie, or arm64 with chained fixups.
/// `stripped` keeps only the undefined `_puts` in the symbol table.
pub(crate) fn thin(arm64: bool, stripped: bool) -> MachoFx {
    let code = if arm64 { arm64_code() } else { x86_code() };
    let stub: Vec<u8> = if arm64 {
        // adrp x16, the slot's page; ldr x16, [x16, the slot's offset]; br x16
        let page = ((GOT >> 12) - (STUB >> 12)) as u32;
        let ldr = 0xf940_0210 | ((((GOT & 0xfff) / 8) as u32) << 10);
        [0x9000_0010 | ((page & 3) << 29) | ((page >> 2) << 5), ldr, 0xd61f_0200]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect()
    } else {
        let mut s = vec![0xff, 0x25];
        s.extend(((GOT - (STUB + 6)) as u32).to_le_bytes());
        s
    };

    // __LINKEDIT
    let mut le: Vec<u8> = Vec::new();
    let mut fixups = (0, 0);
    if arm64 {
        // header, starts in image (4 segments, only __DATA_CONST has fixups), its segment starts, one import
        let mut f = Vec::new();
        for w in [0u32, 0x20, 0x50, 0x54, 1, 1, 0, 0] {
            f.extend(w.to_le_bytes());
        }
        for w in [4u32, 0, 0, 0x18, 0, 0] {
            f.extend(w.to_le_bytes());
        }
        f.extend(24u32.to_le_bytes());
        f.extend(0x1000u16.to_le_bytes());
        f.extend(6u16.to_le_bytes()); // DYLD_CHAINED_PTR_64_OFFSET
        f.extend((GOT - BASE).to_le_bytes());
        f.extend(0u32.to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(0u16.to_le_bytes());
        assert_eq!(f.len(), 0x50);
        f.extend((1u32 | (1 << 9)).to_le_bytes()); // library 1, name at 1
        f.extend(b"\0_puts\0");
        pad8(&mut f);
        fixups = (LINKEDIT + le.len(), f.len());
        le.extend(f);
    }
    let starts_at = LINKEDIT + le.len();
    for d in [0x400, 0x28, 0x8, 0] {
        uleb(&mut le, d);
    }
    pad8(&mut le);
    let starts = (starts_at, LINKEDIT + le.len() - starts_at);
    let (mut bind, mut exports) = ((0, 0), (0, 0));
    if !arm64 {
        let at = LINKEDIT + le.len();
        // library 1, symbol _puts, pointer, segment 2 offset 0, bind, done
        le.extend([0x11, 0x40]);
        le.extend(b"_puts\0");
        le.extend([0x51, 0x72, 0x00, 0x90, 0x00]);
        pad8(&mut le);
        bind = (at, LINKEDIT + le.len() - at);
        let at = LINKEDIT + le.len();
        // the root has one edge, `_main`, to a terminal node: flags 0, offset 0x400
        le.extend([0x00, 0x01]);
        le.extend(b"_main\0");
        le.extend([0x09, 0x03, 0x00, 0x80, 0x08, 0x00]);
        pad8(&mut le);
        exports = (at, LINKEDIT + le.len() - at);
    }
    let mut strtab = vec![0u8];
    let mut syms: Vec<u8> = Vec::new();
    let mut sym = |name: &str, ntype: u8, sect: u8, desc: u16, value: u64| {
        syms.extend((strtab.len() as u32).to_le_bytes());
        syms.extend([ntype, sect]);
        syms.extend(desc.to_le_bytes());
        syms.extend(value.to_le_bytes());
        strtab.extend(name.as_bytes());
        strtab.push(0);
    };
    let (nlocal, ndef) = if stripped {
        (0, 0)
    } else {
        sym("_helper", 0x0e, 1, 0, HELPER);
        sym("_spare", 0x0e, 1, 0, SPARE);
        sym("_main", 0x0f, 1, 0, MAIN);
        (2u32, 1u32)
    };
    sym("_puts", 0x01, 0, 0x0100, 0);
    pad8(&mut strtab);
    let nsyms = nlocal + ndef + 1;
    let symoff = LINKEDIT + le.len();
    le.extend(syms);
    let indirect = LINKEDIT + le.len();
    let puts = nsyms - 1;
    // the stub, then the __got slot
    for i in [puts, puts] {
        le.extend(i.to_le_bytes());
    }
    let stroff = LINKEDIT + le.len();
    le.extend(&strtab);
    pad8(&mut le);

    // load commands
    let mut c: Vec<u8> = Vec::new();
    let text_flags = 0x8000_0400;
    let stub_size = stub.len() as u32;
    segment(&mut c, "__PAGEZERO", (0, BASE), (0, 0), 0, &[]);
    segment(
        &mut c,
        "__TEXT",
        (BASE, 0x1000),
        (0, 0x1000),
        5,
        &[
            ("__text", "__TEXT", MAIN, code.len() as u64, 0x400, text_flags, 0, 0),
            ("__stubs", "__TEXT", STUB, u64::from(stub_size), 0x440, text_flags | 0x8, 0, stub_size),
            ("__cstring", "__TEXT", STRING_AT, HELLO.len() as u64 + 1, 0x450, 0x2, 0, 0),
        ],
    );
    segment(
        &mut c,
        "__DATA_CONST",
        (GOT, 0x1000),
        (0x1000, 0x1000),
        3,
        &[("__got", "__DATA_CONST", GOT, 8, 0x1000, 0x6, 1, 0)],
    );
    segment(&mut c, "__LINKEDIT", (BASE + 0x2000, 0x1000), (LINKEDIT as u64, le.len() as u64), 1, &[]);
    let mut ncmds = 4;
    let mut add = |c: &mut Vec<u8>, id: u32, words: &[u32]| {
        cmd(c, id, words);
        ncmds += 1;
    };
    add(&mut c, 0x2, &[symoff as u32, nsyms, stroff as u32, strtab.len() as u32]);
    let mut dysym = [0u32; 18];
    dysym[..6].copy_from_slice(&[0, nlocal, nlocal, ndef, nlocal + ndef, 1]);
    dysym[12] = indirect as u32;
    dysym[13] = 2;
    add(&mut c, 0xb, &dysym);
    add(&mut c, 0x8000_0028, &[0x400, 0, 0, 0]);
    let mut lib = vec![24u32, 2, 0x0527_0000, 0x0001_0000];
    let mut path = b"/usr/lib/libSystem.B.dylib\0".to_vec();
    pad8(&mut path);
    lib.extend(path.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)));
    add(&mut c, 0xc, &lib);
    add(&mut c, 0x26, &[starts.0 as u32, starts.1 as u32]);
    if arm64 {
        add(&mut c, 0x8000_0034, &[fixups.0 as u32, fixups.1 as u32]);
    } else {
        add(&mut c, 0x8000_0022, &[0, 0, bind.0 as u32, bind.1 as u32, 0, 0, 0, 0, exports.0 as u32, exports.1 as u32]);
    }
    add(&mut c, 0x32, &[1, 0x000e_0000, 0x000e_0000, 0]);
    add(&mut c, 0x1b, &[0x0403_0201, 0x0807_0605, 0x0c0b_0a09, 0x100f_0e0d]);

    let mut b = Vec::new();
    let (cpu, sub) = if arm64 { (0x0100_000c_u32, 0u32) } else { (0x0100_0007, 3) };
    for w in [0xfeed_facf_u32, cpu, sub, 2, ncmds, c.len() as u32, 0x0020_0085, 0] {
        b.extend(w.to_le_bytes());
    }
    assert!(32 + c.len() <= 0x400, "the load commands fit before __text");
    put(&mut b, 32, &c);
    put(&mut b, 0x400, &code);
    put(&mut b, 0x440, &stub);
    put(&mut b, 0x450, HELLO.as_bytes());
    // the __got slot: a chained bind to import 0 (arm64), or zero for the bind opcodes to fill (x86-64)
    put(&mut b, 0x1000, &(u64::from(arm64) << 63).to_le_bytes());
    put(&mut b, LINKEDIT, &le);
    MachoFx { bytes: b, linkedit: (LINKEDIT, le.len()), indirect }
}

/// A universal file with these slices, each at a 4 KiB boundary; the last one ends the file.
pub(crate) fn fat(slices: &[(u32, &[u8])]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(0xcafe_babe_u32.to_be_bytes());
    b.extend((slices.len() as u32).to_be_bytes());
    let mut at = 0x1000usize;
    let mut placed = Vec::new();
    for &(cpu, s) in slices {
        for w in [cpu, if cpu == 0x0100_0007 { 3 } else { 0 }, at as u32, s.len() as u32, 12] {
            b.extend(w.to_be_bytes());
        }
        placed.push((at, s));
        at = (at + s.len()).next_multiple_of(0x1000);
    }
    for (at, s) in placed {
        put(&mut b, at, s);
    }
    b
}

/// x86-64 first, then arm64.
pub(crate) fn universal() -> Vec<u8> {
    fat(&[(0x0100_0007, &thin(false, false).bytes), (0x0100_000c, &thin(true, false).bytes)])
}

fn open(b: &[u8]) -> Image {
    let img = Image::from_bytes(b.to_vec(), PathBuf::from("/fx/prog")).expect("the fixture opens");
    assert!(img.warnings.is_empty(), "{:?}", img.warnings);
    img
}

fn text(o: &Output) -> String {
    o.page(None, 1000).text()
}

fn func<'a>(img: &'a Image, name: &str) -> &'a crate::funcs::Func {
    let names: Vec<&str> = img.funcs().iter().map(|f| f.name.as_str()).collect();
    img.funcs().iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no {name} in {names:?}"))
}

/// What every build of the program shows the same way, whatever its architecture.
fn common(img: &Image, arch: &str) {
    assert_eq!(img.format, Format::MachO);
    assert_eq!(img.arch_name(), arch);
    assert_eq!((img.image_base, img.entry), (BASE, MAIN));
    let names: Vec<&str> = img.funcs().iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["main", "helper", "spare", "puts@plt"], "one leading underscore is the C prefix, not the name");
    assert_eq!(func(img, "main").end, HELPER);
    assert_eq!(img.imports.len(), 1);
    let i = &img.imports[0];
    assert_eq!((i.name.as_str(), i.lib.as_deref(), i.slot), ("puts", Some("libSystem.B.dylib"), Some(GOT)));
    assert_eq!(i.raw, "_puts");
    let t = text(&info(img));
    assert!(t.contains(&format!("Mach-O 64-bit little-endian, {arch}")), "{t}");
    assert!(t.contains("entry       0x100000400 (main)"), "{t}");
    assert!(t.contains("libraries   libSystem.B.dylib"), "{t}");
    assert!(t.contains("pie: yes (position independent) | nx stack: yes | code signature: no | encrypted: no"), "{t}");
    assert!(t.contains("__TEXT,__text"), "{t}");
    let o = info(img);
    assert_eq!(o.meta["macho"]["main"], MAIN);
    assert_eq!(o.meta["macho"]["platform"], "macOS 14.0");

    // main calls the stub, and the stub is named after the import
    let f = text(&func_view(img, &FnQuery::new("main")).unwrap());
    assert!(f.contains("puts@plt") && f.contains("helper"), "{f}");
    let c = text(&calls(img, "main").unwrap());
    assert!(c.contains("puts@plt") && c.contains("helper"), "{c}");
    let x = text(&xref(img, "puts").unwrap());
    assert!(x.contains("main") && x.contains("puts@plt"), "{x}");
    let s = text(&strings(img, None).unwrap());
    assert!(s.contains(HELLO), "{s}");
    let hits = crate::mainfind::find_main(img);
    assert_eq!((hits[0].addr, hits[0].role, hits[0].strength), (MAIN, "main", "proven"), "{hits:?}");
    let o = text(&overview(img).unwrap());
    assert!(o.contains("main"), "{o}");
}

#[test]
fn an_x86_64_mach_o_program_is_read_with_its_stub_import_and_bind_info() {
    let img = open(&thin(false, false).bytes);
    common(&img, "x86-64");
    let t = text(&info(&img));
    assert!(t.contains("macho       executable for macOS 14.0; LC_MAIN main at 0x100000400; fixups classic; 3 function starts listed"), "{t}");
    assert!(t.contains("uuid        01020304-0506-0708-090a-0b0c0d0e0f10"), "{t}");
    let exports: Vec<(&str, u64)> = img.exports.iter().map(|e| (e.name.as_str(), e.addr)).collect();
    assert_eq!(exports, [("main", MAIN)], "the exports trie");
    let f = text(&func_view(&img, &FnQuery::new("main")).unwrap());
    assert!(f.contains("call") && f.contains("lea"), "{f}");
}

#[test]
fn an_arm64_mach_o_program_is_read_with_its_stub_import_and_chained_fixups() {
    let img = open(&thin(true, false).bytes);
    common(&img, "arm64");
    let t = text(&info(&img));
    assert!(t.contains("fixups chained; 3 function starts listed"), "{t}");
    let f = text(&func_view(&img, &FnQuery::new("main")).unwrap());
    assert!(f.contains("bl") && f.contains("adrp"), "{f}");
}

#[test]
fn chained_fixups_alone_give_the_import_its_slot() {
    // the indirect table says nothing: only the chained bind in __got names puts
    let mut fx = thin(true, false);
    for k in 0..2 {
        put(&mut fx.bytes, fx.indirect + 4 * k, &0x8000_0000u32.to_le_bytes());
    }
    let img = open(&fx.bytes);
    assert_eq!(img.imports[0].slot, Some(GOT));
    assert!(img.funcs().iter().any(|f| f.start == STUB && f.name == "puts@plt"), "the stub reads the slot");
}

#[test]
fn a_stripped_mach_o_program_still_has_main_its_functions_and_its_import() {
    for arm64 in [false, true] {
        let img = open(&thin(arm64, true).bytes);
        let names: Vec<(&str, u64)> = img.funcs().iter().map(|f| (f.name.as_str(), f.start)).collect();
        // spare: nothing calls it and no symbol names it; LC_FUNCTION_STARTS lists it
        let spare = format!("sub_{SPARE:x}");
        let helper = format!("sub_{HELPER:x}");
        assert_eq!(
            names,
            [("main", MAIN), (helper.as_str(), HELPER), (spare.as_str(), SPARE), ("puts@plt", STUB)],
            "arm64 {arm64}"
        );
        assert_eq!(img.imports[0].name, "puts");
    }
}

#[test]
fn a_universal_file_reads_its_first_slice_or_the_one_asked_for() {
    let b = universal();
    let img = open(&b);
    assert_eq!(img.arch_name(), "x86-64");
    let t = text(&info(&img));
    assert!(t.contains("slices      universal file: x86-64 (read; the sha256 is of this slice), arm64"), "{t}");
    assert!(t.contains("read another: nexo bin info /fx/prog --arch arm64"), "{t}");
    // the hash is of the slice, as other tools name it
    let thin_x86 = open(&thin(false, false).bytes);
    assert_eq!(img.sha256, thin_x86.sha256);
    assert_eq!(img.funcs().len(), thin_x86.funcs().len());

    let arm = Image::from_bytes_arch(b.clone(), PathBuf::from("/fx/prog"), Some("aarch64")).unwrap();
    assert_eq!(arm.arch_name(), "arm64");
    common(&arm, "arm64");
    let o = func_list(&arm, FuncSort::Addr, None, 0).unwrap();
    assert!(o.command.contains("--arch arm64"), "the next commands keep the slice: {}", o.command);

    let e =
        Image::from_bytes_arch(b.clone(), PathBuf::from("/fx/prog"), Some("ppc")).err().expect("refused").to_string();
    assert!(e.contains("write x86-64 or arm64"), "{e}");
    let e = Image::from_bytes_arch(b, PathBuf::from("/fx/prog"), Some("arm64e")).err().expect("refused").to_string();
    assert!(e.contains("x86-64, arm64; pick one of those"), "{e}");
}

#[test]
fn arch_on_a_thin_file_must_name_its_own_architecture() {
    let b = thin(true, false).bytes;
    assert!(Image::from_bytes_arch(b.clone(), PathBuf::from("/x"), Some("arm64")).is_ok());
    let e = Image::from_bytes_arch(b, PathBuf::from("/x"), Some("x86-64")).err().expect("refused").to_string();
    assert!(e.contains("this file holds arm64 code only"), "{e}");
}

#[test]
fn mach_o_files_are_taken_by_opens() {
    let dir = tempfile::tempdir().unwrap();
    for (name, b, yes) in [
        ("x86", thin(false, false).bytes, true),
        ("arm", thin(true, true).bytes, true),
        ("fat", universal(), true),
        (
            "obj",
            {
                let mut b = thin(false, false).bytes;
                b[12] = 1;
                b
            },
            false,
        ),
    ] {
        let p = dir.path().join(name);
        std::fs::write(&p, &b).unwrap();
        assert_eq!(crate::opens(&p), yes, "{name}");
    }
}

#[test]
fn mach_o_files_it_does_not_read_say_why() {
    let mut obj = thin(false, false).bytes;
    obj[12] = 1;
    let e = Image::from_bytes(obj, PathBuf::from("/x")).err().expect("refused").to_string();
    assert!(e.contains("object file (.o)"), "{e}");
    let mut ppc = thin(false, false).bytes;
    ppc[4..8].copy_from_slice(&0x0100_0012u32.to_le_bytes());
    let e = Image::from_bytes(ppc, PathBuf::from("/x")).err().expect("refused").to_string();
    assert!(e.contains("Mach-O ppc64 is not read yet"), "{e}");
    let ppc_only = fat(&[(0x0000_0012, &thin(false, false).bytes)]);
    let e = Image::from_bytes(ppc_only, PathBuf::from("/x")).err().expect("refused").to_string();
    assert!(e.contains("none of them is x86-64 or arm64"), "{e}");
}

#[test]
fn a_mach_o_header_with_no_load_commands_opens_and_is_empty() {
    let mut b = Vec::new();
    for w in [0xfeed_facf_u32, 0x0100_000c, 0, 2, 0, 0, 0, 0] {
        b.extend(w.to_le_bytes());
    }
    let img = Image::from_bytes(b, PathBuf::from("/x")).unwrap();
    assert!(img.funcs().is_empty() && img.sections.is_empty());
    assert!(text(&info(&img)).contains("no LC_MAIN"));
}
