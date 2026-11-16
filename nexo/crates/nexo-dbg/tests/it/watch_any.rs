#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Watches past the four debug registers: a 64-byte buffer written in a loop (guarded pages, and single steps when
//! asked), six ints at once (four debug registers, then guarded pages), and a conditional watch that only logs, as a
//! write tracer. Skipped without the compiler.

use nexo_dbg::{open, split_words, Kind, Session};

const SOURCE: &str = r#"
#include <stdio.h>
unsigned char buf[64];
int six[6];
int counter;
__attribute__((noinline)) void fill(unsigned char *b, int n) {
    for (int i = 0; i < n; i++) b[i] = (unsigned char)(i * 3 + 1);
}
__attribute__((noinline)) void bump(int *v, int n) {
    for (int i = 0; i < n; i++) v[i] += i + 1;
}
__attribute__((noinline)) void tick(void) { counter++; }
int main(void) {
    fill(buf, 64);
    bump(six, 6);
    for (int i = 0; i < 5; i++) tick();
    printf("%d %d %d\n", buf[63], six[5], counter);
    return 0;
}
"#;

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

/// The program, built (with `extra` flags such as `-m32`) and stopped at `main`.
fn built_at_main(extra: &[&str]) -> Option<(tempfile::TempDir, Session)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("w.c"), SOURCE).unwrap();
    let ok = std::process::Command::new("gcc")
        .args(extra)
        .args(["-g", "-O0", "-o", "w", "w.c"])
        .current_dir(dir.path())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("SKIPPED: gcc {extra:?} cannot build the program here, so nothing was checked");
        return None;
    }
    let (mut s, _) = open(&Kind::Run { exe: dir.path().join("w"), args: vec![] }).expect("start");
    run(&mut s, "break main");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("main"), "{stop}");
    Some((dir, s))
}

fn at_main() -> Option<(tempfile::TempDir, Session)> {
    built_at_main(&[])
}

#[test]
fn a_64_byte_buffer_written_in_a_loop_stops_at_each_write_with_the_writer_and_its_function() {
    let Some((_dir, mut s)) = at_main() else { return };
    let w = run(&mut s, "watch buf");
    assert!(w.contains("watch #2") && w.contains("64 bytes w (page)"), "{w}");
    assert!(w.contains("lose write access") && w.contains("a fault, one step"), "{w}");
    for (i, v) in [(0, 1), (1, 4), (2, 7)] {
        let stop = run(&mut s, "cont");
        assert!(stop.contains("watch #2 page"), "{stop}");
        assert!(stop.contains(&format!("+64 bytes {i}..{}: 00 -> {v:02x} (0 -> {v})", i + 1)), "{stop}");
        assert!(stop.contains("; by ") && stop.contains("fill"), "{stop}");
    }
    // `$at` is the offset of the change, so an `if` on it skips the writes before
    run(&mut s, "delete 2");
    let w = run(&mut s, "watch buf 64 if $at == 63");
    assert!(w.contains("(page)"), "{w}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("63..64: 00 -> be (0 -> 190)") && stop.contains("fill"), "{stop}");
    run(&mut s, "delete all");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    assert!(run(&mut s, "out").contains("190 6 5"));
}

#[test]
fn a_step_watch_compares_the_bytes_after_each_instruction_and_names_the_writer() {
    let Some((_dir, mut s)) = at_main() else { return };
    let w = run(&mut s, "watch buf 64 step");
    assert!(w.contains("64 bytes w (step)") && w.contains("one instruction per stop"), "{w}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("watch #2 step") && stop.contains("0..1: 00 -> 01"), "{stop}");
    assert!(stop.contains("; by ") && stop.contains("fill"), "{stop}");
    // reads change nothing a step can see, so a step watch takes writes only
    let e = s.exec(&split_words("watch counter r step")).map(|o| o.text_all()).unwrap_or_else(|e| e.to_string());
    assert!(e.contains("sees writes"), "{e}");
    run(&mut s, "delete all");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
}

#[test]
fn six_watched_ints_take_the_four_debug_registers_and_then_guarded_pages() {
    let Some((_dir, mut s)) = at_main() else { return };
    for (n, at) in [0, 4, 8, 12].iter().enumerate() {
        let w = run(&mut s, &format!("watch six+{at} 4"));
        assert!(w.contains("(hw)") && w.contains(&format!("{} of 4 debug registers left", 3 - n)), "{w}");
    }
    for at in [16, 20] {
        let w = run(&mut s, &format!("watch six+{at} 4"));
        assert!(w.contains("(page)"), "{w}");
    }
    let list = run(&mut s, "breaks");
    assert_eq!(list.matches("(hw)").count(), 4, "{list}");
    assert_eq!(list.matches("(page)").count(), 2, "{list}");
    // `bump` adds i + 1 to six[i]: each stop shows the one int it wrote, 0 -> i + 1, and the `add` that wrote it
    for i in 0..6u32 {
        let stop = run(&mut s, "cont");
        let how = if i < 4 { "hw" } else { "page" };
        assert!(stop.contains(&format!("watch #{} {how}", i + 2)), "{i}: {stop}");
        assert!(stop.contains(&format!("00000000 -> {:08x} (0 -> {})", i + 1, i + 1)), "{i}: {stop}");
        assert!(stop.contains("; by ") && stop.contains("bump"), "{i}: {stop}");
    }
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
}

#[test]
fn a_conditional_log_watch_traces_the_writes_without_stopping() {
    let Some((_dir, mut s)) = at_main() else { return };
    let w = run(&mut s, "watch counter if $new > 2 log");
    assert!(w.contains("4 bytes w (hw)"), "{w}");
    let w = run(&mut s, "watch buf page if $at >= 62 log b{$at:d}={$new:d}");
    assert!(w.contains("(page)"), "{w}");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    let log = run(&mut s, "log");
    // the counter goes 1 to 5: the three writes past 2, each with its old and new value and the `tick` that wrote it
    for (o, n) in [(2, 3), (3, 4), (4, 5)] {
        assert!(log.lines().any(|l| l.contains("#2 ") && l.contains(&format!("({o} -> {n}); by "))), "{log}");
    }
    assert_eq!(log.matches("tick").count(), 3, "{log}");
    assert!(log.contains("#3 b62=187") && log.contains("#3 b63=190"), "{log}");
}

#[test]
fn a_32_bit_program_gets_its_pages_guarded_through_its_own_system_call_gate() {
    // the debugger changes the page access from inside the program, and a 32-bit program enters the kernel with
    // `int 0x80` and the 32-bit call numbers
    let Some((_dir, mut s)) = built_at_main(&["-m32"]) else { return };
    let w = run(&mut s, "watch buf 64");
    assert!(w.contains("64 bytes w (page)"), "{w}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("watch #2 page") && stop.contains("0..1: 00 -> 01") && stop.contains("fill"), "{stop}");
    run(&mut s, "delete 2");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
}
