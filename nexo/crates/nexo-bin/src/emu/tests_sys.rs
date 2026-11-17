//! System calls, `--break`, `--hook` and `--max-calls` against tiny x86-64 code the test assembles by hand and
//! loads as bare code (one readable, writable, executable section at `BASE`). No C compiler is needed.

use std::path::PathBuf;

use serde_json::Value;

use super::{emulate, EmuSpec};
use crate::Image;

const BASE: u64 = 0x40_1000;
/// Data starts here, after the code.
const DATA: usize = 0x100;
const SYSCALL: [u8; 2] = [0x0f, 0x05];

#[derive(Default)]
struct Asm(Vec<u8>);

impl Asm {
    fn b(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.extend_from_slice(bytes);
        self
    }
    /// `mov r32, imm32` for the registers numbered 0 to 7 (eax, ecx, edx, ebx, esp, ebp, esi, edi).
    fn mov(&mut self, reg: u8, v: u32) -> &mut Self {
        self.0.push(0xb8 + reg);
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// An instruction `op` that ends in a rip-relative 32-bit displacement to `target` (an offset in the buffer).
    fn rip(&mut self, op: &[u8], target: usize) -> &mut Self {
        self.0.extend_from_slice(op);
        let rel = target as i64 - (self.0.len() as i64 + 4);
        self.0.extend_from_slice(&(rel as i32).to_le_bytes());
        self
    }
    fn sys(&mut self, n: u32) -> &mut Self {
        self.mov(0, n).b(&SYSCALL)
    }
    fn exit(&mut self, code: u32) -> &mut Self {
        self.mov(7, code).sys(231)
    }
    fn image(&self, data: &[u8]) -> Image {
        let mut v = self.0.clone();
        assert!(v.len() <= DATA, "the code is {} bytes", v.len());
        v.resize(DATA, 0xcc);
        v.extend_from_slice(data);
        Image::from_raw(v, PathBuf::from("/fx/sys.bin"), BASE, None).expect("the raw image opens")
    }
}

fn run(img: &Image, edit: impl FnOnce(&mut EmuSpec)) -> (String, Value) {
    let mut spec = EmuSpec::new(&format!("{BASE:#x}"));
    edit(&mut spec);
    let out = emulate(img, &spec).expect("emu answers");
    let page = out.page(None, 400);
    (out.text_all(), page.value)
}

fn kind(v: &Value) -> &str {
    v["stop"]["kind"].as_str().unwrap_or("?")
}

#[test]
fn write_to_stdout_then_exit_group_gives_the_output_and_the_exit_code() {
    let mut a = Asm::default();
    a.rip(&[0x48, 0x8d, 0x35], DATA).mov(7, 1).mov(2, 3).sys(1).exit(7);
    let (text, v) = run(&a.image(b"hi\n"), |_| {});
    assert_eq!(kind(&v), "exit", "{text}");
    assert_eq!(v["exit"].as_i64(), Some(7), "{text}");
    assert_eq!(v["stdout"].as_str(), Some("hi\n"), "{text}");
    assert!(text.contains("syscall write(1, ") && text.contains("= 3"), "the calls are logged: {text}");
    assert!(text.contains("syscall exit_group(7)"), "{text}");
}

#[test]
fn read_takes_the_stdin_buffer_and_gives_nothing_without_one() {
    let mut a = Asm::default();
    // rax = read(0, buf, 5); ebx = eax; write(1, buf, ebx); exit_group(ebx)
    a.mov(7, 0).rip(&[0x48, 0x8d, 0x35], DATA).mov(2, 5).sys(0).b(&[0x89, 0xc3]);
    a.mov(7, 1).rip(&[0x48, 0x8d, 0x35], DATA).b(&[0x89, 0xda]).sys(1);
    a.b(&[0x89, 0xdf]).sys(231);
    let img = a.image(&[0; 8]);
    let (text, v) = run(&img, |s| s.stdin = Some("abc".into()));
    assert_eq!(v["exit"].as_i64(), Some(3), "{text}");
    assert_eq!(v["stdout"].as_str(), Some("abc"), "{text}");
    let (text, v) = run(&img, |_| {});
    assert_eq!(v["exit"].as_i64(), Some(0), "{text}");
    assert_eq!(v["stdout"].as_str(), Some(""), "{text}");
    let (text, v) = run(&img, |s| s.stdin = Some("hex:41 42 43 44 45 46".into()));
    assert_eq!((v["exit"].as_i64(), v["stdout"].as_str()), (Some(5), Some("ABCDE")), "{text}");
}

/// `mmap(0, 0x2000, 3, flags, fd, 0)`, then the answer is kept in rbx.
fn mmap_code(a: &mut Asm, flags: u32, fd: u32) {
    a.b(&[0x31, 0xff]).mov(6, 0x2000).mov(2, 3);
    a.b(&[0x41, 0xba]).b(&flags.to_le_bytes());
    a.b(&[0x49, 0xc7, 0xc0]).b(&fd.to_le_bytes()); // mov r8, imm32 (sign-extended)
    a.b(&[0x45, 0x31, 0xc9]).sys(9).b(&[0x48, 0x89, 0xc3]);
}

#[test]
fn mmap_gives_memory_the_program_can_store_to_and_load_from() {
    let mut a = Asm::default();
    mmap_code(&mut a, 0x22, u32::MAX);
    a.b(&[0xc7, 0x80, 0x00, 0x18, 0x00, 0x00, 0x78, 0x56, 0x34, 0x12]); // mov dword [rax+0x1800], 0x12345678
    a.b(&[0x8b, 0x8b, 0x00, 0x18, 0x00, 0x00]); // mov ecx, [rbx+0x1800]
    a.b(&[0x89, 0xcf]).sys(231); // exit_group(ecx)
    let (text, v) = run(&a.image(&[]), |_| {});
    assert_eq!(v["exit"].as_i64(), Some(0x1234_5678), "{text}");
    assert!(text.contains("syscall mmap(0, 0x2000, 3, 0x22, -1, 0) = 0x7f0000000000"), "{text}");
}

#[test]
fn a_file_backed_mmap_answers_enodev() {
    let mut a = Asm::default();
    mmap_code(&mut a, 0x2, 3);
    a.b(&[0x89, 0xc7]).sys(231); // exit_group(eax)
    let (text, v) = run(&a.image(&[]), |_| {});
    assert_eq!(v["exit"].as_i64(), Some(-19), "{text}");
    assert!(text.contains("-19 (ENODEV)"), "{text}");
}

#[test]
fn getrandom_repeats_for_one_seed_and_differs_for_another() {
    let mut a = Asm::default();
    a.rip(&[0x48, 0x8d, 0x3d], DATA).mov(6, 8).b(&[0x31, 0xd2]).sys(318);
    a.rip(&[0x8b, 0x3d], DATA).sys(231); // exit_group(the first 4 bytes)
    let img = a.image(&[0; 8]);
    let code = |seed: u64| run(&img, |s| s.seed = seed).1["exit"].as_i64();
    assert!(code(0).is_some());
    assert_eq!(code(0), code(0));
    assert_ne!(code(0), code(1));
    assert_eq!(code(7), code(7));
}

#[test]
fn time_answers_the_clock_that_was_set() {
    let mut a = Asm::default();
    a.b(&[0x31, 0xff]).sys(201).b(&[0x89, 0xc7]).sys(231);
    let (text, v) = run(&a.image(&[]), |s| s.clock = 77);
    assert_eq!(v["exit"].as_i64(), Some(77), "{text}");
}

#[test]
fn a_system_call_emu_does_not_do_stops_the_run_with_its_number_and_name() {
    let mut a = Asm::default();
    a.sys(59);
    let (text, v) = run(&a.image(&[]), |_| {});
    assert_eq!(kind(&v), "syscall", "{text}");
    assert!(text.contains("system call 59 (execve)"), "{text}");
}

/// `mov eax, 5; mov ebx, 7; add eax, ebx; ret`: the add is at BASE + 10.
fn add_code() -> Asm {
    let mut a = Asm::default();
    a.mov(0, 5).mov(3, 7).b(&[0x01, 0xd8, 0xc3]);
    a
}

#[test]
fn break_stops_at_the_address_before_it_runs_and_reports_registers_flags_and_stack() {
    let img = add_code().image(&[]);
    let at = BASE + 10;
    let (text, v) = run(&img, |s| s.breaks = vec![format!("{at:#x}")]);
    assert_eq!(kind(&v), "break", "{text}");
    assert_eq!(v["stop"]["rip"].as_u64(), Some(at), "{text}");
    assert_eq!((v["regs"]["rax"].as_u64(), v["regs"]["rbx"].as_u64()), (Some(5), Some(7)), "{text}");
    assert!(text.contains("rax=0x5") && text.contains("flags ") && text.contains("stack top"), "{text}");
}

#[test]
fn hook_print_and_dump_log_and_the_run_goes_on() {
    let img = add_code().image(&[]);
    let at = BASE + 10;
    let (text, v) = run(&img, |s| s.hooks = vec![format!("{at:#x}=print"), format!("{at:#x}=dump:rsp:16")]);
    assert_eq!(kind(&v), "returned", "{text}");
    assert_eq!(v["return"].as_u64(), Some(12), "{text}");
    assert!(text.contains("hooks     2 hit(s)") && text.contains("rax=0x5"), "{text}");
    assert!(text.contains("ad de 00 7c"), "the dump shows the return address at rsp: {text}");
}

#[test]
fn hook_set_changes_a_register_that_later_decides_a_branch() {
    let mut a = Asm::default();
    a.mov(3, 0).b(&[0x85, 0xdb, 0x74, 0x06]); // mov ebx, 0; test ebx, ebx; jz +6
    a.mov(0, 1).b(&[0xc3]).mov(0, 2).b(&[0xc3]);
    let img = a.image(&[]);
    let (text, v) = run(&img, |_| {});
    assert_eq!(v["return"].as_u64(), Some(2), "{text}");
    let at = BASE + 5;
    let (text, v) = run(&img, |s| s.hooks = vec![format!("{at:#x}=set:rbx=1")]);
    assert_eq!(v["return"].as_u64(), Some(1), "{text}");
    assert!(text.contains("set    rbx = 0x1 (was 0x0)"), "{text}");
}

#[test]
fn max_calls_lists_that_many_and_says_how_many_were_left_out() {
    let mut a = Asm::default();
    for _ in 0..5 {
        a.sys(39);
    }
    a.b(&[0x31, 0xff]).sys(231);
    let img = a.image(&[]);
    let (text, v) = run(&img, |_| {});
    assert!(text.contains("syscall getpid() = 0x1092") && !text.contains("not listed"), "{text}");
    assert_eq!(v["calls_total"].as_u64(), Some(6), "{text}");
    let (text, v) = run(&img, |s| s.max_calls = 2);
    assert!(text.contains("4 more call(s) not listed (4 of them system calls)"), "{text}");
    assert!(text.contains("--max-calls 6"), "{text}");
    assert_eq!(v["calls_not_listed"].as_u64(), Some(4), "{text}");
}

#[test]
fn bad_break_hook_and_max_calls_are_refused_with_the_form() {
    let img = add_code().image(&[]);
    for bad in ["0x401000", "0x401000=jump", "0x401000=dump:rax:0", "0x401000=dump:rax:5000", "0x401000=set:rsp=1"] {
        let e = emulate(&img, &EmuSpec { hooks: vec![bad.into()], ..EmuSpec::new("0x401000") }).unwrap_err();
        assert!(e.to_string().contains("--hook"), "{bad}: {e}");
    }
    let e = emulate(&img, &EmuSpec { max_calls: 1_000_000, ..EmuSpec::new("0x401000") }).unwrap_err();
    assert!(e.to_string().contains("--max-calls"), "{e}");
}
