//! Indirect calls, jump tables and forward flow, on small C programs compiled in the test.
use crate::tests_map::{compile, whole};
use crate::{decomp_window, Image};

const PTRS: &str = r#"
#include <stdio.h>
__attribute__((noinline)) int add1(int x) { return x + 1; }
__attribute__((noinline)) int dbl(int x) { return x * 2; }
__attribute__((noinline)) int neg(int x) { return -x; }
int (*table[3])(int) = { add1, dbl, neg };
__attribute__((noinline)) int run(int i, int v) { return table[i](v); }
__attribute__((noinline)) int via(int (*f)(int), int v) { return f(v) + 1; }
__attribute__((noinline)) int sw(int c, int v) {
    switch (c) {
    case 0: return v + 10; case 1: return v * 3; case 2: return v - 4;
    case 3: return v ^ 7; case 4: return v << 2; case 5: return v | 9;
    default: return 0;
    }
}
int main(int argc, char **argv) { (void)argv; return run(argc, 5) + via(dbl, argc) + sw(argc, 1); }
"#;

fn dec(img: &Image, dir: &std::path::Path, f: &str) -> String {
    whole(&decomp_window(img, dir, &[f.to_string()], false, None, 500).unwrap())
}

const FLOW: &str = r#"
#include <stdio.h>
volatile int g_sink;
__attribute__((noinline)) int source(int x) { return x * 7 + 1; }
__attribute__((noinline)) int sink(int v) { return v + g_sink; }
__attribute__((noinline)) int use(int a) {
    int r = source(a);
    if (r > 100) {
        g_sink = r;
        return sink(r);
    }
    printf("%d\n", r);
    return 0;
}
int main(int argc, char **argv) { (void)argv; return use(argc); }
"#;

#[test]
fn forward_slice_follows_a_call_result_into_compares_stores_and_calls_on_both_branches() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = compile(dir.path(), FLOW, &["-O0", "-fno-pie", "-no-pie"]) else { return };
    let img = Image::open(&exe).unwrap();
    let f = img.symbols.iter().find(|s| s.name == "use").unwrap().addr;
    let func = img.func_at(f).unwrap();
    let call = img
        .refs()
        .from(func.start, func.end)
        .find(|r| {
            r.kind == crate::Kind::Call
                && crate::graph::node_name(&img, crate::graph::node_of(&img, r.target)).starts_with("source")
        })
        .unwrap()
        .src;
    let t = whole(&crate::slice_forward(&img, &format!("{call:#x}"), None, None, None, 200).unwrap());
    println!("{t}");
    assert!(t.contains("forward slice of the result (rax) of the call to source"), "{t}");
    assert!(t.contains("is compared"), "the compare with 100 is found\n{t}");
    assert!(t.contains("g_sink") && t.contains("stored to"), "the store to the global is found\n{t}");
    assert!(t.contains("argument 1 of the call to sink"), "the call on the taken side\n{t}");
    assert!(t.contains("argument 2 of the call to printf"), "the call on the other side\n{t}");
    assert!(t.contains("[inferred]") && t.contains("[proven]"), "{t}");
    // a row cap names the rest
    let small = crate::slice_forward(&img, &format!("{call:#x}"), None, None, None, 3).unwrap();
    assert!(small.tail > 0);
    // an instruction that writes nothing to follow is refused with the next step
    assert!(crate::slice_forward(&img, &format!("{:#x}", func.end - 1), None, None, None, 20).is_err());
}

#[test]
fn indirect_calls_and_tail_jumps_through_pointers_are_never_dropped() {
    let dir = tempfile::tempdir().unwrap();
    for flags in [&["-O0", "-fno-pie", "-no-pie"][..], &["-O2", "-fno-pie", "-no-pie"][..]] {
        let Some(exe) = compile(dir.path(), PTRS, flags) else { return };
        let img = Image::open(&exe).unwrap();
        let run = dec(&img, dir.path(), "run");
        assert!(run.contains("(*") && run.contains("&table") && run.contains("a2)"), "{flags:?}\n{run}");
        assert!(!run.contains("jump not resolved"), "{flags:?}\n{run}");
        let via = dec(&img, dir.path(), "via");
        assert!(via.contains("(*a1)(a2)"), "{flags:?}\n{via}");
        // the switch comes back as cases, with the bounds check as the default
        let sw = dec(&img, dir.path(), "sw");
        if flags[0] == "-O0" {
            continue; // an unoptimised build compares one case at a time: there is no table
        }
        assert!(sw.contains("switch") && sw.contains("case 5:"), "{flags:?}\n{sw}");
    }
}
