//! Assembly jobs: fix an off-by-one in a nasm routine against a C harness (`asm-fix`), and read a stripped
//! object with `objdump -d` to say what its function computes (`asm-read`).

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::{have, keyed, line, linux_only};
use crate::check::{run, Outcome};
use crate::rng::Rng;
use crate::tasks::{files, Task};

pub fn tasks() -> Vec<Task> {
    vec![fix_task(), read_task()]
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// ---------------------------------------------------------------- asm-fix

const SUMSQ_ASM: &str = "; long sumsq(const int *a, long n): the sum of the squares of a[0..n)
; System V x86-64: rdi = a, rsi = n, result in rax
global sumsq
section .text
sumsq:
    xor     eax, eax
    xor     ecx, ecx
.next:
    cmp     rcx, rsi
    jg      .done
    movsxd  rdx, dword [rdi + rcx*4]
    imul    rdx, rdx
    add     rax, rdx
    inc     rcx
    jmp     .next
.done:
    ret
";

const HARNESS_C: &str = r#"#include <stdio.h>
long sumsq(const int *a, long n);
static int data[16] = {3, -4, 5, 12, -1, 1000, 7, 7, 0, 0, 0, 0, 0, 0, 0, 0};
int main(void) {
  struct { long n, want; } t[] = {{0, 0}, {1, 9}, {3, 50}, {5, 195}};
  int bad = 0;
  for (int i = 0; i < 4; i++) {
    long got = sumsq(data, t[i].n);
    if (got != t[i].want) { printf("FAIL n=%ld: expected %ld, got %ld\n", t[i].n, t[i].want, got); bad++; }
    else printf("PASS n=%ld\n", t[i].n);
  }
  puts(bad ? "SOME FAIL" : "ALL PASS");
  return bad != 0;
}
"#;

const BUILD_SH: &str = "#!/bin/sh
# builds and runs the harness
set -e
nasm -f elf64 sumsq.asm -o sumsq.o
cc -O0 harness.c sumsq.o -o harness
./harness
";

/// The checker's harness: other arrays, a different sentinel after each, negative values.
fn hidden_harness() -> String {
    let mut r = Rng::new(0xa51);
    let mut cases = String::new();
    let mut tests = String::new();
    for (i, n) in [0usize, 1, 2, 9, 40, 100].into_iter().enumerate() {
        let vals: Vec<i64> = (0..n).map(|_| r.int(-900, 900)).collect();
        let want: i64 = vals.iter().map(|v| v * v).sum();
        let list: Vec<String> =
            vals.iter().map(|v| v.to_string()).chain(["31337".to_string(), "31337".to_string()]).collect();
        cases += &format!("static int d{i}[] = {{{}}};\n", list.join(","));
        tests += &format!("  if (sumsq(d{i}, {n}) != {want}L) bad++;\n");
    }
    format!("#include <stdio.h>\nlong sumsq(const int *a, long n);\n{cases}int main(void) {{\n  int bad = 0;\n{tests}  puts(bad ? \"SOME FAIL\" : \"ALL PASS\");\n  return bad != 0;\n}}\n")
}

fn fix_task() -> Task {
    let rules = leak(
        "ASM-FIX - rules of the task

GOAL
  sumsq.asm is a nasm x86-64 routine, long sumsq(const int *a, long n), that returns the sum of the
  squares of a[0..n). The C harness says it is wrong. Fix the assembly.

WHAT YOU HAVE
  sumsq.asm    the routine
  harness.c    a C program that calls it (do not change it)
  build.sh     assembles, links and runs:  sh build.sh
  The system's tools (nasm, cc, objdump, gdb...) can be used. There is no network.

THE FINAL ANSWER
  sumsq.asm, fixed. It is checked with other arrays than harness.c's, including empty and negative
  ones. When you are sure, stop and reply in a few lines.
"
        .into(),
    );
    Task {
        id: "asm-fix",
        trap: "an off-by-one in a loop condition of hand-written assembly: the symptom is a sum that is too big by \
               one square, and the cause is a single jump",
        group: "heavy2",
        files: files(&[
            ("RULES.md", rules),
            ("sumsq.asm", SUMSQ_ASM),
            ("harness.c", HARNESS_C),
            ("build.sh", BUILD_SH),
        ]),
        turns: vec![(1, "sh build.sh fails: sumsq returns too much. Read RULES.md first and fix sumsq.asm.".into())],
        check: check_fix,
        solution: solution_fix,
        setup: None,
        sealed: false,
        mask: &[],
        ready: || linux_only("the assembly tasks").and_then(|()| have(&["nasm", "cc"])),
    }
}

fn check_fix(wd: &Path) -> Outcome {
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return Outcome::fail(e.to_string()),
    };
    let t = tmp.path();
    if std::fs::copy(wd.join("sumsq.asm"), t.join("sumsq.asm")).is_err() {
        return Outcome::fail("sumsq.asm is missing");
    }
    if std::fs::write(t.join("h.c"), hidden_harness()).is_err() {
        return Outcome::fail("cannot write the harness");
    }
    let t_s = |name: &str| t.join(name).to_string_lossy().into_owned();
    let asm = run(
        Command::new("nasm").args(["-f", "elf64", &t_s("sumsq.asm"), "-o", &t_s("s.o")]),
        None,
        Duration::from_secs(30),
    );
    if !asm.success() {
        return Outcome::fail(format!("sumsq.asm does not assemble: {}", asm.tail(300)));
    }
    let cc =
        run(Command::new("cc").args(["-O0", &t_s("h.c"), &t_s("s.o"), "-o", &t_s("h")]), None, Duration::from_secs(60));
    if !cc.success() {
        return Outcome::fail(format!("cannot link the harness: {}", cc.tail(300)));
    }
    let got = run(&mut Command::new(t.join("h")), None, Duration::from_secs(20));
    Outcome::all(vec![(got.stdout.contains("ALL PASS"), format!("the hidden harness printed {:?}", got.stdout.trim()))])
}

