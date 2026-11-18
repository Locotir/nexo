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
fn svc_with_another_immediate_and_simd_stop_with_a_clear_message() {
    // svc #1 is not a Linux system call
    let (img, g) = with(&[0xd400_0021, RET]);
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
fn the_x86_only_abi_option_is_refused_on_arm64() {
    let (img, g) = with(&[RET]);
    let mut spec = EmuSpec::new(&format!("{g:#x}"));
    spec.abi = Some("sysv".into());
    assert!(emulate(&img, &spec).is_err());
}

/// mov x8,#64; mov x0,#1; adr x1,msg; mov x2,#3; svc #0; mov x8,#94; mov x0,#7; svc #0; msg: "hi\n\0"
const WRITE_EXIT: [u32; 9] = [
    0xd280_0808,
    0xd280_0020,
    0x1000_00c1,
    0xd280_0062,
    0xd400_0001,
    0xd280_0bc8,
    0xd280_00e0,
    0xd400_0001,
    0x000a_6968,
];

#[test]
fn svc_write_then_exit_group_gives_the_output_and_the_exit_code() {
    let (img, g) = with(&WRITE_EXIT);
    let (text, v) = run(&img, g, &[], |_| {});
    assert_eq!(kind(&v), "exit", "{text}");
    assert_eq!(v["exit"].as_i64(), Some(7), "{text}");
    assert_eq!(v["stdout"].as_str(), Some("hi\n"), "{text}");
    assert!(text.contains("syscall write(1, ") && text.contains("= 3"), "{text}");
    assert!(text.contains("syscall exit_group(7)"), "{text}");
}

#[test]
fn an_unknown_syscall_stops_with_its_number_and_name() {
    // mov x8,#999; svc #0; ret
    let (img, g) = with(&[0xd280_7ce8, 0xd400_0001, RET]);
    let (text, v) = run(&img, g, &[], |_| {});
    assert_eq!(kind(&v), "syscall", "{text}");
    assert!(text.contains("system call 999 (?)"), "{text}");
    // mov x8,#56 (openat); svc #0
    let (img, g) = with(&[0xd280_0708, 0xd400_0001, RET]);
    let (text, v) = run(&img, g, &[], |_| {});
    assert_eq!(kind(&v), "syscall", "{text}");
    assert!(text.contains("system call 56 (openat)"), "{text}");
}

#[test]
fn brk_grows_and_the_answer_comes_back_in_x0() {
    // mov x8,#214; mov x0,#0; movk x0,#0x7e80,lsl #32; movk x0,#0,lsl #48; add x0,x0,#1,lsl #12; svc #0;
    // mov x8,#94; svc #0   (exit_group(x0) shows the low 32 bits of the new break)
    let words =
        [0xd280_1ac8, 0xd280_0000, 0xf2cf_d000, 0xf2e0_0000, 0x9140_0400, 0xd400_0001, 0xd280_0bc8, 0xd400_0001];
    let (img, g) = with(&words);
    let (text, v) = run(&img, g, &[], |_| {});
    assert_eq!(kind(&v), "exit", "{text}");
    assert_eq!(v["exit"].as_i64(), Some(0x1000), "{text}");
    assert_eq!(v["regs"]["x0"].as_u64(), Some(0x7e80_0000_1000), "{text}");
    assert!(text.contains("syscall brk(0x7e8000001000) = 0x7e8000001000"), "{text}");
}

#[test]
fn break_stops_at_the_svc_with_the_registers_and_hook_changes_a_register() {
    let (img, g) = with(&WRITE_EXIT);
    let at = g + 16;
    let (text, v) = run(&img, g, &[], |s| s.breaks = vec![format!("{at:#x}")]);
    assert_eq!(kind(&v), "break", "{text}");
    assert_eq!(v["regs"]["x8"].as_u64(), Some(64), "{text}");
    assert_eq!(v["regs"]["x2"].as_u64(), Some(3), "{text}");
    assert!(text.contains("stack top") && text.contains("x8=0x40"), "{text}");
    assert_eq!(v["stdout"].as_str(), Some(""), "the call did not run: {text}");
    // a hook sets the length to 2, prints, and the run goes on
    let (text, v) = run(&img, g, &[], |s| s.hooks = vec![format!("{at:#x}=set:x2=2"), format!("{at:#x}=print")]);
    assert_eq!(kind(&v), "exit", "{text}");
    assert_eq!(v["stdout"].as_str(), Some("hi"), "{text}");
    assert!(text.contains("hooks     2 hit(s)") && text.contains("pc=") && text.contains("x8=0x40"), "{text}");
    assert!(emulate(&img, &EmuSpec { hooks: vec![format!("{at:#x}=set:rax=1")], ..EmuSpec::new(&format!("{g:#x}")) })
        .is_err());
}

#[test]
fn max_calls_lists_that_many_and_counts_the_rest() {
    let (img, g) = with(&WRITE_EXIT);
    let (text, v) = run(&img, g, &[], |s| s.max_calls = 1);
    assert!(text.contains("1 more call(s) not listed (1 of them system calls)"), "{text}");
    assert_eq!(v["calls_total"].as_u64(), Some(2), "{text}");
    assert_eq!(v["calls_not_listed"].as_u64(), Some(1), "{text}");
}

#[test]
fn the_x86_system_calls_still_go_through_the_shared_code_unchanged() {
    // lea rsi,[rip+0xf9]; mov edi,1; mov edx,3; mov eax,1; syscall; mov edi,7; mov eax,231; syscall; data "hi\n"
    let mut code: Vec<u8> = vec![0x48, 0x8d, 0x35, 0xf9, 0, 0, 0];
    code.extend_from_slice(&[0xbf, 1, 0, 0, 0, 0xba, 3, 0, 0, 0, 0xb8, 1, 0, 0, 0, 0x0f, 0x05]);
    code.extend_from_slice(&[0xbf, 7, 0, 0, 0, 0xb8, 0xe7, 0, 0, 0, 0x0f, 0x05]);
    code.resize(0x100, 0xcc);
    code.extend_from_slice(b"hi\n");
    let img = Image::from_raw(code, PathBuf::from("/fx/sys.bin"), 0x40_1000, None).expect("the raw image opens");
    let (text, v) = run(&img, 0x40_1000, &[], |_| {});
    // the same expectations as tests_sys::write_to_stdout_then_exit_group_gives_the_output_and_the_exit_code
    assert_eq!(kind(&v), "exit", "{text}");
    assert_eq!(v["exit"].as_i64(), Some(7), "{text}");
    assert_eq!(v["stdout"].as_str(), Some("hi\n"), "{text}");
    assert!(text.contains("syscall write(1, ") && text.contains("= 3"), "{text}");
    assert!(text.contains("syscall exit_group(7)"), "{text}");
}
