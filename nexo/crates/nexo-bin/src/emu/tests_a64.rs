//! The AArch64 emulator against functions written word by word (assembled with llvm-mc), run inside the
//! small arm64 ELF the decoder tests use. Each function goes in as the fixture's extra function G.

use std::path::PathBuf;

use serde_json::Value;

use super::{emulate, EmuSpec};
use crate::a64::fixture::{adrp, bl, elf_with, Arm, BASE, LDP_FRAME, NOP, RET, STP_FRAME};
use crate::fixtures::IMM;
use crate::{Image, Import};

fn image(f: &Arm) -> Image {
    Image::from_bytes(f.bytes.clone(), PathBuf::from("/fx/prog")).expect("an arm64 image")
}

/// The fixture with `words` as function G, and G's address.
fn with(words: &[u32]) -> (Image, u64) {
    let f = elf_with(false, words);
    (image(&f), f.g)
}

fn run(img: &Image, at: u64, args: &[&str], edit: impl FnOnce(&mut EmuSpec)) -> (String, Value) {
    let mut spec = EmuSpec::new(&format!("{at:#x}"));
    spec.args = args.iter().map(|s| s.to_string()).collect();
    edit(&mut spec);
    let out = emulate(img, &spec).expect("emu answers");
    let page = out.page(None, 400);
    (out.text_all(), page.value)
}

fn kind(v: &Value) -> &str {
    v["stop"]["kind"].as_str().unwrap_or("?")
}

fn ret_of(words: &[u32], args: &[&str]) -> u64 {
    let (img, g) = with(words);
    let (text, v) = run(&img, g, args, |_| {});
    assert_eq!(kind(&v), "returned", "{text}");
    v["return"].as_u64().unwrap_or(u64::MAX)
}

#[test]
fn a_byte_sum_loop_over_a_string_argument() {
    // mov x1,#0; loop: ldrb w2,[x0],#1; cbz w2,done; add x1,x1,x2; b loop; done: mov x0,x1; ret
    let words = [0xd280_0001, 0x3840_1402, 0x3400_0062, 0x8b02_0021, 0x17ff_fffd, 0xaa01_03e0, RET];
    let (img, g) = with(&words);
    let (text, v) = run(&img, g, &["str:hello"], |_| {});
    assert_eq!(kind(&v), "returned", "{text}");
    assert_eq!(v["return"].as_u64(), Some(532), "{text}");
    assert_eq!(v["arch"].as_str(), Some("arm64"), "{text}");
    assert!(text.contains("arch arm64") && text.contains("x0="), "{text}");
}

#[test]
fn w_registers_wrap_to_32_bits_and_set_the_overflow_flag() {
    // add w0,w0,w1; ret
    let add = [0x0b01_0000, RET];
    assert_eq!(ret_of(&add, &["0xffffffff", "1"]), 0, "the sum wraps and the upper half is zero");
    assert_eq!(ret_of(&add, &["0x7fffffff", "1"]), 0x8000_0000);
    // subs w0,w0,w1; cset w0,vs; ret
    let ovf = [0x6b01_0000, 0x1a9f_77e0, RET];
    assert_eq!(ret_of(&ovf, &["0x80000000", "1"]), 1, "INT_MIN - 1 overflows");
    assert_eq!(ret_of(&ovf, &["5", "3"]), 0);
}

#[test]
fn a_strcmp_like_check_returns_zero_or_one() {
    // loop: ldrb w2,[x0],#1; ldrb w3,[x1],#1; cmp w2,w3; b.ne out; cbnz w2,loop; out: cset w0,ne; ret
    let words = [0x3840_1402, 0x3840_1423, 0x6b03_005f, 0x5400_0041, 0x35ff_ff82, 0x1a9f_07e0, RET];
    assert_eq!(ret_of(&words, &["str:abc", "str:abc"]), 0);
    assert_eq!(ret_of(&words, &["str:abc", "str:abd"]), 1);
    assert_eq!(ret_of(&words, &["str:abc", "str:ab"]), 1);
}

