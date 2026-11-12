//! Indirect calls, jump tables and forward flow, on small C programs compiled in the test.
use crate::tests_map::{compile, whole};
use crate::{decomp_window, Image};
use std::path::{Path, PathBuf};

/// Compile a sample, or say loudly that the test is skipped (it proves nothing then).
fn sample(dir: &Path, src: &str, flags: &[&str], test: &str) -> Option<PathBuf> {
    let exe = compile(dir, src, flags);
    if exe.is_none() {
        eprintln!(
            "SKIPPED {test} with {flags:?}: no C compiler (cc, gcc or clang) builds the sample, so nothing was checked"
        );
    }
    exe
}

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
    let Some(exe) = sample(dir.path(), FLOW, &["-O0", "-fno-pie", "-no-pie"], "forward_slice_follows_a_call_result")
    else {
        return;
    };
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
    // a row cap shows exactly the budget (10 is the smallest page), counts the rest and names how to get it
    let all = crate::slice_forward(&img, &format!("{call:#x}"), None, None, None, 500).unwrap();
    assert_eq!(all.tail, 0, "the whole answer fits in 500 rows");
    let total = all.rows.len();
    assert!(total > 12, "the sample must have more than a page of rows: {total}");
    let small = crate::slice_forward(&img, &format!("{call:#x}"), None, None, None, 10).unwrap();
    assert_eq!(small.rows.len(), 10);
    assert_eq!(small.tail, total - 10);
    let page = small.page(None, 10).text();
    assert!(page.contains(&format!("... {} more lines: ", total - 9)), "the cut line counts the rest\n{page}");
    // an instruction that writes nothing to follow (the final ret) is refused with the next step
    let ret = {
        let mut last = 0;
        crate::disasm::walk(img.read_at(func.start, (func.end - func.start) as usize).unwrap(), func.start, |i, _| {
            if i.mnemonic() == iced_x86::Mnemonic::Ret {
                last = i.ip();
            }
        });
        last
    };
    assert!(ret != 0 && ret < func.end, "the function has a ret");
    let err = crate::slice_forward(&img, &format!("{ret:#x}"), None, None, None, 20).expect_err("a ret writes nothing");
    assert_eq!(
        err,
        crate::Error::Bad("that instruction writes no register or stack slot to follow; give --reg or --mem".into())
    );
    // an address inside an instruction is refused too, naming the function
    let inside =
        crate::slice_forward(&img, &format!("{:#x}", call + 1), None, None, None, 20).expect_err("mid-instruction");
    assert!(inside.to_string().contains("is inside an instruction of use, not at its start"), "{inside}");
}

#[test]
fn indirect_calls_and_tail_jumps_through_pointers_are_never_dropped() {
    let dir = tempfile::tempdir().unwrap();
    for flags in [&["-O0", "-fno-pie", "-no-pie"][..], &["-O2", "-fno-pie", "-no-pie"][..]] {
        let Some(exe) = sample(dir.path(), PTRS, flags, "indirect_calls_and_tail_jumps") else { return };
        let img = Image::open(&exe).unwrap();
        let run = dec(&img, dir.path(), "run");
        assert!(run.contains("(*") && run.contains("&table") && run.contains("a2)"), "{flags:?}\n{run}");
        assert!(!run.contains("jump not resolved"), "{flags:?}\n{run}");
        let via = dec(&img, dir.path(), "via");
        assert!(via.contains("(*a1)(a2)"), "{flags:?}\n{via}");
        // the switch comes back as cases, with the bounds check as the default
        let sw = dec(&img, dir.path(), "sw");
        if flags[0] == "-O0" {
            // an unoptimised build compares one case at a time (no table): every case value is still there
            // and no jump is left unresolved or turned into a call
            assert!(!sw.contains("jump not resolved") && !sw.contains("return (*"), "{flags:?}\n{sw}");
            assert!(sw.contains("a1 == 5") && sw.contains("a1 == 0") && sw.contains("a2 + 0xa"), "{flags:?}\n{sw}");
            continue;
        }
        assert!(sw.contains("switch") && sw.contains("case 5:"), "{flags:?}\n{sw}");
    }
}

