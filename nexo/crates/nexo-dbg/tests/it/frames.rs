#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Frame selection: stopped three calls deep (`main` -> `outer` -> `middle` -> `inner`), `up`, `down` and `frame N`
//! select a caller, and `regs`, `locals`, `args`, `print`, `mem` and `dis` read that frame. Its registers come from
//! the unwind tables: those the call may have changed (rax, rcx, ...) are said to be not saved, the callee-saved
//! ones and rbp keep their values. Every resume selects frame #0 again. Skipped without the compiler.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
__attribute__((noinline)) int inner(int z) {
    int depth_inner = z * 3;
    volatile int sink = depth_inner + 1;
    return sink;
}
__attribute__((noinline)) int middle(int y) {
    int depth_middle = y + 100;
    int r = inner(depth_middle - 90);
    return r + depth_middle;
}
__attribute__((noinline)) int outer(int x) {
    long depth_outer = x * 1000L;
    int r = middle(x + 1);
    return r + (int)depth_outer;
}
int main(void) {
    int top = 7;
    return outer(top) == 0;
}
"#;

/// The program built by `compiler`, or `None` (and a note) when that compiler is not here.
fn build(compiler: &str) -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("p.c"), SRC).unwrap();
    let ok = std::process::Command::new(compiler)
        .args(["-g", "-O0", "-o", "p", "p.c"])
        .current_dir(dir.path())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no {compiler}: skipped");
        return None;
    }
    let exe = dir.path().join("p");
    Some((dir, exe))
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn refused(s: &mut Session, cmd: &str) -> String {
    match s.exec(&split_words(cmd)) {
        Ok(o) => panic!("{cmd} was answered: {}", o.text_all()),
        Err(e) => e.to_string(),
    }
}

/// Stopped in `inner`, three calls below `main`.
fn stopped_in_inner(exe: std::path::PathBuf) -> (Session, String) {
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, "break inner");
    let stop = run(&mut s, "cont");
    (s, stop)
}

fn walk_the_callers(compiler: &str) {
    let Some((_d, exe)) = build(compiler) else { return };
    let (mut s, stop) = stopped_in_inner(exe);
    assert!(stop.contains(", frame #0)") && stop.contains("inner"), "{compiler}: {stop}");
    let bt = run(&mut s, "bt");
    assert!(bt.contains("#1  ") && bt.contains("middle") && bt.contains("outer") && bt.contains("main"), "{bt}");

    // one up: middle, with its own locals
    let up = run(&mut s, "up");
    assert!(up.starts_with("#1  0x") && up.contains("middle+"), "{compiler}: {up}");
    assert!(up.contains("depth_middle = 108"), "{compiler}: {up}");
    assert!(up.contains("not saved in this frame: rax "), "{compiler}: {up}");
    let locals = run(&mut s, "locals");
    assert!(locals.starts_with("in frame #1  0x") && locals.contains("in middle:"), "{compiler}: {locals}");
    assert!(locals.contains("depth_middle (int) = 108") && locals.contains("arg y (int) = 8"), "{compiler}: {locals}");
    assert!(!locals.contains("depth_inner"), "{compiler}: {locals}");
    let p = run(&mut s, "print depth_middle");
    assert!(p.contains("depth_middle (int at 0x") && p.contains(") = 108"), "{compiler}: {p}");
    let args = run(&mut s, "args");
    assert!(args.contains("y (int) = 8"), "{compiler}: {args}");

    // the registers the call may have changed have no value here; rbp and rsp are the frame's own
    let regs = run(&mut s, "regs");
    assert!(regs.contains("rax    <not saved>") && regs.contains("rbp    0x"), "{compiler}: {regs}");
    assert!(regs.contains("<not saved>: rax rcx rdx rsi rdi r8 r9 r10 r11 eflags"), "{compiler}: {regs}");
    let why = refused(&mut s, "print $rax");
    assert!(why.contains("rax is not saved in this frame"), "{compiler}: {why}");
    let one = run(&mut s, "regs rdi");
    assert!(one.contains("rdi is not saved in frame #1"), "{compiler}: {one}");
    let rbp = run(&mut s, "print $rbp");
    let rsp = run(&mut s, "print $rsp");
    assert!(rbp.contains("$rbp = 0x") && rsp.contains("$rsp = 0x"), "{compiler}: {rbp} {rsp}");
    // the slot of depth_middle is read through the frame's rbp
    let at = p.split("at ").nth(1).and_then(|r| r.split(')').next()).unwrap().to_string();
    let mem = run(&mut s, &format!("mem {at} 4 --as u32"));
    assert!(mem.starts_with("in frame #1") && mem.contains("0x0000006c"), "{compiler}: {mem}");
    // `=>` marks where middle goes on after the call
    let dis = run(&mut s, "dis");
    assert!(dis.lines().nth(1).is_some_and(|l| l.starts_with("=> 0x")), "{compiler}: {dis}");

    // two up: outer; `info frame` says how it was found and who called it
    let f2 = run(&mut s, "frame 2");
    assert!(f2.starts_with("#2  0x") && f2.contains("outer+") && f2.contains("depth_outer = 7000"), "{f2}");
    let info = run(&mut s, "info frame");
    assert!(info.contains("found from frame #1 by .eh_frame"), "{compiler}: {info}");
    assert!(info.contains("called frame #1: middle") && info.contains("called by frame #3: main"), "{info}");
    let p = run(&mut s, "p depth_outer");
    assert!(p.contains(") = 7000") && p.starts_with("in frame #2"), "{compiler}: {p}");
    let back = run(&mut s, "frame function main");
    assert!(back.starts_with("#3  0x") && back.contains("top = 7"), "{compiler}: {back}");
    let down = run(&mut s, "down");
    assert!(down.starts_with("#2  0x") && down.contains("outer+"), "{compiler}: {down}");

    // what cannot be selected or changed here, and the frame that stays
    let past = refused(&mut s, "up 500");
    assert!(past.contains("there is no frame #502: the chain has ") && past.contains("frame #2 stays"), "{past}");
    let below = refused(&mut s, "down 3");
    assert!(below.contains("`down 2` is the furthest down"), "{compiler}: {below}");
    let set = refused(&mut s, "setreg rax 1");
    assert!(set.contains("frame #2 is selected") && set.contains("`frame 0`"), "{compiler}: {set}");
    assert!(run(&mut s, "bt").contains("frame #2 is selected"));

    // a resume selects the stopped frame again
    let step = run(&mut s, "next");
    assert!(step.contains(", frame #0)"), "{compiler}: {step}");
    let locals = run(&mut s, "locals");
    assert!(locals.starts_with("in inner:") && locals.contains("arg z (int) = 18"), "{compiler}: {locals}");
    let regs = run(&mut s, "regs");
    assert!(!regs.contains("not saved"), "{compiler}: {regs}");
}