#[test]
fn an_ldp_stp_frame_with_pre_and_post_index_leaves_sp_where_it_was() {
    // stp x29,x30,[sp,#-32]!; mov x29,sp; mov x0,#5; str x0,[sp,#16]; ldr x1,[sp,#16]; add x0,x1,#1;
    // ldp x29,x30,[sp],#32; ret
    let words = [0xa9be_7bfd, 0x9100_03fd, 0xd280_00a0, 0xf900_0be0, 0xf940_0be1, 0x9100_0420, 0xa8c2_7bfd, RET];
    let (img, g) = with(&words);
    let (text, v) = run(&img, g, &[], |_| {});
    assert_eq!(kind(&v), "returned", "{text}");
    assert_eq!(v["return"].as_u64(), Some(6), "{text}");
    let f = elf_with(false, &words);
    let (text2, v2) = run(&image(&f), f.c, &[], |_| {});
    assert_eq!(kind(&v2), "returned", "{text2}");
    assert_eq!(v["regs"]["sp"], v2["regs"]["sp"], "sp is back where it started: {text}");
}

#[test]
fn bl_to_a_local_function_returns_to_the_caller() {
    let probe = elf_with(false, &[NOP]);
    let (g, b) = (probe.g, probe.b);
    // stp frame; bl B; add x0,x0,#1; ldp frame; ret
    let words = [STP_FRAME, bl(g + 4, b), 0x9100_0400, LDP_FRAME, RET];
    assert_eq!(ret_of(&words, &[]), u64::from(IMM) + 1);
}

/// G calls through the GOT slot of an import with the argument it was given, then adds one.
fn through_got(name: &str) -> (Image, u64) {
    let slot = BASE + 0x1000 + 0x80;
    let probe = elf_with(false, &[NOP]);
    let g = probe.g;
    // stp frame; adrp x8,page; ldr x8,[x8,#0x80]; blr x8; add x0,x0,#1; ldp frame; ret
    let words = [
        STP_FRAME,
        adrp(8, g + 4, slot),
        0xf940_0108 | (((slot & 0xfff) as u32 / 8) << 10),
        0xd63f_0100,
        0x9100_0400,
        LDP_FRAME,
        RET,
    ];
    let f = elf_with(false, &words);
    let mut img = image(&f);
    img.imports.push(Import { lib: None, name: name.to_string(), raw: String::new(), slot: Some(slot) });
    (img, f.g)
}

#[test]
fn a_call_to_an_import_uses_stub_libc_or_stops_naming_it() {
    let (img, g) = through_got("foo");
    let (text, v) = run(&img, g, &[], |s| s.stubs = vec!["foo=77".into()]);
    assert_eq!(kind(&v), "returned", "{text}");
    assert_eq!(v["return"].as_u64(), Some(78), "{text}");
    assert!(text.contains("foo(...) = 0x4d (--stub)"), "{text}");

    let (img, g) = through_got("strlen");
    let (text, v) = run(&img, g, &["str:hello"], |_| {});
    assert_eq!(v["return"].as_u64(), Some(6), "strlen is the library stub: {text}");
    assert!(text.contains("strlen("), "{text}");

    let (img, g) = through_got("mystery");
    let (text, v) = run(&img, g, &[], |_| {});
    assert_eq!(kind(&v), "unknown_import", "{text}");
    assert!(text.contains("mystery") && text.contains("--stub mystery=0"), "{text}");
}

#[test]
fn svc_and_simd_stop_with_a_clear_message() {
    let (img, g) = with(&[0xd400_0001, RET]);
    let (text, v) = run(&img, g, &[], |_| {});
    assert_eq!(kind(&v), "unsupported", "{text}");
    assert!(text.contains("unsupported on arm64: svc"), "{text}");
    assert!(text.contains("nothing was skipped"), "{text}");
    // ldr q0,[x0]
    let (img, g) = with(&[0x3dc0_0000, RET]);
    let (text, v) = run(&img, g, &["buf:16"], |_| {});
    assert_eq!(kind(&v), "unsupported", "{text}");
    assert!(text.contains("unsupported on arm64:"), "{text}");
}

#[test]
fn the_x86_only_options_are_refused_on_arm64() {
    let (img, g) = with(&[RET]);
    let mut spec = EmuSpec::new(&format!("{g:#x}"));
    spec.hooks = vec![format!("{g:#x}=print")];
    assert!(emulate(&img, &spec).is_err());
}