#[test]
fn a_call_through_a_parameter_pointer_uses_its_own_arguments_at_every_optimisation_level() {
    let dir = tempfile::tempdir().unwrap();
    for flags in [&["-O0", "-fno-pie", "-no-pie"][..], &["-O2", "-fno-pie", "-no-pie"][..]] {
        let Some(exe) = sample(dir.path(), PTRS, flags, "a_call_through_a_parameter_pointer") else { return };
        let img = Image::open(&exe).unwrap();
        let via = dec(&img, dir.path(), "via");
        assert!(via.contains("(*a1)(a2)"), "{flags:?}: the pointer is a1 and the one argument is a2\n{via}");
        assert!(!via.contains("(*a1)(a2, "), "{flags:?}: no stale argument is invented\n{via}");
    }
}

#[test]
fn a_non_pie_switch_is_never_rendered_as_a_call_through_a_pointer() {
    let dir = tempfile::tempdir().unwrap();
    // plain -no-pie -O2 uses jmp [table+idx*8]; the large code model loads the table base into a register first
    for flags in [&["-O2", "-fno-pie", "-no-pie"][..], &["-O2", "-fno-pie", "-no-pie", "-mcmodel=large"][..]] {
        let Some(exe) = sample(dir.path(), PTRS, flags, "a_non_pie_switch") else { return };
        let img = Image::open(&exe).unwrap();
        let sw = dec(&img, dir.path(), "sw");
        println!("{flags:?}\n{sw}");
        assert!(!sw.contains("return (*") && !sw.contains("(*"), "{flags:?}: not a tail call\n{sw}");
        assert!(sw.contains("switch") || sw.contains("jump not resolved"), "{flags:?}\n{sw}");
    }
}

/// Hand-written assembly, so each instruction is exactly the one the test talks about.
const ASM: &str = r#"
__asm__(
    ".intel_syntax noprefix\n"
    ".text\n"
    ".globl deref\n.type deref,@function\n"
    "deref:\n"
    "  mov rax, rdi\n"           /* the value followed: rax */
    "  mov rdx, [rax]\n"         /* a load through rax: rdx holds the pointee, not the value */
    "  mov rcx, rdx\n"
    "  mov qword ptr [rax], 5\n" /* a store through rax: rax is only the address */
    "  mov rsi, rax\n"           /* a real copy */
    "  ret\n"
    ".size deref,.-deref\n"
    ".globl partial\n.type partial,@function\n"
    "partial:\n"
    "  mov rax, rdi\n"
    "  mov al, 1\n"              /* only the low byte is overwritten */
    "  mov rsi, rax\n"           /* the upper bytes are still the value */
    "  mov ax, 2\n"
    "  mov rdx, rax\n"
    "  mov eax, 3\n"            /* a 32-bit write clears the upper half: the value is gone */
    "  mov rcx, rax\n"
    "  ret\n"
    ".size partial,.-partial\n"
    ".att_syntax prefix\n"
);
int main(void) { return 0; }
"#;

fn rows_of(img: &Image, at: &str) -> String {
    whole(&crate::slice_forward(img, at, None, None, None, 200).unwrap())
}

#[test]
fn a_register_used_only_as_an_address_is_dereferenced_not_copied() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = sample(dir.path(), ASM, &["-O0", "-fno-pie", "-no-pie"], "a_register_used_only_as_an_address")
    else {
        return;
    };
    let img = Image::open(&exe).unwrap();
    let t = rows_of(&img, "deref");
    println!("{t}");
    assert!(t.contains("the value at [rax] is loaded into rdx"), "a load through rax is a dereference\n{t}");
    assert!(!t.contains("rax is copied to rdx"), "{t}");
    assert!(
        !t.contains("rdx is copied to rcx") && !t.contains("rdx feeds"),
        "rdx holds the pointee, it is not followed\n{t}"
    );
    assert!(
        !t.contains("rax is stored through a pointer"),
        "rax is the address of the store, not the stored value\n{t}"
    );
    assert!(t.contains("rax is the address of a store"), "{t}");
    assert!(t.contains("rax is copied to rsi"), "a real copy is still a copy\n{t}");
}