#[test]
fn up_down_and_frame_read_the_callers_of_a_gcc_program() {
    walk_the_callers("gcc");
}

#[test]
fn up_down_and_frame_read_the_callers_of_a_clang_program() {
    walk_the_callers("clang");
}

#[test]
fn the_gdb_spellings_select_and_describe_frames() {
    let Some((_d, exe)) = build("cc") else { return };
    let (mut s, _) = stopped_in_inner(exe);
    let f = run(&mut s, "f 1");
    assert!(f.starts_with("#1  0x") && f.contains("middle+"), "{f}");
    let info = run(&mut s, "info frame");
    assert!(info.starts_with("frame #1  0x") && info.contains("called by frame #2: outer"), "{info}");
    let regs = run(&mut s, "info registers rbx rax");
    assert!(regs.starts_with("in frame #1") && regs.contains("rbx = 0x"), "{regs}");
    assert!(regs.contains("rax is not saved in frame #1"), "{regs}");
    let x = refused(&mut s, "p/x $rax");
    assert!(x.contains("rax is not saved in this frame"), "{x}");
    let x = run(&mut s, "x/2wx $rsp");
    assert!(x.starts_with("in frame #1"), "{x}");
    run(&mut s, "frame 0");
    let info = run(&mut s, "info frame");
    assert!(info.contains("the registers are thread ") && !info.contains("not saved"), "{info}");
}

#[test]
fn an_address_worked_out_from_a_register_the_caller_lost_says_which_register() {
    let Some((_d, exe)) = build("cc") else { return };
    let (mut s, _) = stopped_in_inner(exe);
    run(&mut s, "up");
    for cmd in [
        "mem $rdi 8",
        "mem rdi 8",
        "mem $rsp..$rax",
        "x/s $rdi",
        "dis $rax",
        "break *$rax",
        "until $rax",
        "watch $rax",
        "info symbol $rax",
    ] {
        let why = refused(&mut s, cmd);
        assert!(why.contains(" is not saved in this frame:"), "{cmd}: {why}");
        assert!(!why.contains("needs a running program") && !why.contains("is not a register"), "{cmd}: {why}");
    }
    // the registers the frame has still name addresses, and frame #1 stays selected
    let mem = run(&mut s, "mem $rbp 8");
    assert!(mem.starts_with("in frame #1  0x") && mem.contains("middle+"), "{mem}");
    assert!(run(&mut s, "info frame").starts_with("frame #1  0x"));
}

#[test]
fn a_condition_set_in_a_callers_frame_is_checked_with_the_stopped_threads_registers() {
    let Some((_d, exe)) = build("cc") else { return };
    let (mut s, _) = stopped_in_inner(exe);
    run(&mut s, "up");
    // a hit works `$rdi` out in its own frame #0, where every register has a value
    let b = run(&mut s, "break middle if $rdi == 8");
    assert!(b.contains("breakpoint #2 at 0x") && b.contains("middle"), "{b}");
    let bad = refused(&mut s, "break outer if $nosuch == 1");
    assert!(bad.contains("the condition \"$nosuch == 1\" does not work"), "{bad}");
    assert!(run(&mut s, "info frame").starts_with("frame #1  0x"));
}