fn solution_fix(wd: &Path) {
    let _ = std::fs::write(wd.join("sumsq.asm"), SUMSQ_ASM.replace("    jg      .done", "    jge     .done"));
}

// ---------------------------------------------------------------- asm-read

const MYSTERY_ASM: &str = "global f
section .text
f:
    xor     eax, eax
.top:
    cmp     rdi, 1
    je      .out
    test    dil, 1
    jz      .even
    lea     rdi, [rdi + rdi*2 + 1]
    jmp     .step
.even:
    shr     rdi, 1
.step:
    inc     rax
    jmp     .top
.out:
    ret
";

const INPUTS: [u64; 3] = [27, 97, 871];

fn steps(mut n: u64) -> u64 {
    let mut k = 0;
    while n != 1 {
        n = if n.is_multiple_of(2) { n / 2 } else { 3 * n + 1 };
        k += 1;
    }
    k
}

fn setup_read(wd: &Path) -> Result<(), String> {
    have(&["nasm", "strip"])?;
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (src, obj) = (tmp.path().join("m.asm"), tmp.path().join("m.o"));
    std::fs::write(&src, MYSTERY_ASM).map_err(|e| e.to_string())?;
    let asm =
        run(Command::new("nasm").args(["-f", "elf64"]).arg(&src).arg("-o").arg(&obj), None, Duration::from_secs(30));
    if !asm.success() {
        return Err(format!("nasm failed: {}", asm.tail(300)));
    }
    let strip = run(
        Command::new("strip").arg("--strip-all").arg("-o").arg(wd.join("mystery.o")).arg(&obj),
        None,
        Duration::from_secs(30),
    );
    if strip.success() {
        Ok(())
    } else {
        Err(format!("strip failed: {}", strip.tail(300)))
    }
}

fn read_task() -> Task {
    let rules = leak(
        "ASM-READ - rules of the task

GOAL
  mystery.o is a stripped x86-64 object with one function in .text. It takes an unsigned number in
  rdi (System V convention) and returns an unsigned number in rax. Say what it returns for three
  inputs.

WHAT YOU HAVE
  mystery.o    the object, without symbols; its source is not available
  The system's tools (objdump, readelf, gdb, nasm, cc...) can be used. There is no network.

THE FINAL ANSWER
  answer.txt, with exactly these lines:
      f(27): <the value returned for rdi = 27>
      f(97): <the value returned for rdi = 97>
      f(871): <the value returned for rdi = 871>
  When you are sure, stop and reply in a few lines.
"
        .into(),
    );
    Task {
        id: "asm-read",
        trap: "a loop in a stripped object with no names: the function is found by reading the instructions, not \
               by running them on guessed inputs",
        group: "heavy2",
        files: files(&[("RULES.md", rules)]),
        turns: vec![(
            1,
            "What does the function in mystery.o return for 27, 97 and 871? Read RULES.md first; write the three \
             values in answer.txt."
                .into(),
        )],
        check: check_read,
        solution: solution_read,
        setup: Some(setup_read),
        sealed: false,
        mask: &[],
        ready: || linux_only("the assembly tasks").and_then(|()| have(&["nasm", "strip", "objdump"])),
    }
}

fn check_read(wd: &Path) -> Outcome {
    let a = keyed(wd, "answer.txt");
    Outcome::share(INPUTS.iter().map(|n| line(&a, &format!("f({n})"), &steps(*n).to_string())).collect())
}

fn solution_read(wd: &Path) {
    let text: String = INPUTS.iter().map(|n| format!("f({n}): {}\n", steps(*n))).collect();
    let _ = std::fs::write(wd.join("answer.txt"), text);
}