#[test]
fn a_partial_register_write_keeps_the_upper_bytes_following() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = sample(dir.path(), ASM, &["-O0", "-fno-pie", "-no-pie"], "a_partial_register_write") else {
        return;
    };
    let img = Image::open(&exe).unwrap();
    let t = rows_of(&img, "partial");
    println!("{t}");
    assert!(t.contains("the low byte (al) of rax is overwritten"), "{t}");
    assert!(t.contains("rax is copied to rsi"), "after mov al,1 the upper bytes are still the value\n{t}");
    assert!(t.contains("the low 2 bytes (ax) of rax are overwritten"), "{t}");
    assert!(t.contains("rax is copied to rdx"), "{t}");
    assert!(t.contains("rax is overwritten"), "a 32-bit write really ends the value\n{t}");
    assert!(!t.contains("rax is copied to rcx"), "{t}");
}

/// The address of the first indirect call in a function.
fn indirect_call_in(img: &Image, name: &str) -> u64 {
    let f = img.func_at(img.symbols.iter().find(|s| s.name == name).unwrap().addr).unwrap();
    let mut site = 0;
    crate::disasm::walk(img.read_at(f.start, (f.end - f.start) as usize).unwrap(), f.start, |i, _| {
        if site == 0 && i.flow_control() == iced_x86::FlowControl::IndirectCall {
            site = i.ip();
        }
    });
    assert!(site != 0, "{name} has an indirect call");
    site
}

#[test]
fn targets_loaded_from_the_names_file_are_capped_cleaned_and_say_when_more_were_seen() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = sample(dir.path(), PTRS, &["-O0", "-fno-pie", "-no-pie"], "targets_loaded_from_the_names_file")
    else {
        return;
    };
    let mut img = Image::open(&exe).unwrap();
    let site = indirect_call_in(&img, "via");
    let data = tempfile::tempdir().unwrap();
    // 20 targets written by hand: the first one tries to close the comment the decomp puts them in
    let mut list: Vec<String> = vec!["0evil */ int pwned; /*".to_string()];
    list.extend((1..20).map(|k| format!("t{k:02}")));
    let file = serde_json::json!({"version": 1, "sha256": img.sha256, "targets": {format!("{site:#x}"): list}});
    std::fs::create_dir_all(crate::names_path(data.path(), &img.sha256).parent().unwrap()).unwrap();
    std::fs::write(crate::names_path(data.path(), &img.sha256), file.to_string()).unwrap();
    assert!(crate::attach_names(&mut img, data.path()).is_none());
    let seen = crate::call_targets(&img, site);
    assert_eq!(seen.len(), crate::names::MAX_TARGETS, "{seen:?}");
    assert!(seen.iter().all(|t| !t.contains("*/") && !t.contains("/*")), "{seen:?}");
    assert!(crate::call_targets_more(&img, site));
    let text = whole(&decomp_window(&img, data.path(), &["via".to_string()], false, None, 200).unwrap());
    let line = text.lines().find(|l| l.contains("seen:")).unwrap_or_else(|| panic!("no seen note\n{text}"));
    assert!(line.contains("... (16 kept, more seen)"), "{line}");
    assert_eq!(line.matches("*/").count(), 1, "only the comment's own end: {line}");
    assert_eq!(line.matches("/*").count(), 1, "{line}");
}

#[test]
fn a_site_that_got_more_targets_than_it_keeps_stays_marked_as_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = sample(dir.path(), PTRS, &["-O0", "-fno-pie", "-no-pie"], "a_site_that_got_more_targets") else {
        return;
    };
    let mut img = Image::open(&exe).unwrap();
    let site = indirect_call_in(&img, "via");
    let data = tempfile::tempdir().unwrap();
    for k in 0..16 {
        assert!(crate::add_call_target(&img, data.path(), site, &format!("t{k:02}")).unwrap());
    }
    // a 17th is not kept, and the file remembers that something was left out
    assert!(!crate::add_call_target(&img, data.path(), site, "t16").unwrap());
    crate::attach_names(&mut img, data.path());
    assert_eq!(crate::call_targets(&img, site).len(), 16);
    assert!(crate::call_targets_more(&img, site));
    let text = whole(&decomp_window(&img, data.path(), &["via".to_string()], false, None, 200).unwrap());
    assert!(text.contains("(16 kept, more seen)"), "{text}");
    // a site with room is not marked
    assert!(!crate::call_targets_more(&img, site + 1));
}
